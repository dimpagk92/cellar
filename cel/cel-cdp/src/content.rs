//! Page Content Extraction via CDP
//!
//! Extracts structured text content from the active page using CDP.
//! Returns content that can be fused with AX data in the context merger.
//!
//! Improvements (informed by Stagehand + Browser-use competitive analysis):
//! - Element bounds (bounding rect) for coordinate-based interaction
//! - Backend node ID for CDP operations
//! - ARIA role extraction (computed role from the browser)
//! - Visibility and enabled state
//! - Checked / expanded state for checkboxes, details, etc.
//! - Shadow DOM traversal (open + closed via __cel_closedShadows)
//! - Paint order capture for occlusion detection
//! - Scroll position awareness (viewport relation)
//! - Expanded interactive element detection (event handlers, framework attrs, cursor)

use crate::client::{CdpClient, CdpError};
use serde::{Deserialize, Serialize};

/// Bounding rectangle for an element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// A console log message captured from the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleMessage {
    pub level: String, // "log", "warn", "error", "info", "debug"
    pub text: String,
}

/// A network resource entry captured via the Performance API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceEntry {
    /// The request URL.
    pub url: String,
    /// Round-trip duration in milliseconds.
    pub duration_ms: u64,
    /// HTTP status code, if available.
    pub status: Option<u16>,
    /// Transfer size in bytes.
    pub size: u64,
}

/// Extracted page content from a CDP session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageContent {
    /// Page title.
    pub title: String,
    /// Page URL.
    pub url: String,
    /// Main text content of the page body (stripped of HTML).
    pub body_text: String,
    /// Structured text blocks (headings, paragraphs, code blocks).
    pub text_blocks: Vec<TextBlock>,
    /// Interactive elements found via DOM (forms, inputs, links).
    pub interactive_elements: Vec<DomElement>,
    /// Console error messages captured from the page.
    /// Uses JS evaluation to detect `window.__celConsoleErrors` and visible
    /// error elements in the DOM (role="alert", .error, .alert-danger, etc.).
    pub console_logs: Vec<ConsoleMessage>,
    /// Recent network requests (fetch/XHR) captured via the Performance API.
    pub network_requests: Vec<ResourceEntry>,
    /// Page load time in milliseconds (navigationStart to loadEventEnd).
    pub load_time_ms: Option<u64>,
    /// DOM content loaded time in milliseconds (navigationStart to domContentLoadedEventEnd).
    pub dom_ready_ms: Option<u64>,
    /// Viewport metadata for scroll position awareness.
    pub viewport: Option<ViewportInfo>,
}

/// Viewport metadata for scroll position awareness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewportInfo {
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub inner_width: f64,
    pub inner_height: f64,
    pub scroll_height: f64,
    pub scroll_width: f64,
}

/// A block of text content from the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    pub block_type: String, // "heading", "paragraph", "code", "list_item"
    pub text: String,
    pub level: Option<u8>, // For headings: 1-6
}

/// An interactive DOM element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomElement {
    pub tag: String,
    pub element_type: String, // "button", "input", "link", "select"
    pub text: String,
    pub href: Option<String>,
    pub input_type: Option<String>,
    pub value: Option<String>,
    pub placeholder: Option<String>,
    /// Bounding rectangle (viewport-relative).
    pub bounds: Option<ElementBounds>,
    /// Incrementing ID for element identification.
    pub backend_node_id: Option<u32>,
    /// Computed ARIA role (e.g., "button", "textbox", "link").
    pub aria_role: Option<String>,
    /// ARIA label text.
    pub aria_label: Option<String>,
    /// Whether the element is visible in the viewport.
    pub is_visible: bool,
    /// Whether the element is enabled (not disabled).
    pub is_enabled: bool,
    /// Checked state for checkboxes/radios.
    pub is_checked: Option<bool>,
    /// Expanded state for details/accordions.
    pub is_expanded: Option<bool>,
    /// Shadow DOM depth (0 = main document).
    pub shadow_depth: u8,
    /// Paint order for occlusion detection.
    pub paint_order: u32,
    /// Position relative to viewport.
    pub viewport_relation: String, // "visible", "above", "below"
}

/// Extract page content from an active CDP connection.
pub async fn extract_page_content(client: &CdpClient) -> Result<PageContent, CdpError> {
    let title = client.get_title().await.unwrap_or_default();
    let url = client.get_url().await.unwrap_or_default();

    // Extract body text via JavaScript
    let body_text = client
        .evaluate("document.body?.innerText || ''")
        .await
        .unwrap_or(serde_json::Value::String(String::new()));
    let body_text = body_text.as_str().unwrap_or("").to_string();

    // Extract structured text blocks
    let blocks_js = r#"
        (function() {
            const blocks = [];
            const selectors = [
                { sel: 'h1,h2,h3,h4,h5,h6', type: 'heading' },
                { sel: 'p', type: 'paragraph' },
                { sel: 'pre,code', type: 'code' },
                { sel: 'li', type: 'list_item' },
            ];
            for (const { sel, type: blockType } of selectors) {
                for (const el of document.querySelectorAll(sel)) {
                    const text = el.innerText?.trim();
                    if (text && text.length > 0 && text.length < 5000) {
                        const block = { block_type: blockType, text };
                        if (blockType === 'heading') {
                            block.level = parseInt(el.tagName[1]) || 1;
                        }
                        blocks.push(block);
                    }
                }
            }
            return blocks.slice(0, 200);
        })()
    "#;
    let text_blocks: Vec<TextBlock> = client
        .evaluate(blocks_js)
        .await
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    // Extract interactive elements with full metadata
    let interactive_js = r#"
        (function() {
            var MAX_TEXT = 200;
            var MAX_ELEMENTS = 500;
            var nodeCounter = 0;
            var scrollY = window.scrollY || 0;
            var innerH = window.innerHeight;
            var elements = [];

            var INTERACTIVE_TAGS = {
                'A':1,'BUTTON':1,'INPUT':1,'SELECT':1,'TEXTAREA':1,
                'DETAILS':1,'SUMMARY':1,'LABEL':1,'OPTION':1
            };
            var INTERACTIVE_ROLES = {
                'button':1,'link':1,'textbox':1,'checkbox':1,'radio':1,
                'combobox':1,'listbox':1,'menuitem':1,'menuitemcheckbox':1,
                'menuitemradio':1,'option':1,'slider':1,'spinbutton':1,
                'switch':1,'tab':1,'treeitem':1,'searchbox':1,'gridcell':1
            };
            var EVENT_ATTRS = ['onclick','onmousedown','onmouseup','onpointerdown','ontouchstart'];
            var FRAMEWORK_ATTRS = ['data-action','ng-click'];
            var SKIP_TAGS = {'SCRIPT':1,'STYLE':1,'NOSCRIPT':1,'META':1,'LINK':1,'HEAD':1,'BR':1,'HR':1,'TEMPLATE':1};

            function isVisible(el) {
                if (el.offsetWidth === 0 && el.offsetHeight === 0 && !el.getClientRects().length) return false;
                try {
                    var s = getComputedStyle(el);
                    if (s.display === 'none' || s.visibility === 'hidden') return false;
                    if (parseFloat(s.opacity) === 0) return false;
                } catch(e) {}
                return true;
            }

            function hasEventHandlers(el) {
                for (var i = 0; i < EVENT_ATTRS.length; i++) {
                    if (el.hasAttribute(EVENT_ATTRS[i])) return true;
                }
                for (var i = 0; i < FRAMEWORK_ATTRS.length; i++) {
                    if (el.hasAttribute(FRAMEWORK_ATTRS[i])) return true;
                }
                return false;
            }

            function isInteractive(el) {
                if (INTERACTIVE_TAGS[el.tagName]) return true;
                var role = el.getAttribute('role') || '';
                if (role && INTERACTIVE_ROLES[role]) return true;
                if (el.hasAttribute('tabindex')) return true;
                if (el.getAttribute('contenteditable') === 'true') return true;
                if (hasEventHandlers(el)) return true;
                try { if (getComputedStyle(el).cursor === 'pointer') return true; } catch(e) {}
                return false;
            }

            function getElementType(el) {
                var role = el.getAttribute('role') || '';
                if (role === 'button') return 'button';
                if (role === 'link') return 'link';
                if (role === 'textbox' || role === 'searchbox') return 'input';
                if (role === 'checkbox') return 'checkbox';
                if (role === 'radio') return 'radio';
                if (role === 'combobox' || role === 'listbox') return 'select';
                if (role === 'menuitem' || role === 'menuitemcheckbox' || role === 'menuitemradio') return 'menuitem';
                if (role === 'tab') return 'tab';
                if (role === 'slider' || role === 'spinbutton') return 'input';
                var tag = el.tagName;
                if (tag === 'BUTTON' || tag === 'SUMMARY') return 'button';
                if (tag === 'A') return 'link';
                if (tag === 'INPUT') {
                    var t = el.type || 'text';
                    if (t === 'submit' || t === 'button' || t === 'reset' || t === 'image') return 'button';
                    if (t === 'checkbox') return 'checkbox';
                    if (t === 'radio') return 'radio';
                    return 'input';
                }
                if (tag === 'TEXTAREA') return 'input';
                if (tag === 'SELECT') return 'select';
                if (tag === 'DETAILS') return 'details';
                return 'button';
            }

            function getPaintOrder(el, idx) {
                try {
                    var s = getComputedStyle(el);
                    var z = parseInt(s.zIndex, 10);
                    if (s.position !== 'static' && !isNaN(z)) return 100000 + z * 1000 + idx;
                } catch(e) {}
                return idx;
            }

            function getViewportRelation(bounds) {
                if (!bounds) return 'visible';
                if (bounds.y + bounds.height < 0) return 'above';
                if (bounds.y > innerH) return 'below';
                return 'visible';
            }

            function walkDOM(root, shadowDepth, depth) {
                if (depth > 20 || elements.length >= MAX_ELEMENTS) return;
                var children = root.children || root.childNodes;
                for (var i = 0; i < children.length; i++) {
                    if (elements.length >= MAX_ELEMENTS) return;
                    var el = children[i];
                    if (el.nodeType !== 1) continue;
                    if (SKIP_TAGS[el.tagName]) continue;

                    var visible = isVisible(el);
                    var shadow = el.shadowRoot || (window.__cel_closedShadows && window.__cel_closedShadows.get(el));
                    if (!visible && !shadow) continue;

                    if (visible && isInteractive(el)) {
                        nodeCounter++;
                        var rect = null;
                        try {
                            var r = el.getBoundingClientRect();
                            if (r.width > 0 || r.height > 0) {
                                rect = { x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height) };
                            }
                        } catch(e) {}

                        var role = el.getAttribute('role') || null;
                        var ariaLabel = el.getAttribute('aria-label') || null;
                        var text = '';
                        if (el.tagName !== 'INPUT' && el.tagName !== 'SELECT') {
                            text = (el.innerText || el.textContent || '').trim().slice(0, MAX_TEXT);
                        }
                        if (!text && ariaLabel) text = ariaLabel;

                        var checked = null;
                        if (el.type === 'checkbox' || el.type === 'radio') checked = !!el.checked;
                        else if (el.getAttribute('aria-checked') === 'true') checked = true;
                        else if (el.getAttribute('aria-checked') === 'false') checked = false;

                        var expanded = null;
                        if (el.hasAttribute('aria-expanded')) expanded = el.getAttribute('aria-expanded') === 'true';
                        else if (el.tagName === 'DETAILS') expanded = !!el.open;

                        elements.push({
                            tag: el.tagName.toLowerCase(),
                            element_type: getElementType(el),
                            text: text,
                            href: el.href || el.getAttribute('href') || null,
                            input_type: el.type || null,
                            value: el.value !== undefined ? (el.value || '').slice(0, MAX_TEXT) || null : null,
                            placeholder: el.placeholder || null,
                            bounds: rect,
                            backend_node_id: nodeCounter,
                            aria_role: role,
                            aria_label: ariaLabel,
                            is_visible: visible,
                            is_enabled: !el.disabled && el.getAttribute('aria-disabled') !== 'true',
                            is_checked: checked,
                            is_expanded: expanded,
                            shadow_depth: shadowDepth,
                            paint_order: getPaintOrder(el, nodeCounter),
                            viewport_relation: getViewportRelation(rect),
                        });
                    }

                    // Recurse into shadow DOM (open + closed)
                    if (shadow) walkDOM(shadow, shadowDepth + 1, depth + 1);

                    // Recurse into same-origin iframes
                    if (el.tagName === 'IFRAME') {
                        try {
                            var iDoc = el.contentDocument;
                            if (iDoc) walkDOM(iDoc.body || iDoc, shadowDepth, depth + 1);
                        } catch(e) {}
                    }

                    walkDOM(el, shadowDepth, depth + 1);
                }
            }

            walkDOM(document.body || document.documentElement, 0, 0);
            return elements;
        })()
    "#;
    let interactive_elements: Vec<DomElement> = client
        .evaluate(interactive_js)
        .await
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    // Extract viewport info
    let viewport_js = r#"
        JSON.stringify({
            scroll_x: window.scrollX || 0,
            scroll_y: window.scrollY || 0,
            inner_width: window.innerWidth,
            inner_height: window.innerHeight,
            scroll_height: document.documentElement.scrollHeight,
            scroll_width: document.documentElement.scrollWidth
        })
    "#;
    let viewport: Option<ViewportInfo> = client
        .evaluate(viewport_js)
        .await
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .and_then(|s| serde_json::from_str(&s).ok());

    // Extract performance timing
    let perf_js = r#"
        try {
            var t = performance.timing;
            JSON.stringify({
                load: t.loadEventEnd > 0 ? t.loadEventEnd - t.navigationStart : null,
                domReady: t.domContentLoadedEventEnd > 0 ? t.domContentLoadedEventEnd - t.navigationStart : null
            });
        } catch(e) { '{}' }
    "#;
    let perf_json = client
        .evaluate(perf_js)
        .await
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "{}".to_string());

    let perf: serde_json::Value =
        serde_json::from_str(&perf_json).unwrap_or(serde_json::Value::Object(Default::default()));
    let load_time_ms = perf.get("load").and_then(|v| v.as_u64());
    let dom_ready_ms = perf.get("domReady").and_then(|v| v.as_u64());

    // Capture console errors via JS evaluation.
    // This won't capture all console.log calls, but detects visible error states
    // (window.__celConsoleErrors + DOM error elements) which is what the planner needs.
    let console_js = r#"
        (function() {
            var errors = [];
            // Check for unhandled errors stored by error event listeners
            if (window.__celConsoleErrors) {
                errors = window.__celConsoleErrors;
            }
            // Also check for visible error elements in the DOM
            var errorEls = document.querySelectorAll('[role="alert"], .error, .alert-danger, .alert-error');
            errorEls.forEach(function(el) {
                if (el.textContent.trim()) {
                    errors.push({level: "error", text: el.textContent.trim().substring(0, 200)});
                }
            });
            return JSON.stringify(errors.slice(0, 20));
        })()
    "#;
    let console_json = client
        .evaluate(console_js)
        .await
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "[]".to_string());
    let console_logs: Vec<ConsoleMessage> = serde_json::from_str(&console_json).unwrap_or_default();

    // Capture recent fetch/XHR network requests via the Performance API.
    // This avoids needing a WebSocket event listener loop for Network.responseReceived.
    let network_js = r#"
        JSON.stringify(
            performance.getEntriesByType('resource')
                .filter(function(e) { return e.initiatorType === 'fetch' || e.initiatorType === 'xmlhttprequest'; })
                .slice(-20)
                .map(function(e) {
                    return {
                        url: e.name,
                        duration_ms: Math.round(e.duration),
                        status: e.responseStatus || null,
                        size: e.transferSize || 0
                    };
                })
        )
    "#;
    let network_json = client
        .evaluate(network_js)
        .await
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "[]".to_string());
    let network_requests: Vec<ResourceEntry> =
        serde_json::from_str(&network_json).unwrap_or_default();

    Ok(PageContent {
        title,
        url,
        body_text: truncate_with_ellipsis(&body_text, 10_000),
        text_blocks,
        interactive_elements,
        console_logs,
        network_requests,
        load_time_ms,
        dom_ready_ms,
        viewport,
    })
}

fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_content_serialization() {
        let content = PageContent {
            title: "Test Page".into(),
            url: "https://example.com".into(),
            body_text: "Hello world".into(),
            text_blocks: vec![TextBlock {
                block_type: "heading".into(),
                text: "Welcome".into(),
                level: Some(1),
            }],
            interactive_elements: vec![DomElement {
                tag: "button".into(),
                element_type: "button".into(),
                text: "Submit".into(),
                href: None,
                input_type: None,
                value: None,
                placeholder: None,
                bounds: Some(ElementBounds {
                    x: 10,
                    y: 20,
                    width: 100,
                    height: 40,
                }),
                backend_node_id: Some(1),
                aria_role: Some("button".into()),
                aria_label: Some("Submit form".into()),
                is_visible: true,
                is_enabled: true,
                is_checked: None,
                is_expanded: None,
                shadow_depth: 0,
                paint_order: 1,
                viewport_relation: "visible".into(),
            }],
            console_logs: vec![ConsoleMessage {
                level: "error".into(),
                text: "Something went wrong".into(),
            }],
            network_requests: vec![ResourceEntry {
                url: "https://api.example.com/data".into(),
                duration_ms: 150,
                status: Some(200),
                size: 4096,
            }],
            load_time_ms: Some(1200),
            dom_ready_ms: Some(800),
            viewport: Some(ViewportInfo {
                scroll_x: 0.0,
                scroll_y: 0.0,
                inner_width: 1280.0,
                inner_height: 800.0,
                scroll_height: 2400.0,
                scroll_width: 1280.0,
            }),
        };
        let json = serde_json::to_string(&content).unwrap();
        let back: PageContent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.title, "Test Page");
        assert_eq!(back.text_blocks.len(), 1);
        assert_eq!(back.interactive_elements.len(), 1);
        assert!(back.interactive_elements[0].bounds.is_some());
        assert_eq!(back.interactive_elements[0].backend_node_id, Some(1));
        assert_eq!(
            back.interactive_elements[0].aria_role.as_deref(),
            Some("button")
        );
        assert!(back.interactive_elements[0].is_visible);
        assert_eq!(back.interactive_elements[0].shadow_depth, 0);
        assert_eq!(back.interactive_elements[0].viewport_relation, "visible");
        assert_eq!(back.console_logs.len(), 1);
        assert_eq!(back.console_logs[0].level, "error");
        assert_eq!(back.network_requests.len(), 1);
        assert_eq!(back.load_time_ms, Some(1200));
        assert_eq!(back.dom_ready_ms, Some(800));
        assert!(back.viewport.is_some());
        let vp = back.viewport.unwrap();
        assert_eq!(vp.inner_width, 1280.0);
        assert_eq!(vp.scroll_height, 2400.0);
    }

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        let text = "Τ".repeat(10_005);
        let truncated = truncate_with_ellipsis(&text, 10_000);

        assert!(truncated.ends_with("..."));
        assert_eq!(truncated.chars().count(), 10_003);
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
