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
    ws: Mutex<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
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
                            return Ok(response.get("result").cloned().unwrap_or(serde_json::Value::Null));
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
            .send_command(
                "DOM.getOuterHTML",
                serde_json::json!({ "nodeId": node_id }),
            )
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
        let result = self
            .evaluate("document.title")
            .await?;
        Ok(result.as_str().unwrap_or("").to_string())
    }

    /// Get the page URL.
    pub async fn get_url(&self) -> Result<String, CdpError> {
        let result = self
            .evaluate("window.location.href")
            .await?;
        Ok(result.as_str().unwrap_or("").to_string())
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
    pub async fn dispatch_key_event(
        &self,
        event_type: &str,
        key: &str,
    ) -> Result<(), CdpError> {
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
        Ok(result.get("cookies").cloned().unwrap_or(serde_json::Value::Array(vec![])))
    }

    /// Get a specific cookie by name. Returns the cookie value or None.
    pub async fn get_cookie(&self, name: &str) -> Result<Option<String>, CdpError> {
        let cookies = self.get_cookies().await?;
        if let Some(arr) = cookies.as_array() {
            for cookie in arr {
                if cookie.get("name").and_then(|n| n.as_str()) == Some(name) {
                    return Ok(cookie.get("value").and_then(|v| v.as_str()).map(|s| s.to_string()));
                }
            }
        }
        Ok(None)
    }

    /// Read localStorage for a given key. Returns the value or null.
    pub async fn get_local_storage(&self, key: &str) -> Result<Option<String>, CdpError> {
        let expr = format!("localStorage.getItem({})", serde_json::to_string(key).unwrap_or_else(|_| format!("\"{}\"", key)));
        let result = self.evaluate(&expr).await?;
        match result {
            serde_json::Value::String(s) => Ok(Some(s)),
            serde_json::Value::Null => Ok(None),
            _ => Ok(Some(result.to_string())),
        }
    }

    /// Read sessionStorage for a given key.
    pub async fn get_session_storage(&self, key: &str) -> Result<Option<String>, CdpError> {
        let expr = format!("sessionStorage.getItem({})", serde_json::to_string(key).unwrap_or_else(|_| format!("\"{}\"", key)));
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
    pub async fn get_network_requests(&self, limit: usize) -> Result<Vec<cel_network::HttpEvent>, CdpError> {
        let js = format!(r#"
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
        "#, limit);

        let result = self.evaluate(&js).await?;
        let events: Vec<cel_network::HttpEvent> = serde_json::from_value(result)
            .unwrap_or_default();
        Ok(events)
    }

    /// Navigate to a URL.
    pub async fn navigate(&self, url: &str) -> Result<(), CdpError> {
        self.send_command("Page.enable", serde_json::json!({})).await?;
        self.send_command("Page.navigate", serde_json::json!({ "url": url })).await?;
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
}
