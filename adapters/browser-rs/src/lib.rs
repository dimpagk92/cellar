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
//! merges through `cel_adapter_sdk::merge_manifest_layers` so this in-Rust
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
use cel_adapter_sdk::{
    merge_manifest_layers, ActionResult, AdapterDriver, AdapterError, AdapterManifest,
};
use cel_cdp::CdpClient;
use cel_context::ContextElement;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::debug;

mod element_mapper;
pub mod overlay_detector;

pub use element_mapper::{dom_element_to_context_element, sanitize_id_part};
pub use overlay_detector::{
    detect_overlay, dismiss_overlay, tag_blocked_elements, BlockingOverlay, DismissOverlayOptions,
    DismissResult,
};

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
    /// Per-tick counter so `get_context` only runs the overlay-detection
    /// JS every Nth call. Detection costs ~5–20ms per CDP round-trip,
    /// which we'd rather not pay on every 100ms cortex tick. Wrapped
    /// in a `Mutex` because `get_context` takes `&self`.
    overlay_tick_counter: Mutex<u64>,
    /// `true` (default) ⇒ when `get_context` detects an overlay AND the
    /// auto-dismiss tick fires, call `dismiss_overlay` privacy-preservingly
    /// (reject > close > accept > nuclear-hide). Disable via
    /// `with_auto_dismiss_overlays(false)` when the task is to interact
    /// with the banner itself (rare).
    auto_dismiss_overlays: bool,
    /// Detection cadence: run the detection script every Nth tick of
    /// `get_context`. Defaults to 3 (~300ms at default tick) — fast
    /// enough that the planner doesn't burn a turn on a banner, slow
    /// enough that the CDP eval cost doesn't dominate.
    overlay_detect_every: u64,
}

impl Default for BrowserAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Default auto-dismiss policy.
///
/// `lazy=true` is the [`BrowserAdapter::new`] (ambient / MCP) path; `lazy=false`
/// is [`BrowserAdapter::with_cdp_client`] (eval harness / benchmarks).
///
/// `CEL_AUTO_DISMISS_OVERLAYS` (truthy `1|true|yes|on`, falsy `0|false|no|off`)
/// overrides both paths. When unset:
/// - **lazy** → `false`. Customer-facing MCP runs (Claude Code, Cursor) keep
///   banners visible by default — the user is usually reasoning about consent,
///   not asking the agent to bypass it.
/// - **pinned** → `true`. Benchmarks (WebVoyager / Mind2Web / cellar-mcp) need
///   the banner out of the way to measure the agent on the underlying task.
///
/// Callers that disagree with this heuristic should pass an explicit value via
/// [`BrowserAdapter::with_auto_dismiss_overlays`] at construction.
fn default_auto_dismiss(lazy: bool) -> bool {
    default_auto_dismiss_from(lazy, std::env::var("CEL_AUTO_DISMISS_OVERLAYS").ok())
}

/// Pure helper for [`default_auto_dismiss`] — accepts the env value
/// explicitly so unit tests can pin the resolution table without mutating
/// process-wide state (cargo runs `#[test]`s in parallel within one
/// process, so racy env mutation is a real footgun here).
fn default_auto_dismiss_from(lazy: bool, env: Option<String>) -> bool {
    let trimmed = env.as_deref().map(str::trim);
    let lowered = trimmed.map(|s| s.to_ascii_lowercase());
    match lowered.as_deref() {
        Some("1" | "true" | "yes" | "on") => true,
        Some("0" | "false" | "no" | "off") => false,
        // Empty string and unknown values fall back to the lazy-vs-pinned
        // heuristic — a typo like `CEL_AUTO_DISMISS_OVERLAYS=maybe` should
        // NOT silently flip the default in either direction.
        _ => !lazy,
    }
}

impl BrowserAdapter {
    /// Lazy-discovery construction. Adapter starts unbound; `probe()`
    /// attempts `cel_cdp::connect_to_focused_app()` on each call and
    /// caches the result so subsequent ticks don't re-pay discovery
    /// cost. Right for ambient embedders (MCP server) where browsers
    /// come and go during a session.
    ///
    /// Auto-dismiss defaults OFF in this path so customer-facing MCP runs
    /// (Claude Code, Cursor) don't silently click through cookie banners
    /// the user might be reasoning about. Override via the
    /// `CEL_AUTO_DISMISS_OVERLAYS=1` env var or
    /// [`BrowserAdapter::with_auto_dismiss_overlays`].
    pub fn new() -> Self {
        Self {
            manifest: default_browser_manifest(),
            cdp_client: Mutex::new(None),
            pinned: false,
            connected: false,
            overlay_tick_counter: Mutex::new(0),
            auto_dismiss_overlays: default_auto_dismiss(true),
            overlay_detect_every: 3,
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
    ///
    /// Auto-dismiss defaults ON in this path so benchmark + eval runs don't
    /// burn vision calls reading cookie banners. Override via
    /// `CEL_AUTO_DISMISS_OVERLAYS=0` or
    /// [`BrowserAdapter::with_auto_dismiss_overlays`].
    pub fn with_cdp_client(client: Arc<CdpClient>) -> Self {
        Self {
            manifest: default_browser_manifest(),
            cdp_client: Mutex::new(Some(client)),
            pinned: true,
            connected: false,
            overlay_tick_counter: Mutex::new(0),
            auto_dismiss_overlays: default_auto_dismiss(false),
            overlay_detect_every: 3,
        }
    }

    /// Toggle auto-dismiss of cookie / consent / modal overlays detected
    /// during `get_context`. The construction-time default depends on
    /// constructor: [`BrowserAdapter::new`] defaults OFF (customer MCP),
    /// [`BrowserAdapter::with_cdp_client`] defaults ON (benchmarks).
    /// Both defaults can be overridden via the `CEL_AUTO_DISMISS_OVERLAYS`
    /// env var (truthy → ON, falsy → OFF).
    ///
    /// Call this builder to set the value explicitly — it overrides both
    /// the constructor default and the env var. Use `true` to force
    /// auto-dismiss on (e.g. in a benchmark runner explicitly opting in);
    /// use `false` when interacting WITH the banner IS the goal ("Click
    /// 'Accept all cookies' on this consent dialog"). Per-tick override
    /// is also available via the `dismiss_overlay` / `preserve_overlay`
    /// adapter actions in `cel_act`.
    #[must_use]
    pub fn with_auto_dismiss_overlays(mut self, enabled: bool) -> Self {
        self.auto_dismiss_overlays = enabled;
        self
    }

    /// Override the overlay-detection cadence. Defaults to every 3rd tick.
    /// Pass `1` to detect on every tick (expensive, ~5–20ms CDP eval per
    /// call) or a higher number to throttle further. Effective range is
    /// `1..=255`; anything outside is clamped.
    #[must_use]
    pub fn with_overlay_detect_every(mut self, every: u64) -> Self {
        self.overlay_detect_every = every.clamp(1, 255);
        self
    }

    /// Bind a CDP client post-construction.
    ///
    /// Always overwrites the current client — explicit binds from
    /// `bind_browser_cdp_url` must take precedence over whatever
    /// `probe()` may have discovered via ambient `connect_to_focused_app`
    /// before the bind was called. Without this, probe()'s lazy discovery
    /// can latch onto a stale or wrong-page connection first, and the
    /// `is_none()` guard would then block `bind_browser_cdp_url` from
    /// installing the correct client.
    pub async fn set_cdp_client(&self, client: Arc<CdpClient>) {
        let mut guard = self.cdp_client.lock().await;
        *guard = Some(client);
    }
}

/// Shared partial manifest for the browser-perception adapter pair, embedded
/// at compile time. Single source of truth for `app_patterns`, `platform`,
/// `context.truth_surface`, `context.confidence`, and `verification.truth_surface`
/// across the TS adapter (`adapters/browser/`) and this Rust adapter.
///
/// `cel_adapter_sdk::load_manifest` resolves the same file at runtime via the
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

    /// Phase 3 of ADR-unify-browser-ownership: exposes the concrete adapter
    /// so external code (e.g. tests) can downcast to BrowserAdapter and
    /// access inherent APIs. The main Phase 3 binding path goes through
    /// the trait-level `set_cdp_client` below, not via downcast.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    /// Phase 3 binding hook. Lets `Cortex::bind_browser_cdp_url` hand the
    /// CDP client of the just-launched Chromium (from `cel.ensureBrowser`)
    /// directly to this adapter, bypassing focus-based discovery that
    /// doesn't work for headless browsers.
    ///
    /// Delegates to the inherent `BrowserAdapter::set_cdp_client` which
    /// always overwrites (explicit bind always wins over probe() discovery).
    async fn set_cdp_client(&self, client: std::sync::Arc<cel_cdp::CdpClient>) {
        BrowserAdapter::set_cdp_client(self, client).await;
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
        let mut elements: Vec<ContextElement> = match cel_cdp::extract_page_content(&client).await {
            Ok(page) => page
                .interactive_elements
                .iter()
                .enumerate()
                .map(|(idx, dom)| dom_element_to_context_element(dom, idx))
                .collect(),
            Err(err) => {
                debug!("browser adapter: extract_page_content failed: {err}");
                return Ok(Vec::new());
            }
        };

        // Overlay handling — runs every `overlay_detect_every` ticks.
        // We bump the counter UP FRONT so a `dismiss_overlay` attempt
        // doesn't trigger the next tick to immediately re-run (giving
        // the page time to settle is the whole point of the cadence).
        let should_check_overlay = {
            let mut counter = self.overlay_tick_counter.lock().await;
            *counter = counter.wrapping_add(1);
            self.overlay_detect_every > 0 && *counter % self.overlay_detect_every == 0
        };
        if should_check_overlay {
            if let Some(overlay) = overlay_detector::detect_overlay(&client).await {
                debug!(
                    "browser adapter: detected overlay type={} cmp={:?} confidence={:.2}",
                    overlay.overlay_type, overlay.cmp_platform, overlay.confidence
                );
                // Tag elements that the overlay covers BEFORE attempting
                // dismissal — so even if dismiss fails this turn, the
                // planner sees which elements are obscured and won't
                // burn a step clicking through the banner.
                overlay_detector::tag_blocked_elements(&mut elements, &overlay);
                if self.auto_dismiss_overlays {
                    let result = overlay_detector::dismiss_overlay(
                        &client,
                        &overlay_detector::DismissOverlayOptions::default(),
                    )
                    .await;
                    if result.success {
                        // INFO not DEBUG — we want this visible in benchmark
                        // logs so we can verify the overlay detector is
                        // actually firing on real sites (2026-05-25 smoke
                        // raised this as an open question).
                        tracing::info!(
                            "browser adapter: auto-dismissed overlay type={} cmp={:?} method={:?} detail={:?}",
                            overlay.overlay_type, overlay.cmp_platform, result.method, result.detail
                        );
                        // Stamp the receipt on perception so the planner
                        // sees what was done this tick rather than
                        // wondering where the banner went.
                        if let Some(first) = elements.first_mut() {
                            first
                                .properties
                                .insert("_overlay_dismissed".into(), "true".into());
                            if let Some(method) = result.method {
                                first
                                    .properties
                                    .insert("_overlay_dismiss_method".into(), method);
                            }
                            if let Some(detail) = result.detail {
                                first
                                    .properties
                                    .insert("_overlay_dismiss_detail".into(), detail);
                            }
                        }
                    } else {
                        debug!(
                            "browser adapter: overlay detected but dismiss failed: {:?}",
                            result.detail
                        );
                    }
                }
            }
        }

        Ok(elements)
    }

    async fn execute(&self, action: &str, _params: Value) -> Result<ActionResult, AdapterError> {
        // Browser actions (cdp_eval, set_value, click) flow through the
        // canonical runner's CDP dispatch path — the runner already
        // routes `dom:*` element_ids through `cortex::build_click_js`
        // and friends. The adapter intentionally exposes no DOM actions
        // of its own to avoid two parallel dispatch paths going stale
        // at different rates.
        //
        // EXCEPTION: overlay management. The detection + dismissal
        // pipeline lives in this crate (`overlay_detector.rs`), so the
        // adapter is the right owner for the explicit
        // `dismiss_overlay` / `preserve_overlay` actions. The planner
        // dispatches them via `cel_act adapter_action: { adapter:
        // "browser", action: "dismiss_overlay" }` when it sees an
        // `_overlay_present=true` element and wants to deal with it
        // out-of-band of `get_context`'s tick-based auto-dismiss
        // (e.g., to force an early dismissal on the first turn instead
        // of waiting for tick N).
        //
        // The one canonical action that *would* also fit here is
        // `navigate` (since this adapter owns the browser CDP surface),
        // but it's intentionally left to the cortex's
        // `dispatch_navigate` fallback path — that path runs
        // cel_cdp::Page.navigate plus a readyState poll plus the shared
        // `CEL_DISMISS_OVERLAYS_JS` script, all of which would
        // otherwise have to be reimplemented here. Adding `navigate`
        // to this adapter's manifest would collide with the TS
        // `adapters/browser/` peer (which is what dispatch_navigate
        // prefers when both are active) — keep navigation in the cortex.
        match action {
            "dismiss_overlay" => {
                let client = {
                    let guard = self.cdp_client.lock().await;
                    match guard.as_ref() {
                        Some(c) => Arc::clone(c),
                        None => {
                            return Err(AdapterError::ExecutionFailed(
                                "dismiss_overlay: no CDP client bound — call bind_browser_cdp_url first".into(),
                            ));
                        }
                    }
                };
                let result = overlay_detector::dismiss_overlay(
                    &client,
                    &overlay_detector::DismissOverlayOptions::default(),
                )
                .await;
                // Carry the structured result via `data` — `ActionResult`
                // has only success/error/data slots and the planner reads
                // `data` for evidence in receipts. We also mirror the
                // human-readable summary onto `error` when failed so it
                // shows up in the planner's plain-text history.
                let payload = serde_json::json!({
                    "success": result.success,
                    "method": result.method,
                    "detail": result.detail,
                });
                Ok(if result.success {
                    ActionResult {
                        success: true,
                        error: None,
                        data: Some(payload),
                    }
                } else {
                    let summary = format!(
                        "overlay dismiss failed: method={:?} detail={:?}",
                        result.method, result.detail
                    );
                    ActionResult {
                        success: false,
                        error: Some(summary),
                        data: Some(payload),
                    }
                })
            }
            "preserve_overlay" => {
                // No-op confirmation action. The planner calls this to
                // signal "leave the banner alone this turn" — useful
                // for tasks where interacting with the banner IS the
                // goal. The adapter doesn't toggle its tick-based
                // auto-dismiss from this (config flag on construction
                // is the right place for that); instead the action
                // exists as a planner-facing affirmation that the next
                // turn's perception will still show the overlay.
                Ok(ActionResult {
                    success: true,
                    error: None,
                    data: Some(serde_json::json!({
                        "preserved": true,
                        "note": "tick auto-dismiss still governs subsequent ticks; pass with_auto_dismiss_overlays(false) at construction to disable entirely",
                    })),
                })
            }
            _ => Err(AdapterError::ExecutionFailed(format!(
                "browser adapter does not expose action {action:?} — use canonical runner CDP dispatch for click/set_value/cdp_eval, or pass 'dismiss_overlay' | 'preserve_overlay' for overlay management"
            ))),
        }
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
    use cel_cdp::DomElement;
    use cel_context::{ContextSource, ElementState};

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
        // (`cel_adapter_sdk::group_paired_manifests`) only pair them when both
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
            data_testid: None,
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

    // ───── Auto-dismiss env-gate resolution table ──────────────────────
    //
    // The lazy/pinned asymmetry exists because the same crate ships to two
    // very different audiences:
    //   - lazy (BrowserAdapter::new) → MCP server → customer-facing
    //   - pinned (with_cdp_client)   → eval / benchmark harness
    // Pinning the resolution here so a future refactor of the env-string
    // table can't silently flip either default for the wrong audience.

    #[test]
    fn auto_dismiss_unset_defaults_off_in_lazy_on_in_pinned() {
        assert!(!default_auto_dismiss_from(true, None));
        assert!(default_auto_dismiss_from(false, None));
    }

    #[test]
    fn auto_dismiss_truthy_env_forces_on_in_both_paths() {
        for v in [
            "1", "true", "TRUE", "True", "yes", "YES", "on", "On", "  true  ",
        ] {
            assert!(
                default_auto_dismiss_from(true, Some(v.into())),
                "expected lazy to be ON for env={v:?}",
            );
            assert!(
                default_auto_dismiss_from(false, Some(v.into())),
                "expected pinned to be ON for env={v:?}",
            );
        }
    }

    #[test]
    fn auto_dismiss_falsy_env_forces_off_in_both_paths() {
        for v in [
            "0", "false", "FALSE", "False", "no", "NO", "off", "Off", "  0  ",
        ] {
            assert!(
                !default_auto_dismiss_from(true, Some(v.into())),
                "expected lazy to be OFF for env={v:?}",
            );
            assert!(
                !default_auto_dismiss_from(false, Some(v.into())),
                "expected pinned to be OFF for env={v:?}",
            );
        }
    }

    #[test]
    fn auto_dismiss_unknown_env_value_falls_through_to_lazy_heuristic() {
        // Typos and unknown strings should NOT silently flip behaviour —
        // they fall back to the lazy-vs-pinned default. This keeps a stray
        // `CEL_AUTO_DISMISS_OVERLAYS=maybe` from surprising customer MCP
        // runs with auto-dismiss on (or vice-versa for benchmarks).
        assert!(!default_auto_dismiss_from(true, Some("maybe".into())));
        assert!(default_auto_dismiss_from(false, Some("maybe".into())));
        // Empty string is the same as unset for our purposes.
        assert!(!default_auto_dismiss_from(true, Some(String::new())));
        assert!(default_auto_dismiss_from(false, Some(String::new())));
    }
}
