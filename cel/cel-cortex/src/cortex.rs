//! Cortex — Always-on perception engine for CEL.
//!
//! The human brain doesn't request visual input. The retina is always firing.
//! The visual cortex is always processing. When you reach for a cup, you
//! already know where it is. Perception is decoupled from action.
//!
//! The Cortex maintains a continuously-updated mental model via a background
//! tick loop. It combines:
//! - **Polling**: ContextWatchdog diffs consecutive snapshots (200ms)
//! - **Push**: AXObserver delivers accessibility events in real-time
//!
//! Both event streams are merged each tick. Consumers read FROM the model —
//! they never trigger new observations. The model is always fresh.

use crate::anomaly::{detect_anomalies_from_context, detect_anomalies_from_events};
use crate::daemon_bridge::DaemonBridge;
use crate::differ::{diff_contexts, is_diff_significant, ContextDiff};
use crate::model::*;
use crate::skeleton::{is_skeleton_screen, skeleton_wait_ms};
use cellar_types::event::{Event, EventKind, EventSource};

#[cfg(target_os = "macos")]
use cel_accessibility::ElementRole;
use cel_accessibility::{AccessibilityEvent, AccessibilityTree};
use cel_context::{CelEvent, ContextMerger, ContextWatchdog, ScreenContext};
use cel_contracts::{EffectExpectation, PlannedAction};
#[cfg(target_os = "macos")]
use cel_input::InputError;
use cel_input::{create_controller, MouseButton};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};
use tracing::{debug, trace, warn};

// The Cortex implementation is split across focused submodules, re-exported so
// `Cortex` and every `cel_cortex::*` path stay identical:
//   tick     — background perception loop + event→daemon-bridge forwarding
//   cdp      — CDP binding/eval + DOM/key dispatch primitives
//   dispatch — `execute()` and per-action-kind routing
//   targets  — target validation + element/URL resolution
//   focus    — app-focus management + AX-on-browser refusal
//   numbers  — Apple Numbers document bootstrap (macOS)
// This file retains the `Cortex` struct, `CortexError`, lifecycle/builders, and
// shared helpers.
mod cdp;
mod dispatch;
mod focus;
mod numbers;
mod receipt;
pub use receipt::{clear_run_id, current_run_id, set_run_id};
mod targets;
#[cfg(test)]
mod tests;
mod tick;

use targets::platform_matches;
pub use targets::TargetValidation;

/// Compute a simple fingerprint of a ScreenContext for change detection.
pub fn context_fingerprint(ctx: &ScreenContext) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ctx.app.hash(&mut hasher);
    ctx.window.hash(&mut hasher);
    ctx.elements.len().hash(&mut hasher);
    for el in ctx.elements.iter().take(10) {
        el.id.hash(&mut hasher);
        el.element_type.hash(&mut hasher);
    }
    hasher.finish()
}

/// Get current time in milliseconds since epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn command_status_with_timeout(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> Option<std::process::ExitStatus> {
    let mut child = command.spawn().ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn command_output_with_timeout(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> Option<std::process::Output> {
    use std::process::Stdio;

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Install `client` as the active CDP client in `slot` and push the same
/// client to every adapter in `adapters` via the adapter `set_cdp_client` hook.
///
/// THE single runtime writer of the cortex's CDP slot. Everything that binds a
/// client at runtime funnels here: `Cortex::bind_cdp_client` (and through it
/// `bind_browser_cdp_url` and the per-action `cdp_client_or_ambient` fallback)
/// plus the `boot()` tick-loop ambient auto-bind. As a result the cortex's own
/// dispatch path and its in-process adapters' perception path can never end up
/// on different CDP connections — there is no "which client is bound?"
/// ambiguity at runtime. Defined here in the parent module (rather than as a
/// method) so the tick loop, which owns `Arc` clones of the slot + adapters but
/// not `self`, can reuse the identical propagation via `use super::*`.
///
/// Adapters that don't consume CDP get the default no-op `set_cdp_client`, so
/// propagating to the full adapter set is cheap. The std-`Mutex` guard is
/// dropped before the `.await`, so this never holds a lock across a suspend
/// point.
async fn install_cdp_client(
    slot: &Arc<std::sync::Mutex<Option<Arc<cel_cdp::CdpClient>>>>,
    adapters: &Arc<RwLock<Vec<crate::adapter::RegisteredAdapter>>>,
    client: Arc<cel_cdp::CdpClient>,
) {
    // Poison-tolerant: this is on the long-running tick loop's auto-bind path,
    // so recover the guard rather than panic the loop on the (practically
    // impossible — only trivial ops run under this lock) poisoned case.
    *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(client.clone());
    let guard = adapters.read().await;
    for adapter in guard.iter() {
        adapter.driver.set_cdp_client(client.clone()).await;
    }
}

/// The Cortex perception engine.
///
/// Maintains a continuously-updated `MentalModel` via a background tick loop.
/// The model is wrapped in `Arc<RwLock>` for thread-safe concurrent reads.
pub struct Cortex {
    /// Unique instance ID.
    pub id: String,

    /// The shared mental model — always current.
    model: Arc<RwLock<MentalModel>>,

    /// Whether the cortex is running.
    running: Arc<AtomicBool>,

    /// Tick interval in milliseconds.
    tick_ms: u64,

    /// Handle to the background tick task.
    task_handle: Option<tokio::task::JoinHandle<()>>,

    /// Registered adapters — shared between tick loop and execute dispatch.
    adapters: Arc<RwLock<Vec<crate::adapter::RegisteredAdapter>>>,

    /// Optional audio capture backend. When set, the tick loop drains transcripts
    /// each tick and injects them into `ScreenContext::transcripts`.
    audio_capture: Option<Arc<std::sync::Mutex<Box<dyn cel_audio::AudioCapture>>>>,
    /// Config used to start `audio_capture` in boot().
    audio_config: Option<cel_audio::AudioConfig>,
    /// Optional bound CDP client. When set, `execute()` routes any action whose
    /// target_id starts with `dom:` through CDP (Input.dispatchKeyEvent +
    /// Runtime.evaluate) instead of native macOS input. Critical for safe
    /// browser-only execution: native input drivers send keystrokes to the
    /// **focused application**, which is whatever the user has on top — not
    /// necessarily the CDP browser cellar is reading from. Routing browser
    /// actions through CDP guarantees they land in the right tab regardless
    /// of focus.
    /// Wrapped in `Arc<…>` so callers (e.g. the merger's CDP-screenshot
    /// vision fallback closure) can hold a separate clone that races
    /// the lock independently of the cortex's own access. The inner
    /// std::sync::Mutex stays — async paths use `.lock().unwrap()`
    /// only after cloning the Arc<CdpClient>, so contention is brief.
    cdp_client: Arc<std::sync::Mutex<Option<Arc<cel_cdp::CdpClient>>>>,
    /// SAFETY GATE. Default `false` — execute() refuses any action that
    /// would dispatch through native macOS input (mouse / keyboard / app
    /// activation / AX) unless this is explicitly opted into via
    /// `with_native_input_unsafe()`. Browser-only actions still work via
    /// the `cdp_client` path. Set to `true` ONLY in the production cellar
    /// app/MCP server where the user's intent is to drive their actual
    /// machine — never in eval, never in CI.
    allow_native_input: bool,

    /// Native-input focus policy (WS1, Peekaboo-parity). `Foreground`
    /// (default) activates the target app before posting session-wide input;
    /// `Background` posts CGEvents directly to the target PID via
    /// `cel_input::background` without activating it, so the user keeps
    /// focus. The dispatcher falls back to `Foreground` when no target PID
    /// resolves. Opt in with `with_background_input()` / `with_focus_mode()`.
    /// Interior-mutable (`AtomicU8` of `FocusMode as u8`) so the MCP layer can
    /// flip it per `cel_act` call on the long-lived singleton cortex.
    focus_mode: std::sync::atomic::AtomicU8,

    // ─── Liveness state (Phase 1) ─────────────────────────────────────────
    // These atomics mirror the tick loop's progress so callers can poll
    // liveness without taking the tokio RwLock. Writers = tick loop; readers
    // = anyone (napi, tests, monitoring).
    /// Total successful ticks since boot. Mirrors `MentalModel.cycle_count`
    /// but is sync-readable.
    tick_count: Arc<AtomicU64>,

    /// Wall-clock ms (since UNIX epoch) of the most recent successful tick.
    /// 0 until the first tick writes to the model.
    last_tick_ms: Arc<AtomicU64>,

    /// Count of `refresh_now` calls that timed out waiting for a tick. Does
    /// not include unrelated lateness — this is specifically "a caller asked
    /// for a fresh tick and didn't get one in time".
    stalled_ticks: Arc<AtomicU64>,

    /// Notify handle the tick loop waits on alongside its interval. Wake
    /// via `refresh_now()` to trigger an out-of-band tick.
    refresh_notify: Arc<Notify>,

    /// Most recent `activate_app` target. Set when an ActivateApp action
    /// succeeds. Read by the native-input dispatch path to re-raise the
    /// expected target app if another app stole frontmost between the
    /// activation and the keystroke (Terminal, the editor the eval was
    /// launched from, a notification, a system dialog, …).
    ///
    /// Wrapped in `Arc<Mutex<…>>` rather than kept behind the model
    /// `RwLock` because native-input dispatch is sync-from-blocking-task
    /// and must not contend with the tick loop.
    last_activated_app: Arc<std::sync::Mutex<Option<String>>>,

    /// Optional bridge to the daemon's event bus. When set, the tick loop
    /// forwards every Cortex-observed stream — AX (app/window/element), CDP
    /// (`url_changed`, `page_loaded`), network connections, audio activity, and
    /// keyboard/pointer input — so the daemon's rule matcher can react to them.
    daemon_bridge: Option<Arc<dyn DaemonBridge>>,

    /// Optional network monitor. When set, the tick loop drains newly observed
    /// TCP/UDP connections each poll and forwards `NetworkConnectionOpened` to
    /// the daemon bridge. Mirrors `audio_capture`'s lifecycle (started in
    /// `boot()`, stopped in `shutdown()`).
    network_monitor: Option<Arc<std::sync::Mutex<Box<dyn cel_network::NetworkMonitor>>>>,

    /// When true *and* a daemon bridge is set, forward audio transcript CONTENT
    /// (`AudioTranscript`) to the daemon. Off by default — transcripts are
    /// privacy-sensitive. `AudioCaptureStarted`/`Stopped` activity events are
    /// forwarded regardless of this flag.
    forward_audio_transcripts: bool,

    /// Optional input capture (keyboard/mouse observation). When set, the tick
    /// loop drains observed events and forwards `KeyboardInput`/`Pointer*`
    /// events to the daemon bridge. Privacy-sensitive — see
    /// `forward_input_content` for keystroke text.
    input_capture: Option<Arc<std::sync::Mutex<Box<dyn cel_input::InputCapture>>>>,

    /// When true *and* a daemon bridge is set, attach typed-character CONTENT
    /// to forwarded `KeyboardInput` events. Off by default — keystroke content
    /// is highly sensitive. Keycodes and pointer events forward regardless.
    forward_input_content: bool,
}

impl Cortex {
    /// Create a new Cortex instance (does not start it — call `boot()`).
    ///
    /// SAFE BY DEFAULT: native macOS input is disabled. The new Cortex can
    /// only execute via a CDP client (set via `with_cdp_client`) or pure
    /// data actions (Wait, Done, Fail, Extract). To enable native input
    /// drivers (the original behaviour, used by the production cellar
    /// app/worker), call `.with_native_input_unsafe()`.
    pub fn new(id: String) -> Self {
        Self {
            id,
            model: Arc::new(RwLock::new(MentalModel::default())),
            running: Arc::new(AtomicBool::new(false)),
            tick_ms: TICK_INTERVAL_MS,
            task_handle: None,
            adapters: Arc::new(RwLock::new(Vec::new())),
            audio_capture: None,
            audio_config: None,
            cdp_client: Arc::new(std::sync::Mutex::new(None)),
            allow_native_input: false,
            focus_mode: std::sync::atomic::AtomicU8::new(
                cel_contracts::actions::FocusMode::Foreground as u8,
            ),
            tick_count: Arc::new(AtomicU64::new(0)),
            last_tick_ms: Arc::new(AtomicU64::new(0)),
            stalled_ticks: Arc::new(AtomicU64::new(0)),
            refresh_notify: Arc::new(Notify::new()),
            last_activated_app: Arc::new(std::sync::Mutex::new(None)),
            daemon_bridge: None,
            network_monitor: None,
            forward_audio_transcripts: false,
            input_capture: None,
            forward_input_content: false,
        }
    }

    /// Bind a CDP client. When set, browser-targeted actions (target_id
    /// starts with `dom:`) are dispatched through CDP — never through
    /// native macOS input. See the `cdp_client` field for why this matters.
    pub fn with_cdp_client(mut self, client: Arc<cel_cdp::CdpClient>) -> Self {
        self.cdp_client = Arc::new(std::sync::Mutex::new(Some(client)));
        self
    }

    /// Enable native macOS input dispatch (CGEventCreateKeyboardEvent,
    /// AXUIElementPerformAction, app activation, etc.).
    ///
    /// **DANGER**: native input drivers target whichever app is currently
    /// frontmost on the user's machine. If the user has a chat window or
    /// editor focused when an action fires, the keystrokes land there.
    /// Past incident: an eval typed "engineering" into the user's open
    /// Claude.ai tab because the headless eval browser wasn't focused.
    ///
    /// Use only in:
    ///   * the production cellar Tauri app
    ///   * cellar-worker (server context with a dedicated session)
    ///   * the MCP server (when the user explicitly invoked it)
    ///
    /// NEVER in:
    ///   * eval scenarios
    ///   * CI test runners
    ///   * any code path that doesn't have explicit user intent to drive
    ///     the local machine
    pub fn with_native_input_unsafe(mut self) -> Self {
        self.allow_native_input = true;
        self
    }

    /// Set the native-input focus policy (WS1). See
    /// [`cel_contracts::actions::FocusMode`]. `Background` requires
    /// `with_native_input_unsafe()` too — it's still native input, just
    /// delivered to a PID instead of the frontmost app.
    pub fn with_focus_mode(self, mode: cel_contracts::actions::FocusMode) -> Self {
        self.set_focus_mode(mode);
        self
    }

    /// Shorthand for [`Self::with_focus_mode`] with `Background`: post native
    /// input to the target PID without bringing the app to the foreground.
    pub fn with_background_input(self) -> Self {
        self.set_focus_mode(cel_contracts::actions::FocusMode::Background);
        self
    }

    /// Set the native-input focus policy at runtime (interior-mutable). The
    /// MCP `cel_act` `focus_mode` param uses this to flip the long-lived
    /// singleton cortex per call without rebuilding it. WS1.
    pub fn set_focus_mode(&self, mode: cel_contracts::actions::FocusMode) {
        self.focus_mode
            .store(mode as u8, std::sync::atomic::Ordering::Relaxed);
    }

    /// Bool convenience for the napi/MCP layer — toggles Background vs
    /// Foreground without naming the `FocusMode` enum across the FFI boundary.
    pub fn set_background_input(&self, on: bool) {
        self.set_focus_mode(if on {
            cel_contracts::actions::FocusMode::Background
        } else {
            cel_contracts::actions::FocusMode::Foreground
        });
    }

    /// Current native-input focus policy.
    pub fn focus_mode(&self) -> cel_contracts::actions::FocusMode {
        if self.focus_mode.load(std::sync::atomic::Ordering::Relaxed)
            == cel_contracts::actions::FocusMode::Background as u8
        {
            cel_contracts::actions::FocusMode::Background
        } else {
            cel_contracts::actions::FocusMode::Foreground
        }
    }

    /// Attach a daemon bridge so the tick loop can forward events to the
    /// daemon's event bus (call before `boot()`). The Tauri app injects its
    /// IPC client here; eval and test harnesses leave it unset.
    pub fn with_daemon_bridge(mut self, bridge: Arc<dyn DaemonBridge>) -> Self {
        self.daemon_bridge = Some(bridge);
        self
    }

    /// Attach an audio capture backend (call before `boot()`).
    /// Pass a config to override source, sample rate, or other capture settings.
    /// The cortex will start the backend in `boot()` and drain transcripts each tick.
    pub fn with_audio(
        mut self,
        capture: Box<dyn cel_audio::AudioCapture>,
        config: cel_audio::AudioConfig,
    ) -> Self {
        self.audio_capture = Some(Arc::new(std::sync::Mutex::new(capture)));
        self.audio_config = Some(config);
        self
    }

    /// Attach a network monitor (call before `boot()`). The cortex starts it in
    /// `boot()`, drains newly observed connections each poll, and forwards
    /// `NetworkConnectionOpened` to the daemon bridge. Construct the platform
    /// default via `cel_network::create_monitor()`.
    pub fn with_network(mut self, monitor: Box<dyn cel_network::NetworkMonitor>) -> Self {
        self.network_monitor = Some(Arc::new(std::sync::Mutex::new(monitor)));
        self
    }

    /// Enable forwarding of audio transcript CONTENT (`AudioTranscript`) to the
    /// daemon bridge. Off by default (privacy). Capture start/stop activity is
    /// always forwarded when audio is configured and a bridge is set.
    pub fn with_audio_transcript_forwarding(mut self, enable: bool) -> Self {
        self.forward_audio_transcripts = enable;
        self
    }

    /// Attach an input capture backend (call before `boot()`). The cortex starts
    /// it in `boot()` and forwards observed keyboard/pointer events to the daemon
    /// bridge. Construct via `cel_input::create_capture(capture_chars)`.
    pub fn with_input_capture(mut self, capture: Box<dyn cel_input::InputCapture>) -> Self {
        self.input_capture = Some(Arc::new(std::sync::Mutex::new(capture)));
        self
    }

    /// Enable forwarding of typed-character CONTENT on `KeyboardInput` events.
    /// Off by default (privacy). Keycodes and pointer events forward regardless.
    pub fn with_input_content_forwarding(mut self, enable: bool) -> Self {
        self.forward_input_content = enable;
        self
    }

    /// Create a new Cortex with a custom tick interval.
    pub fn with_tick_ms(mut self, tick_ms: u64) -> Self {
        self.tick_ms = tick_ms;
        self
    }

    /// Get a handle to the shared mental model for concurrent reads.
    pub fn model(&self) -> Arc<RwLock<MentalModel>> {
        Arc::clone(&self.model)
    }

    /// Spawn an isolated, browser-only Cortex suitable for eval scenarios.
    ///
    /// Uses `StubAccessibility` (no platform AX) so the runtime stays portable
    /// and doesn't pollute the user's focus/AX state. Browser fixtures are
    /// expected to be driven via `cel-cdp` separately; the Cortex perception
    /// path will surface CDP-enriched context when available.
    ///
    /// Returns a fresh Cortex + the merger you'd hand to `boot()`. The caller
    /// is responsible for booting and dropping it per scenario.
    pub fn isolated(id: impl Into<String>) -> (Self, cel_context::ContextMerger) {
        let cortex = Self::new(id.into());
        let merger = cel_context::ContextMerger::new();
        (cortex, merger)
    }

    /// Same as `isolated`, but binds a CDP client up-front. Use this for
    /// safe browser-only eval runs: every dom:* action is guaranteed to
    /// dispatch through CDP rather than native input.
    pub fn isolated_with_cdp(
        id: impl Into<String>,
        client: Arc<cel_cdp::CdpClient>,
    ) -> (Self, cel_context::ContextMerger) {
        let (cortex, merger) = Self::isolated(id);
        let cortex = cortex.with_cdp_client(client);
        (cortex, merger)
    }

    /// Register an adapter driver. Must be called BEFORE boot().
    ///
    /// Adapters whose manifest declares a `platform:` list that doesn't
    /// include the current OS are silently skipped at registration —
    /// e.g. `adapters/mail` declares `platform: ["macos"]` because it
    /// drives Mail.app via AppleScript; registering it on Linux just
    /// produces "Adapter not available" warnings every cortex tick
    /// (the adapter spawn fails: `target/release/adapter-mail: No such
    /// file or directory`). Skipping at register time avoids 40+ noise
    /// lines per scenario in eval logs, and shortens cortex boot by
    /// not even attempting the spawn.
    ///
    /// An empty `platform` list is treated as "all platforms", same as
    /// before — most workspace-local adapters (browser, browser-rs)
    /// leave the list empty.
    pub fn register_adapter(&mut self, driver: Box<dyn crate::adapter::AdapterDriver>) {
        let manifest = driver.manifest();
        if !manifest.platform.is_empty() && !manifest.platform.iter().any(|p| platform_matches(p)) {
            tracing::debug!(
                adapter = %manifest.name,
                declared_platforms = ?manifest.platform,
                current_os = %std::env::consts::OS,
                "Skipping adapter — current OS not in manifest.platform"
            );
            return;
        }
        // Safe: register_adapter is called before boot(), so no contention on the lock.
        let mut guard = self
            .adapters
            .try_write()
            .expect("adapters lock not contested before boot");
        guard.push(crate::adapter::RegisteredAdapter::new(driver));
    }

    /// List the names of currently registered adapters. Used by the
    /// runner to (a) tell the planner which `Custom { adapter }` values
    /// are valid and (b) reject hallucinated adapter names before
    /// dispatch. Takes the adapters RwLock in read mode — cheap but
    /// allocates a Vec of owned strings since the guard can't outlive
    /// the call.
    pub async fn registered_adapter_names(&self) -> Vec<String> {
        let guard = self.adapters.read().await;
        guard
            .iter()
            .map(|a| a.driver.manifest().name.clone())
            .collect()
    }

    /// Snapshot the manifests of all currently-`Active` adapters. Used by
    /// the canonical runner to surface available app-specific ops as
    /// structured `PlanningView::adapter_actions`, with
    /// `PlanningView::adapter_actions_prompt` kept as a transitional
    /// prompt-only fallback. Inactive / Error-state adapters are skipped —
    /// telling the planner about an op that will fail "adapter not active"
    /// is worse than silence.
    pub async fn active_adapter_manifests(&self) -> Vec<crate::adapter::AdapterManifest> {
        let guard = self.adapters.read().await;
        guard
            .iter()
            .filter(|a| a.state == crate::adapter::AdapterState::Active)
            .map(|a| a.driver.manifest().clone())
            .collect()
    }

    /// Closing-gap fill: aggregate `AdapterFactRef`s from every
    /// **active** registered adapter for the current goal + perception.
    /// Each adapter's `facts_for_planning_view` impl decides what's
    /// relevant; the cortex just unions the results without reranking.
    /// Inactive adapters are skipped (we don't poke deactivated apps
    /// for facts).
    ///
    /// Per-turn cost = N active adapters × adapter's facts call.
    /// Default `facts_for_planning_view` returns empty in O(1), so
    /// adapters that haven't opted in are free.
    pub async fn collect_adapter_facts_for_planning_view(
        &self,
        goal: &str,
        context: &cel_context::ScreenContext,
    ) -> Vec<cel_contracts::AdapterFactRef> {
        let guard = self.adapters.read().await;
        let mut out = Vec::new();
        for adapter in guard.iter() {
            if adapter.state != crate::adapter::AdapterState::Active {
                continue;
            }
            let mut facts = adapter.driver.facts_for_planning_view(goal, context).await;
            out.append(&mut facts);
        }
        out
    }

    /// Is native macOS input unlocked? Used by the planner to steer
    /// away from ax_action / key / type when the cortex is in
    /// browser-only safety mode.
    pub fn native_input_allowed(&self) -> bool {
        self.allow_native_input
    }

    /// Is the cortex currently running?
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Total successful ticks since boot. Matches `MentalModel.cycle_count`
    /// but readable without awaiting the tokio RwLock.
    pub fn tick_count(&self) -> u64 {
        self.tick_count.load(Ordering::Relaxed)
    }

    /// Number of `refresh_now` calls that timed out waiting for a tick to
    /// fire. Useful as a health signal — a stalled merger will drive this
    /// up.
    pub fn stalled_ticks(&self) -> u64 {
        self.stalled_ticks.load(Ordering::Relaxed)
    }

    /// Age (in ms) of the most recent successful tick, or `None` if no tick
    /// has fired yet. Derived from wall-clock time, so it keeps increasing
    /// even if the tick loop is stalled — that's the whole point.
    pub fn last_tick_age_ms(&self) -> Option<u64> {
        let last = self.last_tick_ms.load(Ordering::Relaxed);
        if last == 0 {
            return None;
        }
        Some(now_ms().saturating_sub(last))
    }

    /// Shutdown the cortex — stops the background loop.
    pub fn shutdown(&self) {
        if !self.running.load(Ordering::Relaxed) {
            return;
        }
        self.running.store(false, Ordering::Relaxed);
        if let Some(ref audio) = self.audio_capture {
            let _ = audio.lock().unwrap_or_else(|p| p.into_inner()).stop();
            // Activity event — safe to forward always (no audio content).
            if let Some(ref bridge) = self.daemon_bridge {
                bridge.forward(Event::now(
                    EventSource::CortexAudio,
                    EventKind::AudioCaptureStopped,
                ));
            }
        }
        if let Some(ref net) = self.network_monitor {
            let _ = net.lock().unwrap_or_else(|p| p.into_inner()).stop();
        }
        if let Some(ref input) = self.input_capture {
            let _ = input.lock().unwrap_or_else(|p| p.into_inner()).stop();
        }
        debug!(cortex_id = %self.id, "Cortex shutdown requested");
    }

    /// Notify the cortex that an action was taken.
    /// Resets idle tracking so the next tick treats events as post-action feedback.
    pub async fn notify_action(&self, _action: &str) {
        let mut m = self.model.write().await;
        m.temporal.idle_since = None;
        m.temporal.stagnant_cycles = 0;
    }

    /// Report a consecutive action failure (triggers vision_needed after 2).
    pub async fn report_action_failure(&self) {
        let mut m = self.model.write().await;
        m.vision_needed = true;
    }

    /// Report a successful action (resets failure counter).
    pub async fn report_action_success(&self) {
        let mut m = self.model.write().await;
        m.vision_needed = false;
    }

    /// Consume anomalies from the queue (drains them).
    pub async fn consume_anomalies(&self) -> Vec<Anomaly> {
        let mut m = self.model.write().await;
        m.anomaly_queue.drain(..).collect()
    }
}

/// Cortex errors.
#[derive(Debug, thiserror::Error)]
pub enum CortexError {
    #[error("Cortex \"{0}\" is already running")]
    AlreadyRunning(String),
    #[error("Cortex \"{0}\" is not running")]
    NotRunning(String),
    #[error("Execution failed: {0}")]
    ExecutionFailed(String),
    #[error("Refresh timed out after {elapsed_ms}ms — tick loop may be stalled")]
    RefreshTimeout { elapsed_ms: u64 },
    #[error(
        "URL wait timed out after {elapsed_ms}ms — expected \"{expected}\", \
         last observed \"{observed:?}\""
    )]
    WaitForUrlTimeout {
        expected: String,
        observed: Option<String>,
        elapsed_ms: u64,
    },
}
