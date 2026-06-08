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

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinSet;
use tracing::Instrument;
use uuid::Uuid;

use crate::codec::{read_message, write_message, Message};
use crate::envelope::{JsonRpcRequest, JsonRpcResponse, RequestId};
use crate::error::{IpcError, IpcResult};
use crate::handler::{FrameSink, Handler};
use crate::subscription::StreamFrame;

/// Mint a fresh server-side `trace_id`. Used when the client omits the
/// field so every request has a correlation token. UUID v7 is monotonic
/// per millisecond, which keeps daemon log lines naturally sorted.
fn mint_trace_id() -> String {
    format!("srv-{}", Uuid::now_v7())
}

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
        Self::bind_with_arc(path, Arc::new(handler)).await
    }

    /// As [`Self::bind`], but takes an existing `Arc<H>`. Useful when the
    /// daemon already holds the handler behind an Arc (e.g., the gateway
    /// or other subsystems share access to it) and would otherwise be
    /// forced to either clone or unwrap.
    pub async fn bind_with_arc(path: impl AsRef<Path>, handler: Arc<H>) -> IpcResult<Self> {
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
            handler,
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
    //
    // RFC §6 backpressure: the channel is bounded so that slow clients
    // can't grow unbounded queues inside the daemon. When the channel
    // fills, the standard subscription forwarders (`events`, `fires`,
    // `agent_actions`, `daemon.health`) drop frames and emit a single
    // `subscription.gap` notification once the client catches up. The
    // critical forwarders (`confirmation`, `agent.chat`) instead notify
    // `close_hint`, which this task observes via `tokio::select!` and
    // tears the connection down to force the client to reconnect.
    let (frame_tx, mut frame_rx) = mpsc::channel::<StreamFrame>(256);
    let close_hint = Arc::new(Notify::new());
    let sink = FrameSink::new(frame_tx, Arc::clone(&close_hint));

    // Spawn the frame forwarder.
    let writer_cl = Arc::clone(&writer);
    let forwarder = tokio::spawn(async move {
        while let Some(frame) = frame_rx.recv().await {
            let method = frame_notification_method(&frame);
            // Pull the trace_id off the frame so the JSON-RPC notification
            // envelope echoes it at the top level too. Subscribers that
            // only look at the envelope's `trace_id` (e.g. the Tauri
            // network sniffer) can correlate without parsing the params.
            let trace_id = frame.trace_id.clone();
            let params = match serde_json::to_value(&frame) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "frame serialize failed");
                    continue;
                }
            };
            let notif = match trace_id {
                Some(t) => JsonRpcRequest::notification_with_trace(method, params, t),
                None => JsonRpcRequest::notification(method, params),
            };
            let mut w = writer_cl.lock().await;
            if let Err(e) = write_message(&mut *w, &Message::Request(notif)).await {
                tracing::debug!(error = %e, "frame write failed; closing");
                break;
            }
        }
    });

    // Main read-dispatch loop. We `select!` on the read future and the
    // `close_hint` notification so a critical-subscription forwarder
    // overflow can pull the connection down without waiting for the next
    // client message.
    let close_hint_for_loop = Arc::clone(&close_hint);
    loop {
        let msg = tokio::select! {
            biased;
            _ = close_hint_for_loop.notified() => {
                tracing::debug!(
                    "ipc connection closing — critical subscription forwarder overflowed"
                );
                break;
            }
            read = read_message(&mut reader) => {
                match read {
                    Ok(Some(Message::Request(req))) => req,
                    Ok(Some(Message::Response(_))) => {
                        // Client sent a response back to the server. Spec doesn't
                        // forbid it but the daemon doesn't initiate requests in v1.
                        tracing::warn!("ignoring unexpected response from client");
                        continue;
                    }
                    Ok(None) => break, // clean EOF
                    Err(IpcError::Codec(msg)) => {
                        // Parse errors mint a fresh trace_id — the client
                        // payload was unparseable so we couldn't extract one.
                        let trace_id = mint_trace_id();
                        let resp = JsonRpcResponse::from_ipc_error_with_trace(
                            None,
                            &IpcError::Parse(msg.clone()),
                            trace_id,
                        );
                        let mut w = writer.lock().await;
                        let _ = write_message(&mut *w, &Message::Response(resp)).await;
                        continue;
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "read error; closing");
                        break;
                    }
                }
            }
        };

        let id = msg.id.clone();
        let is_notification = msg.is_notification();
        // Resolve the correlation token: prefer the client-supplied
        // trace_id; otherwise mint one so every request has a value the
        // daemon's structured logs can carry.
        let trace_id = msg.trace_id.clone().unwrap_or_else(mint_trace_id);
        let method = msg.method.clone();
        // Stamp the sink so subscription forwarders inherit the same
        // trace_id on every frame they push.
        let scoped_sink = sink.clone().with_trace_id(Some(trace_id.clone()));
        // Build the per-request span. Every `tracing::info!` /
        // `warn!` / `error!` inside the handler (and any child spans
        // the handler enters) will carry these fields.
        let span = tracing::info_span!(
            "ipc.request",
            trace_id = %trace_id,
            method = %method,
            request_id = %id.as_ref().map(RequestId::to_str).unwrap_or_default(),
        );
        let response = dispatch(&handler, msg, scoped_sink).instrument(span).await;

        if is_notification {
            // Notifications get no response, even on error. Log and move on.
            if let Err(e) = response {
                tracing::debug!(
                    trace_id = %trace_id,
                    error = %e,
                    "notification handler errored"
                );
            }
            continue;
        }

        let id_for_response = id.unwrap_or(RequestId::Num(0));
        let resp = match response {
            Ok(value) => JsonRpcResponse::ok_with_trace(id_for_response, value, trace_id.clone()),
            Err(e) => JsonRpcResponse::from_ipc_error_with_trace(
                Some(id_for_response),
                &e,
                trace_id.clone(),
            ),
        };
        let mut w = writer.lock().await;
        if let Err(e) = write_message(&mut *w, &Message::Response(resp)).await {
            tracing::debug!(
                trace_id = %trace_id,
                error = %e,
                "response write failed; closing"
            );
            break;
        }
    }

    // Drop the sink so all outstanding subscription forwarders see the
    // channel close and exit cleanly. The frame-forwarder task then
    // drains its mpsc and exits when the receiver returns `None`.
    //
    // We bound the await: if a critical subscription forwarder
    // requested close because its sink was full, the frame forwarder is
    // by definition blocked in `write_message` against a saturated
    // downstream. Awaiting indefinitely would deadlock the serve task,
    // so after a short grace period we abort the forwarder. The
    // connection writer half is dropped when this function returns,
    // releasing the downstream waiters.
    drop(sink);
    let abort_handle = forwarder.abort_handle();
    if tokio::time::timeout(std::time::Duration::from_millis(50), forwarder)
        .await
        .is_err()
    {
        tracing::debug!("frame forwarder still busy at connection-close; aborting");
        abort_handle.abort();
    }
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
    // Routing is generated by `#[ipc_dispatch]` on the `Handler` trait
    // (see `crate::handler::dispatch`), so every handler method has a route.
    crate::handler::dispatch(handler, &req.method, params, sink).await
}
