//! IPC server. Accepts UDS connections, dispatches messages to a
//! [`Handler`], serialises typed results back as JSON-RPC responses,
//! and forwards subscription frames as notifications.
//!
//! The server is intentionally framework-light: it owns the framing
//! ([`crate::codec`]), the dispatch table (this module), and the per-connection
//! frame-fanout machinery. It does not implement any RPC business logic —
//! that's the handler's job.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::codec::{read_message, write_message, Message};
use crate::envelope::{JsonRpcRequest, JsonRpcResponse, RequestId};
use crate::error::{IpcError, IpcResult};
use crate::handler::{FrameSink, Handler};
use crate::subscription::StreamFrame;

/// The server. Owns a [`UnixListener`] and an [`Arc<Handler>`]. Drop the
/// server to stop accepting new connections.
///
/// Existing connections are spawned tasks that run independently — the
/// server's drop doesn't disconnect them.
pub struct Server<H: Handler> {
    listener: UnixListener,
    handler: Arc<H>,
    socket_path: PathBuf,
}

impl<H: Handler> Server<H> {
    /// Bind to a UDS socket path. Removes any stale socket at that path
    /// first, then sets mode `0600`.
    pub async fn bind(path: impl AsRef<Path>, handler: H) -> IpcResult<Self> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            // RFC §2.1: clean up stale sockets at startup.
            tokio::fs::remove_file(&path).await.map_err(IpcError::Io)?;
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(IpcError::Io)?;
        }
        let listener = UnixListener::bind(&path).map_err(IpcError::Io)?;
        // Mode 0600 — owner only. The RFC's only auth mechanism.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = tokio::fs::metadata(&path)
                .await
                .map_err(IpcError::Io)?
                .permissions();
            perms.set_mode(0o600);
            tokio::fs::set_permissions(&path, perms)
                .await
                .map_err(IpcError::Io)?;
        }
        Ok(Self {
            listener,
            handler: Arc::new(handler),
            socket_path: path,
        })
    }

    /// The path the server is bound to. Owns the socket — cleaned up on
    /// [`Server::shutdown`].
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Run the accept loop until the listener is dropped or the future is
    /// cancelled. Each accepted connection is spawned into the supplied
    /// [`JoinSet`] so the caller can await graceful shutdown.
    pub async fn run(self, tasks: &mut JoinSet<()>) -> IpcResult<()> {
        loop {
            let (stream, _addr) = match self.listener.accept().await {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(error = %e, "ipc accept failed");
                    continue;
                }
            };
            let handler = Arc::clone(&self.handler);
            tasks.spawn(async move {
                if let Err(e) = serve_connection(stream, handler).await {
                    tracing::debug!(error = %e, "ipc connection ended");
                }
            });
        }
    }

    /// Stop accepting new connections and remove the socket file. In-flight
    /// connections are left to drain (they're not owned by `self`).
    pub async fn shutdown(self) -> IpcResult<()> {
        drop(self.listener);
        if self.socket_path.exists() {
            tokio::fs::remove_file(&self.socket_path)
                .await
                .map_err(IpcError::Io)?;
        }
        Ok(())
    }
}

/// Drive one bidirectional connection: receive requests, dispatch, write
/// responses, forward subscription frames. Returns when the client
/// disconnects or any unrecoverable error occurs.
///
/// Public so external tests / in-memory pipes (`tokio::io::duplex`) can
/// use the same dispatch logic without going through a UDS socket.
pub async fn serve_connection<S, H>(stream: S, handler: Arc<H>) -> IpcResult<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
    H: Handler,
{
    // Split bidirectional stream so the reader and the frame-forwarder
    // can both write back independently.
    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    // Mutex-protect the write side so request responses and frame
    // notifications can both write without interleaving partial lines.
    let writer = Arc::new(Mutex::new(write_half));

    handler.on_connect().await;

    // Per-connection frame channel. Subscriptions push frames into this
    // sender; the forwarder task pulls and writes to the wire.
    let (frame_tx, mut frame_rx) = mpsc::channel::<StreamFrame>(256);

    // Spawn the frame forwarder.
    let writer_cl = Arc::clone(&writer);
    let forwarder = tokio::spawn(async move {
        while let Some(frame) = frame_rx.recv().await {
            let method = frame_notification_method(&frame);
            let params = match serde_json::to_value(&frame) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "frame serialize failed");
                    continue;
                }
            };
            let notif = JsonRpcRequest::notification(method, params);
            let mut w = writer_cl.lock().await;
            if let Err(e) = write_message(&mut *w, &Message::Request(notif)).await {
                tracing::debug!(error = %e, "frame write failed; closing");
                break;
            }
        }
    });

    // Main read-dispatch loop.
    loop {
        let msg = match read_message(&mut reader).await {
            Ok(Some(Message::Request(req))) => req,
            Ok(Some(Message::Response(_))) => {
                // Client sent a response back to the server. Spec doesn't
                // forbid it but the daemon doesn't initiate requests in v1.
                tracing::warn!("ignoring unexpected response from client");
                continue;
            }
            Ok(None) => break, // clean EOF
            Err(IpcError::Codec(msg)) => {
                let resp = JsonRpcResponse::from_ipc_error(None, &IpcError::Parse(msg.clone()));
                let mut w = writer.lock().await;
                let _ = write_message(&mut *w, &Message::Response(resp)).await;
                continue;
            }
            Err(e) => {
                tracing::debug!(error = %e, "read error; closing");
                break;
            }
        };

        let id = msg.id.clone();
        let is_notification = msg.is_notification();
        let response = dispatch(&handler, msg, frame_tx.clone()).await;

        if is_notification {
            // Notifications get no response, even on error. Log and move on.
            if let Err(e) = response {
                tracing::debug!(error = %e, "notification handler errored");
            }
            continue;
        }

        let id_for_response = id.unwrap_or(RequestId::Num(0));
        let resp = match response {
            Ok(value) => JsonRpcResponse::ok(id_for_response, value),
            Err(e) => JsonRpcResponse::from_ipc_error(Some(id_for_response), &e),
        };
        let mut w = writer.lock().await;
        if let Err(e) = write_message(&mut *w, &Message::Response(resp)).await {
            tracing::debug!(error = %e, "response write failed; closing");
            break;
        }
    }

    drop(frame_tx);
    let _ = forwarder.await;
    handler.on_disconnect().await;
    Ok(())
}

fn frame_notification_method(frame: &StreamFrame) -> String {
    use crate::subscription::StreamPayload as P;
    match &frame.payload {
        P::Event { .. } => "events.frame",
        P::Fire { .. } => "fires.frame",
        P::AgentAction { .. } => "agent_actions.frame",
        P::Confirmation { .. } => "confirmation.frame",
        P::Token { .. }
        | P::ToolCallAttempt { .. }
        | P::ToolCallAwaitingConfirmation { .. }
        | P::ToolCallResult { .. }
        | P::MessageComplete { .. }
        | P::RequestDone { .. }
        | P::Error { .. } => "agent.chat.frame",
        P::Health { .. } => "daemon.health.frame",
        P::Gap { .. } => "subscription.gap",
    }
    .to_string()
}

/// Dispatch one request to the typed handler.
async fn dispatch<H: Handler>(
    handler: &Arc<H>,
    req: JsonRpcRequest,
    sink: FrameSink,
) -> IpcResult<Value> {
    let params = req
        .params
        .unwrap_or_else(|| Value::Object(Default::default()));
    macro_rules! call {
        ($method:ident) => {{
            let p = parse(params)?;
            let r = handler.$method(p).await?;
            let v: Value = serde_json::to_value(&r).map_err(serde_to_internal)?;
            Ok(v)
        }};
        ($method:ident, no_params) => {{
            let r = handler.$method().await?;
            let v: Value = serde_json::to_value(&r).map_err(serde_to_internal)?;
            Ok(v)
        }};
        ($method:ident, with_sink) => {{
            let p = parse(params)?;
            let r = handler.$method(p, sink).await?;
            let v: Value = serde_json::to_value(&r).map_err(serde_to_internal)?;
            Ok(v)
        }};
        ($method:ident, sink_only) => {{
            let r = handler.$method(sink).await?;
            let v: Value = serde_json::to_value(&r).map_err(serde_to_internal)?;
            Ok(v)
        }};
    }

    match req.method.as_str() {
        // system
        "system.hello" => call!(system_hello),
        "system.shutdown" => call!(system_shutdown),
        "system.pong" => call!(system_pong, no_params),

        // daemon
        "daemon.status" => call!(daemon_status, no_params),
        "daemon.health.subscribe" => call!(daemon_health_subscribe, sink_only),
        "daemon.health.unsubscribe" => call!(daemon_health_unsubscribe),

        // rules
        "rules.list" => call!(rules_list),
        "rules.get" => call!(rules_get),
        "rules.add" => call!(rules_add),
        "rules.update" => call!(rules_update),
        "rules.remove" => call!(rules_remove),
        "rules.pause" => call!(rules_pause),
        "rules.resume" => call!(rules_resume),
        "rules.compile" => call!(rules_compile),
        "rules.test" => call!(rules_test),

        // watchlists
        "watchlists.list" => call!(watchlists_list),
        "watchlists.get" => call!(watchlists_get),
        "watchlists.set" => call!(watchlists_set),
        "watchlists.add_item" => call!(watchlists_add_item),
        "watchlists.remove_item" => call!(watchlists_remove_item),
        "watchlists.remove" => call!(watchlists_remove),

        // webhooks
        "webhooks.list" => call!(webhooks_list),
        "webhooks.add" => call!(webhooks_add),
        "webhooks.remove" => call!(webhooks_remove),
        "webhooks.test" => call!(webhooks_test),

        // events
        "events.recent" => call!(events_recent),
        "events.subscribe" => call!(events_subscribe, with_sink),
        "events.unsubscribe" => call!(events_unsubscribe),

        // fires
        "fires.recent" => call!(fires_recent),
        "fires.subscribe" => call!(fires_subscribe, with_sink),
        "fires.unsubscribe" => call!(fires_unsubscribe),

        // agent_actions
        "agent_actions.recent" => call!(agent_actions_recent),
        "agent_actions.subscribe" => call!(agent_actions_subscribe, with_sink),
        "agent_actions.unsubscribe" => call!(agent_actions_unsubscribe),

        // confirmation
        "confirmation.list_pending" => call!(confirmation_list_pending),
        "confirmation.subscribe" => call!(confirmation_subscribe, with_sink),
        "confirmation.unsubscribe" => call!(confirmation_unsubscribe),
        "confirmation.resolve" => call!(confirmation_resolve),

        // agent
        "agent.sessions.list" => call!(agent_sessions_list),
        "agent.sessions.create" => call!(agent_sessions_create),
        "agent.sessions.get" => call!(agent_sessions_get),
        "agent.sessions.rename" => call!(agent_sessions_rename),
        "agent.sessions.delete" => call!(agent_sessions_delete),
        "agent.message" => call!(agent_message),
        "agent.chat.subscribe" => call!(agent_chat_subscribe, with_sink),
        "agent.chat.unsubscribe" => call!(agent_chat_unsubscribe),
        "agent.interrupt" => call!(agent_interrupt),

        // settings
        "settings.get" => call!(settings_get),
        "settings.set" => call!(settings_set),

        other => Err(IpcError::MethodNotFound(other.to_string())),
    }
}

fn parse<T: DeserializeOwned>(value: Value) -> IpcResult<T> {
    serde_json::from_value(value).map_err(|e| IpcError::InvalidParams(e.to_string()))
}

fn serde_to_internal(e: serde_json::Error) -> IpcError {
    IpcError::Internal(format!("serialize result: {e}"))
}
