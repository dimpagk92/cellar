//! Native Rust browser adapter — DOM perception via the cel-cdp transport.
//!
//! Slots into the same `AdapterDriver` framework as `adapters/numbers`,
//! `adapters/excel`, etc. The Cortex's tick loop activates this adapter
//! when CDP is reachable and walks `get_context()` every refresh cycle;
//! the resulting `dom:*`-id'd `ContextElement`s land in `ScreenContext`
//! alongside AX / display / network / signals — same merge path as
//! every other adapter.
//!
//! # Why a Rust adapter when `adapters/browser/` already exists in TS?
//!
//! `adapters/browser/` is the langgraph runtime's out-of-process
//! browser perception driver: full Playwright + CDP hybrid with
//! mutation tracking, watchdogs, and IPC. The canonical (Rust) runtime
//! used to bypass it entirely and read CDP directly inside the runner —
//! perception leaked into the wrong layer and never reached the model.
//! This adapter is the in-process Rust peer: same conceptual contract
//! (`AdapterDriver`), same perception pipeline, same element_id shape,
//! just no IPC overhead.
//!
//! As of Cut B (May 2026) the two implementations share a single canonical
//! partial manifest at `adapters/browser/manifest.json` — that file is the
//! source of truth for `app_patterns`, `platform`, `truth_surface`, and
//! `confidence`. Each implementation's `adapter.json` layers runtime-specific
//! overrides (entrypoint, runtime, element_types, refresh_ms, lifecycle).
//! `default_browser_manifest` embeds both layers via `include_str!` and
//! merges through `cel_cortex::merge_manifest_layers` so this in-Rust
//! manifest can't drift from what cortex's `discover_adapters` sees on disk.
//!
//! # When does it activate?
//!
//! `requires_frontmost: false` — the headless-Chrome eval scenarios
//! that motivated this adapter never bring Chrome to the macOS
//! foreground. `background_refresh: true` — `probe()` returns true
//! whenever a CDP target is reachable.
//!
//! Two construction patterns:
//!   - **Eager**: `with_cdp_client(client)` for callers (eval harness)
//!     that already have a CDP session pinned to a specific target.
//!     `probe()` only verifies that supplied client.
//!   - **Lazy**: `new()` for ambient-discovery callers (MCP server)
//!     that want the adapter to come alive when the user happens to
//!     have a browser open. `probe()` calls
//!     `cel_cdp::connect_to_focused_app()` and caches the result so
//!     subsequent ticks don't re-pay the discovery cost.

use std::sync::Arc;

use async_trait::async_trait;
use cel_cdp::CdpClient;
use cel_context::ContextElement;
use cel_cortex::{
    merge_manifest_layers, ActionResult, AdapterDriver, AdapterError, AdapterManifest,
};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::debug;

mod element_mapper;

pub use element_mapper::{dom_element_to_context_element, sanitize_id_part};

/// Native Rust browser adapter.
///
/// Internal state uses [`Mutex`] for the CDP client because lazy
/// discovery happens inside `probe()` (which takes `&self`) — the
/// alternative is forcing every caller to pre-bind a client at
/// construction, which doesn't match how MCP-server users open
/// browsers (after the cortex has booted).
pub struct BrowserAdapter {
    manifest: AdapterManifest,
    cdp_client: Mutex<Option<Arc<CdpClient>>>,
    /// `true` for callers that hand-picked a specific client at
    /// construction (`with_cdp_client`). When `true`, `probe()` skips
    /// ambient discovery — re-binding a different target mid-session
    /// would silently swap perception out from under the runner.
    pinned: bool,
    connected: bool,
}

impl Default for BrowserAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserAdapter {
    /// Lazy-discovery construction. Adapter starts unbound; `probe()`
    /// attempts `cel_cdp::connect_to_focused_app()` on each call and
    /// caches the result so subsequent ticks don't re-pay discovery
    /// cost. Right for ambient embedders (MCP server) where browsers
    /// come and go during a session.
    pub fn new() -> Self {
        Self {
            manifest: default_browser_manifest(),
            cdp_client: Mutex::new(None),
            pinned: false,
            connected: false,
        }
    }

    /// Eager construction with a pre-bound client. The eval harness
    /// uses this: by the time the cortex boots, the eval flow has
    /// already established a CDP session against the specific headless
    /// browser it owns and wants to keep talking to that one client
    /// for the run. Pinned mode means `probe()` only checks this
    /// client and never tries ambient discovery — preventing the
    /// adapter from drifting onto whatever browser the user happens
    /// to have open.
    pub fn with_cdp_client(client: Arc<CdpClient>) -> Self {
        Self {
            manifest: default_browser_manifest(),
            cdp_client: Mutex::new(Some(client)),
            pinned: true,
            connected: false,
        }
    }

    /// Bind a CDP client post-construction. No-ops if a client is
    /// already set so callers can't accidentally redirect perception
    /// mid-session.
    pub async fn set_cdp_client(&self, client: Arc<CdpClient>) {
        let mut guard = self.cdp_client.lock().await;
        if guard.is_none() {
            *guard = Some(client);
        }
    }
}

/// Shared partial manifest for the browser-perception adapter pair, embedded
/// at compile time. Single source of truth for `app_patterns`, `platform`,
/// `context.truth_surface`, `context.confidence`, and `verification.truth_surface`
/// across the TS adapter (`adapters/browser/`) and this Rust adapter.
///
/// `cel_cortex::load_manifest` resolves the same file at runtime via the
/// `manifest_extends` pointer in `adapter.json` — embedding the bytes here
/// keeps the in-Rust manifest agreeing with what diagnostics see on disk
/// without forcing the crate to do file I/O at construction time.
const SHARED_BROWSER_MANIFEST: &str = include_str!("../../browser/manifest.json");
/// This adapter's runtime-specific overlay (display_name, runtime, lifecycle,
/// element_types, refresh_ms, manifest_alias). Layered on top of
/// `SHARED_BROWSER_MANIFEST` to produce the full `AdapterManifest`.
const BROWSER_RS_ADAPTER_OVERLAY: &str = include_str!("../adapter.json");

fn default_browser_manifest() -> AdapterManifest {
    // Embed and merge both JSON layers at construction time so the in-Rust
    // manifest can't drift from the on-disk adapter.json that cortex's
    // discover_adapters reads for diagnostics. Pre-Cut B, the manifest was
    // hand-maintained in three places (this function, adapter.json, the
    // peer adapters/browser/adapter.json) and the bookkeeping comment
    // `// Mirrors adapters/browser/adapter.json` was load-bearing.
    let shared: Value = serde_json::from_str(SHARED_BROWSER_MANIFEST)
        .expect("adapters/browser/manifest.json must be valid JSON");
    let overlay: Value = serde_json::from_str(BROWSER_RS_ADAPTER_OVERLAY)
        .expect("adapters/browser-rs/adapter.json must be valid JSON");
    let merged = merge_manifest_layers(shared, overlay);
    serde_json::from_value(merged)
        .expect("merged browser manifest must deserialize into AdapterManifest")
}

#[async_trait]
impl AdapterDriver for BrowserAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    async fn activate(&mut self) -> Result<(), AdapterError> {
        // The cortex calls activate() *after* probe() returns true,
        // so a successful probe must have already attached a client.
        // Activating without a client at this point is a wiring bug
        // worth surfacing rather than silently degrading to empty
        // perception forever.
        if self.cdp_client.lock().await.is_none() {
            return Err(AdapterError::ContextReadFailed(
                "browser adapter activated without a bound CDP client".into(),
            ));
        }
        self.connected = true;
        debug!("browser adapter: activated");
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<(), AdapterError> {
        self.connected = false;
        debug!("browser adapter: deactivated");
        Ok(())
    }

    async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError> {
        if !self.connected {
            return Ok(Vec::new());
        }
        let client = {
            // Hold the lock only long enough to clone the Arc — the
            // expensive DOM walk happens outside the critical section so
            // a slow CDP round-trip doesn't block parallel probes.
            let guard = self.cdp_client.lock().await;
            match guard.as_ref() {
                Some(client) => Arc::clone(client),
                None => return Ok(Vec::new()),
            }
        };
        // extract_page_content does the heavy DOM walk in JS via CDP
        // Runtime.evaluate. Failure here returns empty so the cortex
        // tick loop tolerates the miss and tries again next cycle —
        // browser tabs close, navigate, or hang transiently and that
        // shouldn't cascade into adapter-error state.
        match cel_cdp::extract_page_content(&client).await {
            Ok(page) => Ok(page
                .interactive_elements
                .iter()
                .enumerate()
                .map(|(idx, dom)| dom_element_to_context_element(dom, idx))
                .collect()),
            Err(err) => {
                debug!("browser adapter: extract_page_content failed: {err}");
                Ok(Vec::new())
            }
        }
    }

    async fn execute(&self, action: &str, _params: Value) -> Result<ActionResult, AdapterError> {
        // Browser actions (cdp_eval, set_value, click) flow through the
        // canonical runner's CDP dispatch path — the runner already
        // routes `dom:*` element_ids through `cortex::build_click_js`
        // and friends. The adapter intentionally exposes no actions of
        // its own to avoid two parallel dispatch paths going stale at
        // different rates.
        Err(AdapterError::ExecutionFailed(format!(
            "browser adapter does not expose direct actions — use canonical runner CDP dispatch (action requested: {action:?})"
        )))
    }

    async fn probe(&self) -> bool {
        // Two probe modes:
        //   - **Pinned** (with_cdp_client): only ping the supplied
        //     client. Don't attempt ambient discovery — that would
        //     swap perception onto a different browser if the user
        //     happened to have one focused while eval was running.
        //   - **Lazy** (new): try the cached client, and if absent
        //     attempt `connect_to_focused_app()` and cache the result.
        //     Right for MCP-style ambient embedders where a browser
        //     can show up at any point during a session.
        //
        // Either way, the final test is `get_url()` — a cheap CDP
        // round-trip that confirms the WebSocket is alive without
        // paying for a full DOM walk. Errors collapse to false so the
        // cortex marks the adapter inactive and stops calling
        // get_context until the next probe finds CDP again.
        let client = {
            let mut guard = self.cdp_client.lock().await;
            if guard.is_none() && !self.pinned {
                if let Some(c) = cel_cdp::connect_to_focused_app().await {
                    *guard = Some(Arc::new(c));
                }
            }
            match guard.as_ref() {
                Some(c) => Arc::clone(c),
                None => return false,
            }
        };
        client.get_url().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_accessibility::ElementState;
    use cel_cdp::DomElement;
    use cel_context::ContextSource;

    #[test]
    fn manifest_declares_background_refresh_for_headless() {
        // requires_frontmost would deadlock the adapter in any headless
        // eval flow (Chrome --headless is never AXFocusedApplication).
        // background_refresh + probe() is the path that actually fires.
        // Test pins both — flipping either to the wrong value silently
        // disables the adapter for the very scenarios it was built for.
        let m = default_browser_manifest();
        assert!(!m.lifecycle.requires_frontmost);
        assert!(m.lifecycle.background_refresh);
    }

    #[test]
    fn manifest_app_patterns_match_common_chromium_browsers() {
        // The app_patterns drive frontmost-match in non-headless flows
        // (e.g. an MCP user clicking around their actual Chrome). Pin
        // the regex shapes so a future refactor doesn't drop a
        // distribution and silently break perception when the user
        // happens to be on Brave / Arc / Edge.
        let m = default_browser_manifest();
        let joined = m.app_patterns.join(" ");
        for needle in ["chrome", "chromium", "brave", "arc", "edge"] {
            assert!(
                joined.contains(needle),
                "app_patterns should cover {needle}: got {joined}"
            );
        }
    }

    #[test]
    fn manifest_alias_points_at_typescript_peer() {
        // The TS peer at `adapters/browser/` and this Rust adapter form
        // one logical adapter via bidirectional manifest_alias. Diagnostics
        // (`cel_cortex::group_paired_manifests`) only pair them when both
        // sides agree; if this constant drifts away from "browser" the
        // pair silently splits in two and dashboards stop showing the
        // unification. Now that the value lives in adapter.json (Cut B),
        // this test also guards against an accidental edit to the JSON
        // bleeding through include_str!.
        let m = default_browser_manifest();
        assert_eq!(m.manifest_alias.as_deref(), Some("browser"));
    }

    #[test]
    fn manifest_inherits_shared_fields_from_parent_layer() {
        // Cut B contract: the shared manifest at adapters/browser/manifest.json
        // is authoritative for fields that must agree across the TS and Rust
        // implementations. If a future refactor removes the include_str! of
        // the shared layer, these values would silently fall back to serde
        // defaults (truth_surface → "native_api", and friends), and
        // attribution would regress. Pin the inheritance here so the bug
        // surfaces at this test, not at a downstream telemetry consumer.
        let m = default_browser_manifest();
        assert_eq!(m.context.truth_surface, "browser_dom");
        assert_eq!(m.verification.truth_surface, "browser_dom");
        // app_patterns comes from the shared layer too — the Rust adapter's
        // overlay deliberately doesn't redeclare them.
        let joined = m.app_patterns.join(" ");
        for needle in ["chrome", "chromium", "brave", "arc", "edge"] {
            assert!(joined.contains(needle));
        }
    }

    #[test]
    fn confidence_pinned_to_browser_dom_tier() {
        // Browser DOM ranks above raw AX (~0.85) but below confirmed
        // adapter facts (0.95+) and document-model adapters (Numbers
        // 0.97). A peer dom:* element should win against an ax:* peer
        // at the same id but never crowd out a Numbers cell. Source of
        // truth for the literal: `adapters/browser/manifest.json`'s
        // `context.confidence`. Bumping that file shifts where the test
        // lands — adjust the literal here in lock-step.
        let m = default_browser_manifest();
        assert!((m.context.confidence - 0.88).abs() < f64::EPSILON);
        assert!(m.context.confidence > 0.85);
        assert!(m.context.confidence < 0.95);
    }

    #[tokio::test]
    async fn activate_without_client_errors() {
        // Activating without a bound client would silently degrade to
        // empty perception forever. Better to surface as a hard error
        // so the registration-time wiring bug shows up at boot, not
        // turn 30 of a live run.
        let mut adapter = BrowserAdapter::new();
        let err = adapter.activate().await.expect_err("should error");
        let message = format!("{err}");
        assert!(
            message.to_lowercase().contains("cdp"),
            "error should mention CDP: {message}"
        );
    }

    #[tokio::test]
    async fn probe_false_without_client() {
        // The cortex tick loop uses probe() to decide activation —
        // returning true with no client would cause activate() to fire
        // and immediately error.
        let adapter = BrowserAdapter::new();
        assert!(!adapter.probe().await);
    }

    #[tokio::test]
    async fn get_context_empty_when_not_connected() {
        // Pre-activate, get_context must not attempt a CDP call. The
        // cortex calls activate() then get_context() in sequence; a
        // racy get_context() before activate completes shouldn't blow
        // up — it returns empty and the next tick fills it in.
        let adapter = BrowserAdapter::new();
        let ctx = adapter
            .get_context()
            .await
            .expect("should return ok even with no client");
        assert!(ctx.is_empty());
    }

    #[tokio::test]
    async fn execute_refuses_to_dispatch_actions() {
        // Pin the no-actions contract — the adapter exists for
        // perception, not action dispatch. Any future refactor that
        // accidentally adds actions here would create a parallel path
        // to the canonical runner's CDP dispatch and the two would
        // drift. Test fails loudly if that happens.
        let adapter = BrowserAdapter::new();
        let err = adapter
            .execute("click", serde_json::json!({}))
            .await
            .expect_err("should refuse");
        let message = format!("{err}");
        assert!(message.contains("does not expose"));
    }

    // Sanity: sourced elements must carry the right tag so downstream
    // SourceSummary aggregation and dom:* dispatch routing work.
    #[test]
    fn produced_elements_tagged_cdp_source() {
        let dom = DomElement {
            tag: "button".into(),
            element_type: "button".into(),
            text: "Click me".into(),
            href: None,
            input_type: None,
            value: None,
            placeholder: None,
            dom_id: Some("ok".into()),
            dom_name: None,
            bounds: None,
            backend_node_id: Some(1),
            aria_role: None,
            aria_label: None,
            is_visible: true,
            is_enabled: true,
            is_checked: None,
            is_expanded: None,
            shadow_depth: 0,
            paint_order: 1,
            viewport_relation: "visible".into(),
        };
        let el = dom_element_to_context_element(&dom, 0);
        assert_eq!(el.id, "dom:button:ok");
        // The cortex tick loop reads the manifest's
        // `truth_surface == "browser_dom"` and tags adapter elements as
        // `Cdp` (cortex.rs ~L654). The mapper pre-tags at the source so
        // pre-merge / post-merge attribution stays identical — anyone
        // reading raw adapter output sees the same value
        // SourceSummary will end up counting.
        assert_eq!(el.source, ContextSource::Cdp);
        // ElementState wrapper conventionally inert when CDP doesn't
        // emit per-element focus.
        let _: &ElementState = &el.state;
    }
}
