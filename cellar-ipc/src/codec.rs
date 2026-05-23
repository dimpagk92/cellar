//! Line-delimited JSON framing.
//!
//! Each message on the socket is one JSON-RPC envelope ([`JsonRpcRequest`] or
//! [`JsonRpcResponse`]) terminated by `\n`. This module provides
//! [`read_message`] / [`write_message`] over any tokio `AsyncRead` /
//! `AsyncWrite`, so the same code paths work over UDS, `tokio::io::duplex`
//! (for tests), or any future transport.
//!
//! [`JsonRpcRequest`]: crate::JsonRpcRequest
//! [`JsonRpcResponse`]: crate::JsonRpcResponse

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::envelope::{JsonRpcRequest, JsonRpcResponse};
use crate::error::{IpcError, IpcResult};

/// A message that can appear on the wire — either a request/notification or
/// a response. Used by the codec to dispatch incoming bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Message {
    /// A request or notification (has `method`; may or may not have `id`).
    Request(JsonRpcRequest),
    /// A response (has `result` or `error`; carries the request's `id`).
    Response(JsonRpcResponse),
}

/// Read one line-delimited JSON message from the reader.
///
/// Returns `Ok(None)` on clean EOF; `Err(IpcError::Codec(_))` if the line
/// doesn't parse as JSON-RPC. Long lines are bounded by the BufReader's
/// internal buffer — for v1 we accept any size up to 16 MiB per message
/// (caller can configure via [`read_message_with_limit`]).
pub async fn read_message<R>(reader: &mut BufReader<R>) -> IpcResult<Option<Message>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    read_message_with_limit(reader, 16 * 1024 * 1024).await
}

/// As [`read_message`], with an explicit byte limit.
pub async fn read_message_with_limit<R>(
    reader: &mut BufReader<R>,
    max_bytes: usize,
) -> IpcResult<Option<Message>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = String::new();
    let mut total = 0;
    // Read until newline; cap total bytes for safety.
    loop {
        let read = reader.read_line(&mut buf).await.map_err(IpcError::Io)?;
        if read == 0 {
            // EOF.
            return if buf.is_empty() {
                Ok(None)
            } else {
                Err(IpcError::Codec("unexpected EOF mid-line".into()))
            };
        }
        total += read;
        if buf.ends_with('\n') {
            break;
        }
        if total >= max_bytes {
            return Err(IpcError::Codec(format!(
                "message exceeds limit {max_bytes}"
            )));
        }
    }
    let line = buf.trim_end_matches(['\n', '\r']);
    if line.is_empty() {
        // Skip blank lines.
        return Box::pin(read_message_with_limit(reader, max_bytes)).await;
    }
    let msg: Message = serde_json::from_str(line)
        .map_err(|e| IpcError::Codec(format!("parse: {e} (line: {line:?})")))?;
    Ok(Some(msg))
}

/// Write a message followed by a single `\n`. Flushes the writer.
pub async fn write_message<W>(writer: &mut W, msg: &Message) -> IpcResult<()>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    let mut buf =
        serde_json::to_vec(msg).map_err(|e| IpcError::Codec(format!("serialize: {e}")))?;
    buf.push(b'\n');
    writer.write_all(&buf).await.map_err(IpcError::Io)?;
    writer.flush().await.map_err(IpcError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trip_request_through_duplex() {
        let (client, server) = duplex(64 * 1024);
        let (mut cr, mut cw) = tokio::io::split(client);
        let (mut sr, mut sw) = tokio::io::split(server);

        let req = JsonRpcRequest::new(1_i64, "rules.list", json!({}));
        let out = Message::Request(req.clone());

        // Client writes; server reads.
        write_message(&mut cw, &out).await.unwrap();
        let mut reader = BufReader::new(&mut sr);
        let got = read_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(got, out);

        // Server writes a response; client reads.
        let resp = JsonRpcResponse::ok(crate::RequestId::Num(1), json!({"ok": true}));
        write_message(&mut sw, &Message::Response(resp.clone()))
            .await
            .unwrap();
        let mut reader = BufReader::new(&mut cr);
        let got = read_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(got, Message::Response(resp));
    }

    #[tokio::test]
    async fn read_returns_none_on_clean_eof() {
        let (client, server) = duplex(64);
        drop(client);
        let (mut sr, _sw) = tokio::io::split(server);
        let mut reader = BufReader::new(&mut sr);
        let got = read_message(&mut reader).await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn malformed_line_returns_codec_error() {
        let (mut client, server) = duplex(64);
        client.write_all(b"not json\n").await.unwrap();
        client.flush().await.unwrap();
        drop(client);
        let (mut sr, _sw) = tokio::io::split(server);
        let mut reader = BufReader::new(&mut sr);
        let err = read_message(&mut reader).await.unwrap_err();
        assert!(matches!(err, IpcError::Codec(_)));
    }

    #[tokio::test]
    async fn blank_lines_are_skipped() {
        let (mut client, server) = duplex(1024);
        client.write_all(b"\n\n").await.unwrap();
        let req = JsonRpcRequest::new(1_i64, "x", json!({}));
        let line = serde_json::to_string(&Message::Request(req.clone())).unwrap();
        client.write_all(line.as_bytes()).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client.flush().await.unwrap();
        drop(client);
        let (mut sr, _sw) = tokio::io::split(server);
        let mut reader = BufReader::new(&mut sr);
        let got = read_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(got, Message::Request(req));
    }
}
