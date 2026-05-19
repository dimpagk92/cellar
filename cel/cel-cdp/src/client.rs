//! CDP WebSocket Client
//!
//! Minimal Chrome DevTools Protocol client for page content extraction.

use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    #[error("WebSocket connection failed: {0}")]
    ConnectionFailed(String),
    #[error("CDP command failed: {0}")]
    CommandFailed(String),
    #[error("Timeout waiting for CDP response")]
    Timeout,
    #[error("Invalid response: {0}")]
    InvalidResponse(String),
}

/// Minimal CDP client — connects via WebSocket and sends commands.
pub struct CdpClient {
    ws: Mutex<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    /// WebSocket URL captured at construction so the resilient
    /// retry path can rebuild the WebSocket if Chrome drops the
    /// underlying connection mid-run. Wrapped in a Mutex because
    /// reconnection sometimes has to fall back to a freshly-
    /// discovered target (Chrome destroyed the original page) —
    /// the URL the next reconnect should try is updated after a
    /// successful re-discovery so we don't keep banging on a dead
    /// target. Empty when the URL isn't known (legacy callers
    /// that bypass `connect`).
    ws_url: std::sync::Mutex<String>,
    /// Port pinned to this client at construction (parsed out of
    /// `ws_url`). When set, the resilient reconnect re-discovery loop
    /// REFUSES to connect to any target on a different port — so a
    /// dead CEL-dedicated browser doesn't get silently replaced by
    /// the user's real Chrome window mid-eval.
    ///
    /// The 2026-05-13 trial caught this in `recover_from_stale_state`:
    /// the bound CDP socket dropped, reconnect's discovery loop
    /// grabbed the user's real Chrome (with Notion/Timberhub tabs),
    /// and the agent's perception silently shifted onto the wrong
    /// browser. After this pin: reconnect to a non-matching port
    /// fails cleanly rather than hijacking an arbitrary Chrome.
    ///
    /// `None` for legacy callers that constructed via the older code
    /// paths or directly handed in a WebSocket — preserves back-compat.
    pinned_port: Option<u16>,
    next_id: AtomicU64,
}

impl CdpClient {
    /// Connect to a CDP WebSocket URL.
    pub async fn connect(ws_url: &str) -> Result<Self, CdpError> {
        let (ws, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| CdpError::ConnectionFailed(e.to_string()))?;

        Ok(Self {
            ws: Mutex::new(ws),
            ws_url: std::sync::Mutex::new(ws_url.to_string()),
            pinned_port: parse_ws_port(ws_url),
            next_id: AtomicU64::new(1),
        })
    }

    /// Send a CDP command and wait for the result.
    pub async fn send_command(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CdpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
        });

        let mut ws = self.ws.lock().await;

        ws.send(Message::Text(msg.to_string().into()))
            .await
            .map_err(|e| CdpError::CommandFailed(e.to_string()))?;

        // Wait for the response with matching id (timeout after 5s)
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(CdpError::Timeout);
            }

            match tokio::time::timeout(remaining, ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(response) = serde_json::from_str::<serde_json::Value>(&text) {
                        if response.get("id").and_then(|v| v.as_u64()) == Some(id) {
                            if let Some(error) = response.get("error") {
                                return Err(CdpError::CommandFailed(error.to_string()));
                            }
                            return Ok(response
                                .get("result")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null));
                        }
                        // Not our response — it's an event, skip it
                    }
                }
                Ok(Some(Ok(_))) => continue, // Binary or other message types
                Ok(Some(Err(e))) => return Err(CdpError::CommandFailed(e.to_string())),
                Ok(None) => return Err(CdpError::ConnectionFailed("WebSocket closed".into())),
                Err(_) => return Err(CdpError::Timeout),
            }
        }
    }

    /// Get the full DOM document as a tree.
    pub async fn get_document(&self) -> Result<serde_json::Value, CdpError> {
        self.send_command("DOM.getDocument", serde_json::json!({ "depth": -1 }))
            .await
    }

    /// Get the outer HTML of a node.
    pub async fn get_outer_html(&self, node_id: i64) -> Result<String, CdpError> {
        let result = self
            .send_command("DOM.getOuterHTML", serde_json::json!({ "nodeId": node_id }))
            .await?;
        result
            .get("outerHTML")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| CdpError::InvalidResponse("No outerHTML in response".into()))
    }

    /// Execute JavaScript in the page and return the result.
    pub async fn evaluate(&self, expression: &str) -> Result<serde_json::Value, CdpError> {
        let result = self
            .send_command(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": expression,
                    "returnByValue": true,
                }),
            )
            .await?;
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// Enable network event tracking.
    pub async fn enable_network(&self) -> Result<(), CdpError> {
        self.send_command("Network.enable", serde_json::json!({}))
            .await?;
        Ok(())
    }

    /// Get the page title.
    pub async fn get_title(&self) -> Result<String, CdpError> {
        let result = self.evaluate("document.title").await?;
        Ok(result.as_str().unwrap_or("").to_string())
    }

    /// Get the page URL.
    pub async fn get_url(&self) -> Result<String, CdpError> {
        let result = self.evaluate("window.location.href").await?;
        Ok(result.as_str().unwrap_or("").to_string())
    }

    /// Capture a screenshot of the bound page via `Page.captureScreenshot`.
    ///
    /// Returns JPEG bytes at quality 80, viewport-only (no
    /// `captureBeyondViewport`). Routing screenshots through CDP rather
    /// than the macOS display capture is what lets headless Chrome
    /// scenarios actually photograph the rendered page instead of the
    /// foreground OS window — without this, the planner sees the editor /
    /// terminal that happens to be focused and refuses with "I'm not in
    /// the browser".
    ///
    /// Quality 80 mirrors the macOS-display fallback in
    /// `CortexStepExecutor::screenshot_png` so payload size stays
    /// consistent regardless of which path produced the bytes (typical
    /// 100–300 KB; well under the Anthropic 5 MB cap).
    pub async fn capture_screenshot(&self) -> Result<Vec<u8>, CdpError> {
        let result = self
            .send_command(
                "Page.captureScreenshot",
                serde_json::json!({
                    "format": "jpeg",
                    "quality": 80,
                    "captureBeyondViewport": false,
                }),
            )
            .await?;
        let b64 = result.get("data").and_then(|v| v.as_str()).ok_or_else(|| {
            CdpError::InvalidResponse("Page.captureScreenshot missing 'data' field".into())
        })?;
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| {
                CdpError::InvalidResponse(format!("Page.captureScreenshot invalid base64: {e}"))
            })
    }

    /// Capture a DOM snapshot with paint order and computed styles.
    /// Returns the raw CDP DOMSnapshot.captureSnapshot result.
    pub async fn capture_dom_snapshot(&self) -> Result<serde_json::Value, CdpError> {
        self.send_command(
            "DOMSnapshot.captureSnapshot",
            serde_json::json!({
                "computedStyles": [
                    "z-index", "position", "display", "visibility",
                    "opacity", "cursor", "overflow"
                ],
                "includePaintOrder": true,
                "includeDOMRects": true,
            }),
        )
        .await
    }

    /// Get the full accessibility tree via CDP.
    pub async fn get_accessibility_tree(&self) -> Result<serde_json::Value, CdpError> {
        self.send_command("Accessibility.enable", serde_json::json!({}))
            .await?;
        self.send_command("Accessibility.getFullAXTree", serde_json::json!({}))
            .await
    }

    /// Dispatch a key event via CDP Input domain.
    pub async fn dispatch_key_event(&self, event_type: &str, key: &str) -> Result<(), CdpError> {
        self.send_command(
            "Input.dispatchKeyEvent",
            serde_json::json!({
                "type": event_type,
                "key": key,
            }),
        )
        .await?;
        Ok(())
    }

    /// Click at coordinates via CDP Input domain.
    /// Dispatches mousePressed + mouseReleased at the given position.
    pub async fn click_at(&self, x: f64, y: f64) -> Result<(), CdpError> {
        // mouseMoved
        self.send_command(
            "Input.dispatchMouseEvent",
            serde_json::json!({
                "type": "mouseMoved",
                "x": x,
                "y": y,
            }),
        )
        .await?;

        // mousePressed
        self.send_command(
            "Input.dispatchMouseEvent",
            serde_json::json!({
                "type": "mousePressed",
                "x": x,
                "y": y,
                "button": "left",
                "clickCount": 1,
            }),
        )
        .await?;

        // mouseReleased
        self.send_command(
            "Input.dispatchMouseEvent",
            serde_json::json!({
                "type": "mouseReleased",
                "x": x,
                "y": y,
                "button": "left",
                "clickCount": 1,
            }),
        )
        .await?;

        Ok(())
    }

    /// Insert text via CDP Input domain (IME-style insertion).
    pub async fn insert_text(&self, text: &str) -> Result<(), CdpError> {
        self.send_command(
            "Input.insertText",
            serde_json::json!({
                "text": text,
            }),
        )
        .await?;
        Ok(())
    }

    /// Get all cookies for the current page.
    /// Returns JSON array of cookie objects.
    pub async fn get_cookies(&self) -> Result<serde_json::Value, CdpError> {
        let result = self
            .send_command("Network.getCookies", serde_json::json!({}))
            .await?;
        Ok(result
            .get("cookies")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![])))
    }

    /// Get a specific cookie by name. Returns the cookie value or None.
    pub async fn get_cookie(&self, name: &str) -> Result<Option<String>, CdpError> {
        let cookies = self.get_cookies().await?;
        if let Some(arr) = cookies.as_array() {
            for cookie in arr {
                if cookie.get("name").and_then(|n| n.as_str()) == Some(name) {
                    return Ok(cookie
                        .get("value")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()));
                }
            }
        }
        Ok(None)
    }

    /// Read localStorage for a given key. Returns the value or null.
    pub async fn get_local_storage(&self, key: &str) -> Result<Option<String>, CdpError> {
        let expr = format!(
            "localStorage.getItem({})",
            serde_json::to_string(key).unwrap_or_else(|_| format!("\"{}\"", key))
        );
        let result = self.evaluate(&expr).await?;
        match result {
            serde_json::Value::String(s) => Ok(Some(s)),
            serde_json::Value::Null => Ok(None),
            _ => Ok(Some(result.to_string())),
        }
    }

    /// Read sessionStorage for a given key.
    pub async fn get_session_storage(&self, key: &str) -> Result<Option<String>, CdpError> {
        let expr = format!(
            "sessionStorage.getItem({})",
            serde_json::to_string(key).unwrap_or_else(|_| format!("\"{}\"", key))
        );
        let result = self.evaluate(&expr).await?;
        match result {
            serde_json::Value::String(s) => Ok(Some(s)),
            serde_json::Value::Null => Ok(None),
            _ => Ok(Some(result.to_string())),
        }
    }

    /// Get recent network requests via the Performance API.
    /// Returns structured HttpEvents with real HTTP data (method, URL, status, timing).
    /// This is more reliable than Network.enable events since it works without
    /// maintaining a persistent event listener.
    pub async fn get_network_requests(
        &self,
        limit: usize,
    ) -> Result<Vec<cel_network::HttpEvent>, CdpError> {
        let js = format!(
            r#"
            (() => {{
                const entries = performance.getEntriesByType('resource')
                    .filter(e => e.initiatorType === 'fetch' || e.initiatorType === 'xmlhttprequest')
                    .slice(-{})
                    .map(e => ({{
                        timestamp_ms: Math.floor(performance.timeOrigin + e.startTime),
                        method: 'GET',
                        url: e.name,
                        status_code: e.responseStatus || null,
                        content_type: null,
                        duration_ms: e.duration,
                        size_bytes: e.transferSize || null,
                        source: 'performance_api'
                    }}));
                return entries;
            }})()
        "#,
            limit
        );

        let result = self.evaluate(&js).await?;
        let events: Vec<cel_network::HttpEvent> =
            serde_json::from_value(result).unwrap_or_default();
        Ok(events)
    }

    /// Navigate to a URL.
    pub async fn navigate(&self, url: &str) -> Result<(), CdpError> {
        self.send_command("Page.enable", serde_json::json!({}))
            .await?;
        self.send_command("Page.navigate", serde_json::json!({ "url": url }))
            .await?;
        Ok(())
    }

    /// Send a CDP command with retry on timeout (up to 2 retries).
    pub async fn send_command_retry(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CdpError> {
        let mut last_err = CdpError::Timeout;
        for attempt in 0..3 {
            match self.send_command(method, params.clone()).await {
                Ok(result) => return Ok(result),
                Err(CdpError::Timeout) if attempt < 2 => {
                    tracing::debug!("CDP command {} timed out, retry {}/2", method, attempt + 1);
                    last_err = CdpError::Timeout;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err)
    }

    /// Send a CDP command, transparently reconnecting once if the
    /// underlying WebSocket has been torn down by Chrome
    /// (`closed connection` / `broken pipe` / `WebSocket closed`).
    /// Used by long-lived shared `Arc<CdpClient>` instances that
    /// need to survive Chrome dropping a single socket while the
    /// browser process itself remains alive — without this, a
    /// transient hiccup mid-eval cascades into "every later
    /// scenario fails because the shared client is dead".
    pub async fn send_command_resilient(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CdpError> {
        match self.send_command(method, params.clone()).await {
            Ok(v) => Ok(v),
            Err(e) if is_connection_dropped(&e) => {
                tracing::warn!(
                    method = %method,
                    error = %e,
                    "CDP connection dropped — reconnecting and retrying once"
                );
                if let Err(reconnect_err) = self.reconnect().await {
                    tracing::warn!(
                        error = %reconnect_err,
                        "CDP reconnect failed; surfacing original error"
                    );
                    return Err(e);
                }
                self.send_command(method, params).await
            }
            Err(e) => Err(e),
        }
    }

    /// `navigate` variant that uses the resilient sender. Same
    /// reconnect-once-on-drop behaviour as
    /// [`send_command_resilient`].
    pub async fn navigate_resilient(&self, url: &str) -> Result<(), CdpError> {
        self.send_command_resilient("Page.enable", serde_json::json!({}))
            .await?;
        self.send_command_resilient(
            "Page.navigate",
            serde_json::json!({ "url": url }),
        )
        .await?;
        Ok(())
    }

    /// `evaluate` variant that uses the resilient sender so a
    /// dropped WebSocket auto-reconnects + retries once. Used by
    /// the cortex action-dispatch path so a single Chrome hiccup
    /// during a multi-scenario eval doesn't permanently break the
    /// shared `Arc<CdpClient>` for every consumer that holds a
    /// clone of it.
    ///
    /// Mirrors the parsing logic in [`Self::evaluate`] (extracts
    /// `result.value` from the CDP `Runtime.evaluate` response).
    pub async fn evaluate_resilient(
        &self,
        expression: &str,
    ) -> Result<serde_json::Value, CdpError> {
        let result = self
            .send_command_resilient(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": expression,
                    "returnByValue": true,
                }),
            )
            .await?;
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// Replace the inner WebSocket with a fresh connection.
    ///
    /// Strategy:
    /// 1. Try the originally-supplied `ws_url`. Fast path — works
    ///    when Chrome dropped the socket but the page target is
    ///    still alive (the common case for `closed connection` /
    ///    `broken pipe` mid-eval).
    /// 2. If the original URL fails (Chrome destroyed the page,
    ///    HTTP 500 on the WebSocket upgrade, etc.) re-discover the
    ///    target via the same `discover_cdp_targets` path that
    ///    `connect_to_focused_app` uses, and try the first
    ///    successful WebSocket. Update the stored `ws_url` so the
    ///    next reconnect (if needed) uses the live target.
    ///
    /// Best-effort: holds the WebSocket mutex for the duration of
    /// the swap so concurrent senders don't see a half-open state.
    /// Errors propagate so the caller can fall back if needed.
    async fn reconnect(&self) -> Result<(), CdpError> {
        let stored_url = self
            .ws_url
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();

        let mut last_err: Option<CdpError> = None;
        let mut new_ws = None;
        let mut new_url = None;

        if !stored_url.is_empty() {
            match tokio_tungstenite::connect_async(&stored_url).await {
                Ok((ws, _)) => {
                    new_ws = Some(ws);
                }
                Err(e) => {
                    tracing::debug!(
                        url = %stored_url,
                        error = %e,
                        "CDP reconnect to stored ws_url failed; re-discovering"
                    );
                    last_err = Some(CdpError::ConnectionFailed(format!(
                        "reconnect to {stored_url} failed: {e}"
                    )));
                }
            }
        }

        if new_ws.is_none() {
            for target in crate::discovery::discover_cdp_targets() {
                if target.ws_url.is_empty() || target.ws_url == stored_url {
                    continue;
                }
                // Port pin: when the client was originally bound to a
                // specific CDP port (the eval's dedicated browser, the
                // MCP-spawned debugging instance, etc.), reconnect
                // MUST stay on that port. Without this filter, a dead
                // dedicated-browser process gets silently replaced by
                // whatever Chrome the user happens to have running —
                // observed on 2026-05-13 in
                // `recover_from_stale_state`, where the agent's
                // perception jumped from the cel-eval-chrome-profile
                // fixture to the user's real Chrome with Notion /
                // Timberhub tabs. The filter rejects any target on a
                // different port; the reconnect either finds a
                // pinned-port target or fails (and the caller
                // surfaces the error rather than silently hijacking).
                if let Some(pin) = self.pinned_port {
                    if target.port != pin {
                        tracing::debug!(
                            pin = pin,
                            target_port = target.port,
                            target_app = %target.app_name,
                            target_ws = %target.ws_url,
                            "CDP reconnect: skipping target on non-pinned port"
                        );
                        continue;
                    }
                }
                match tokio_tungstenite::connect_async(&target.ws_url).await {
                    Ok((ws, _)) => {
                        tracing::info!(
                            ws_url = %target.ws_url,
                            "CDP reconnect: switched to freshly-discovered target after \
                             stored ws_url died"
                        );
                        new_url = Some(target.ws_url.clone());
                        new_ws = Some(ws);
                        break;
                    }
                    Err(e) => {
                        last_err = Some(CdpError::ConnectionFailed(format!(
                            "reconnect to discovered {} failed: {e}",
                            target.ws_url
                        )));
                    }
                }
            }
        }

        let Some(ws) = new_ws else {
            return Err(last_err.unwrap_or_else(|| {
                CdpError::ConnectionFailed("reconnect failed: no usable target".into())
            }));
        };

        if let Some(url) = new_url {
            if let Ok(mut guard) = self.ws_url.lock() {
                *guard = url;
            }
        }
        let mut guard = self.ws.lock().await;
        *guard = ws;
        // Reset id counter — the new socket is a fresh CDP session
        // and Chrome's id-tracking starts over.
        self.next_id.store(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Recognise WebSocket-shutdown errors so `send_command_resilient`
/// knows when to attempt a reconnect rather than propagating the
/// failure unchanged.
fn is_connection_dropped(err: &CdpError) -> bool {
    let (CdpError::CommandFailed(msg) | CdpError::ConnectionFailed(msg)) = err else {
        return false;
    };
    let lower = msg.to_lowercase();
    lower.contains("closed connection")
        || lower.contains("broken pipe")
        || lower.contains("websocket closed")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("trying to work with closed connection")
}

/// Parse the TCP port out of a CDP WebSocket URL. CDP devtools URLs
/// have the shape `ws://127.0.0.1:9333/devtools/page/<id>` (or `wss://`
/// for remote tunnels). Returns `None` for malformed URLs or shapes
/// without an explicit port — those skip the port-pin filter and
/// preserve the legacy "any reachable target" reconnect behavior.
///
/// Pulled out as a free function so the reconnect's port-pin check
/// can use the same parsing the constructor used — drift here would
/// silently invalidate the pin.
fn parse_ws_port(ws_url: &str) -> Option<u16> {
    // Strip scheme.
    let rest = ws_url
        .strip_prefix("ws://")
        .or_else(|| ws_url.strip_prefix("wss://"))?;
    // Authority is everything before the first `/`.
    let authority = rest.split('/').next()?;
    // Port is everything after the LAST `:` (handles bracketed IPv6
    // addresses like `[::1]:9333` correctly: rfind picks the
    // post-bracket colon).
    let port_str = authority.rsplit(':').next()?;
    port_str.parse::<u16>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ws_port_extracts_from_standard_chrome_devtools_url() {
        // The shape Chrome's /json/list emits.
        assert_eq!(
            parse_ws_port("ws://127.0.0.1:9333/devtools/page/ABCDEF1234"),
            Some(9333)
        );
        assert_eq!(
            parse_ws_port("ws://localhost:9222/devtools/browser"),
            Some(9222)
        );
        // wss for remote-tunneled browsers (rare but valid).
        assert_eq!(
            parse_ws_port("wss://example.com:443/devtools/page/X"),
            Some(443)
        );
    }

    #[test]
    fn parse_ws_port_returns_none_for_malformed_or_missing_port() {
        // Missing scheme — not a CDP URL we recognise.
        assert_eq!(parse_ws_port("127.0.0.1:9333/devtools/page/X"), None);
        // Missing port — fall through to no-pin.
        assert_eq!(parse_ws_port("ws://localhost/devtools/page/X"), None);
        // Empty.
        assert_eq!(parse_ws_port(""), None);
        // Bogus port.
        assert_eq!(parse_ws_port("ws://127.0.0.1:notaport/path"), None);
    }
}
