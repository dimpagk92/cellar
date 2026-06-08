//! IPC client. Connects to a server over UDS or any duplex stream, sends
//! typed RPC requests, and surfaces typed responses and subscription frames.
//!
//! The client implementation is intentionally minimal — one in-flight request
//! at a time, single subscription channel per connection. Tauri / CLI / test
//! callers wrap this in higher-level abstractions; the daemon side never
//! consumes a `Client`.

use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::codec::{read_message, write_message, Message};
use crate::envelope::{JsonRpcRequest, RequestId};
use crate::error::{IpcError, IpcResult};

/// IPC client. Holds a connection plus the bookkeeping that maps incoming
/// responses back to in-flight requests and dispatches notifications to
/// subscribers.
pub struct Client {
    writer: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    pending: Arc<PendingMap>,
    next_id: AtomicI64,
    // The reader task is aborted in Client's Drop.
    reader_task: tokio::task::JoinHandle<()>,
}

impl Drop for Client {
    fn drop(&mut self) {
        // Abort the reader task on drop so it doesn't outlive the client
        // (the OS would clean up the socket, but the spawned task would
        // otherwise stay attached to the runtime).
        self.reader_task.abort();
    }
}

type PendingMap = Mutex<std::collections::HashMap<String, oneshot::Sender<RpcEnvelope>>>;

type RpcResult = Result<Value, crate::envelope::JsonRpcError>;

/// What the reader task hands back to the waiting `call_inner`: the
/// JSON-RPC result/error plus the daemon-echoed `trace_id` (when present).
type RpcEnvelope = (RpcResult, Option<String>);

/// A notification (subscription frame, ping, etc.) the server pushed without
/// being asked.
#[derive(Debug, Clone)]
pub struct NotificationMessage {
    /// Method name from the JSON-RPC notification (e.g., `"events.frame"`).
    pub method: String,
    /// Notification params; for subscription frames this deserialises into
    /// a [`StreamFrame`].
    pub params: Value,
    /// Optional correlation token from the originating subscribe request.
    /// For subscription frames this echoes whatever `trace_id` the
    /// `*.subscribe` call carried; for ad-hoc server-initiated
    /// notifications (`system.ping`) the field is `None`.
    pub trace_id: Option<String>,
}

impl Client {
    /// Connect to a UDS server.
    pub async fn connect_unix(
        path: impl AsRef<Path>,
    ) -> IpcResult<(Self, mpsc::Receiver<NotificationMessage>)> {
        let stream = UnixStream::connect(path).await.map_err(IpcError::Io)?;
        Self::from_stream(stream).await
    }

    /// Build a client on top of any bidirectional stream (used by tests with
    /// `tokio::io::duplex`).
    pub async fn from_stream<S>(stream: S) -> IpcResult<(Self, mpsc::Receiver<NotificationMessage>)>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (read_half, write_half) = tokio::io::split(stream);
        let writer: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>> =
            Arc::new(Mutex::new(Box::new(write_half)));
        let pending: Arc<PendingMap> = Arc::new(Mutex::new(Default::default()));
        let (notif_tx, notif_rx) = mpsc::channel(256);
        let pending_cl = Arc::clone(&pending);
        let notif_tx_cl = notif_tx.clone();
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            loop {
                match read_message(&mut reader).await {
                    Ok(Some(Message::Response(resp))) => {
                        let id_key = resp.id.as_ref().map(|id| id.to_str()).unwrap_or_default();
                        if let Some(tx) = pending_cl.lock().await.remove(&id_key) {
                            let trace_id = resp.trace_id.clone();
                            let payload = if let Some(err) = resp.error {
                                Err(err)
                            } else {
                                Ok(resp.result.unwrap_or(Value::Null))
                            };
                            let _ = tx.send((payload, trace_id));
                        } else {
                            tracing::debug!(id = id_key, "orphan response ignored");
                        }
                    }
                    Ok(Some(Message::Request(req))) => {
                        // Server-initiated notifications (subscription frames,
                        // ping, etc.).
                        if req.is_notification() {
                            let n = NotificationMessage {
                                method: req.method,
                                params: req.params.unwrap_or(Value::Null),
                                trace_id: req.trace_id,
                            };
                            if notif_tx_cl.send(n).await.is_err() {
                                break;
                            }
                        } else {
                            tracing::debug!(
                                method = req.method,
                                "ignoring server-initiated request"
                            );
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!(error = %e, "client read error; closing");
                        break;
                    }
                }
            }
            // Signal any waiters that the connection is gone.
            let mut map = pending_cl.lock().await;
            for (_id, tx) in map.drain() {
                let _ = tx.send((
                    Err(crate::envelope::JsonRpcError {
                        code: -32603,
                        message: "connection closed".into(),
                        data: None,
                    }),
                    None,
                ));
            }
        });

        // notif_tx is dropped here; the reader task's clone keeps the
        // channel alive until either the reader exits or the caller drops
        // the receiver.
        drop(notif_tx);

        Ok((
            Self {
                writer,
                pending,
                next_id: AtomicI64::new(1),
                reader_task,
            },
            notif_rx,
        ))
    }

    /// Make an RPC call with typed params and typed response. The daemon
    /// mints a fresh trace_id server-side for correlation; use
    /// [`Self::call_with_trace`] to supply one (e.g., from the Tauri
    /// command's UUID v7 so the UI action and daemon log lines share an
    /// id).
    pub async fn call<P, R>(&self, method: &str, params: P) -> IpcResult<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        self.call_inner(method, params, None).await.map(|(r, _)| r)
    }

    /// Make an RPC call with typed params, typed response, and an
    /// explicit `trace_id` to propagate through the daemon's structured
    /// logs and any subscription frames produced by this call. Returns
    /// the typed result alongside the `trace_id` the daemon echoed back
    /// (always the one supplied here, but returned for completeness so
    /// callers can use the same code path for both call variants).
    pub async fn call_with_trace<P, R>(
        &self,
        method: &str,
        params: P,
        trace_id: impl Into<String>,
    ) -> IpcResult<(R, Option<String>)>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        self.call_inner(method, params, Some(trace_id.into())).await
    }

    /// Shared call body. Returns the parsed result alongside the
    /// daemon-echoed `trace_id` (the daemon always echoes a trace_id —
    /// the client's when supplied, a freshly minted one otherwise — so
    /// callers that didn't pass one can still discover what the daemon
    /// stamped on the request).
    async fn call_inner<P, R>(
        &self,
        method: &str,
        params: P,
        trace_id: Option<String>,
    ) -> IpcResult<(R, Option<String>)>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = RequestId::Num(self.next_id.fetch_add(1, Ordering::Relaxed));
        let id_key = id.to_str();
        let params_v = serde_json::to_value(&params)
            .map_err(|e| IpcError::Codec(format!("serialize params: {e}")))?;
        let req = match trace_id {
            Some(t) => JsonRpcRequest::new_with_trace(id.clone(), method, params_v, t),
            None => JsonRpcRequest::new(id.clone(), method, params_v),
        };

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id_key.clone(), tx);

        {
            let mut w = self.writer.lock().await;
            if let Err(e) = write_message(&mut **w, &Message::Request(req)).await {
                // Drain the slot we registered.
                let _ = self.pending.lock().await.remove(&id_key);
                return Err(e);
            }
        }

        let (resp, echoed_trace_id) = rx.await.map_err(|_| IpcError::ConnectionClosed)?;
        match resp {
            Ok(v) => serde_json::from_value(v)
                .map(|r| (r, echoed_trace_id))
                .map_err(|e| IpcError::Codec(format!("deserialize result: {e}"))),
            Err(err) => Err(map_jsonrpc_error(err)),
        }
    }

    /// Send a notification (no response expected). The server's spec uses
    /// these for `system.pong` and `system.ping`.
    pub async fn notify<P>(&self, method: &str, params: P) -> IpcResult<()>
    where
        P: Serialize,
    {
        let params_v = serde_json::to_value(&params)
            .map_err(|e| IpcError::Codec(format!("serialize params: {e}")))?;
        let n = JsonRpcRequest::notification(method, params_v);
        let mut w = self.writer.lock().await;
        write_message(&mut **w, &Message::Request(n)).await
    }
}

fn map_jsonrpc_error(err: crate::envelope::JsonRpcError) -> IpcError {
    match err.code {
        -32700 => IpcError::Parse(err.message),
        -32600 => IpcError::InvalidRequest(err.message),
        -32601 => IpcError::MethodNotFound(err.message),
        -32602 => IpcError::InvalidParams(err.message),
        -32603 => IpcError::Internal(err.message),
        -32000 => IpcError::ShuttingDown,
        -32001 => IpcError::UnsupportedProtocolVersion(
            err.data
                .as_ref()
                .and_then(|d| d.get("client_supports"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        -32002 => IpcError::NotAuthorized,
        -32003 => IpcError::RateLimited,
        -32004 => IpcError::RuleNotFound(extract_id(&err.data, "rule_id")),
        -32005 => IpcError::WatchlistNotFound(extract_id(&err.data, "watchlist_name")),
        -32006 => IpcError::WebhookNotFound(extract_id(&err.data, "webhook_id")),
        -32007 => IpcError::SessionNotFound(extract_id(&err.data, "session_id")),
        -32008 => IpcError::ConfirmationNotFound(extract_id(&err.data, "confirmation_id")),
        -32009 => IpcError::ConfirmationAlreadyResolved(extract_id(&err.data, "confirmation_id")),
        -32010 => IpcError::ValidationFailed(err.message),
        -32011 => IpcError::LlmProviderError(err.message),
        -32012 => IpcError::ExternalMcpDisabled,
        -32013 => IpcError::TauriNotAttached,
        -32099 => {
            // NotImplemented wants a &'static str; box-leak the method
            // name so the error type stays stable. Bounded by the number of
            // methods so this never grows unbounded.
            let m = err
                .data
                .as_ref()
                .and_then(|d| d.get("method"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            IpcError::NotImplemented(Box::leak(m.to_string().into_boxed_str()))
        }
        _ => IpcError::Internal(format!("code {}: {}", err.code, err.message)),
    }
}

fn extract_id(data: &Option<Value>, key: &str) -> String {
    data.as_ref()
        .and_then(|d| d.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}
