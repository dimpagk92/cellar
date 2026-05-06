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
use crate::differ::{diff_contexts, is_diff_significant, ContextDiff};
use crate::model::*;
use crate::skeleton::{is_skeleton_screen, skeleton_wait_ms};

use cel_accessibility::AccessibilityTree;
#[cfg(target_os = "macos")]
use cel_accessibility::ElementRole;
use cel_context::{CelEvent, ContextMerger, ContextWatchdog, ScreenContext};
#[cfg(target_os = "macos")]
use cel_input::InputError;
use cel_input::{create_controller, MouseButton};
use cel_contracts::PlannedAction;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};
use tracing::{debug, trace, warn};

#[cfg(target_os = "macos")]
const NUMBERS_DOCUMENT_BOOTSTRAP_CANDIDATES: &[&str] = &[
    "/Applications/Numbers Creator Studio.app/Contents/Resources/SampleDocument.numbers",
    "/Applications/Numbers.app/Contents/Resources/SampleDocument.numbers",
];

#[cfg(target_os = "macos")]
const NUMBERS_BLANK_TEMPLATE_CANDIDATES: &[&str] = &[
    "/Applications/Numbers Creator Studio.app/Contents/SharedSupport/Templates/Blank/Traditional.nmbtemplate",
    "/Applications/Numbers.app/Contents/SharedSupport/Templates/Blank/Traditional.nmbtemplate",
];

/// Check if a CelEvent is significant (triggers full context read).
fn is_significant_event(event: &CelEvent) -> bool {
    matches!(
        event,
        CelEvent::TreeChanged { .. }
            | CelEvent::ValueChanged { .. }
            | CelEvent::WindowCreated { .. }
            | CelEvent::SheetCreated
            | CelEvent::LayoutChanged
    )
}

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

/// Convert a raw ContextDiff to a PerceptionDiff summary.
fn to_perception_diff(diff: &ContextDiff) -> PerceptionDiff {
    PerceptionDiff {
        added_count: diff.added.len(),
        removed_count: diff.removed.len(),
        changed_count: diff.changed.len(),
        unchanged_count: diff.unchanged_count,
        added_labels: diff
            .added
            .iter()
            .take(10)
            .map(|el| el.label.clone().unwrap_or_else(|| el.id.clone()))
            .collect(),
        changed_labels: diff
            .changed
            .iter()
            .take(10)
            .map(|c| {
                c.element
                    .label
                    .clone()
                    .unwrap_or_else(|| c.element.id.clone())
            })
            .collect(),
    }
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
    cdp_client: Option<Arc<cel_cdp::CdpClient>>,
    /// SAFETY GATE. Default `false` — execute() refuses any action that
    /// would dispatch through native macOS input (mouse / keyboard / app
    /// activation / AX) unless this is explicitly opted into via
    /// `with_native_input_unsafe()`. Browser-only actions still work via
    /// the `cdp_client` path. Set to `true` ONLY in the production cellar
    /// app/MCP server where the user's intent is to drive their actual
    /// machine — never in eval, never in CI.
    allow_native_input: bool,

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
            cdp_client: None,
            allow_native_input: false,
            tick_count: Arc::new(AtomicU64::new(0)),
            last_tick_ms: Arc::new(AtomicU64::new(0)),
            stalled_ticks: Arc::new(AtomicU64::new(0)),
            refresh_notify: Arc::new(Notify::new()),
            last_activated_app: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Bind a CDP client. When set, browser-targeted actions (target_id
    /// starts with `dom:`) are dispatched through CDP — never through
    /// native macOS input. See the `cdp_client` field for why this matters.
    pub fn with_cdp_client(mut self, client: Arc<cel_cdp::CdpClient>) -> Self {
        self.cdp_client = Some(client);
        self
    }

    /// Mutate-style binding for CDP client (used after construction).
    pub fn set_cdp_client(&mut self, client: Arc<cel_cdp::CdpClient>) {
        self.cdp_client = Some(client);
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
        let stub: Box<dyn cel_accessibility::AccessibilityTree> =
            Box::new(cel_accessibility::StubAccessibility);
        let merger = cel_context::ContextMerger::new(stub);
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
    pub fn register_adapter(&mut self, driver: Box<dyn crate::adapter::AdapterDriver>) {
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

    /// Is a CDP client bound? Used by the canonical runner to tell
    /// the planner whether `cdp_eval` / `navigate` will actually
    /// dispatch somewhere vs be blind.
    pub fn has_cdp_client(&self) -> bool {
        self.cdp_client.is_some()
    }

    /// Is native macOS input unlocked? Used by the planner to steer
    /// away from ax_action / key / type when the cortex is in
    /// browser-only safety mode.
    pub fn native_input_allowed(&self) -> bool {
        self.allow_native_input
    }

    /// Fetch the current URL of the CDP-bound page (if any). Used by
    /// the canonical runner to tell the planner whether it's already
    /// on the right page before emitting `navigate`. Returns None
    /// when there is no CDP client, the client is unreachable, or
    /// the bound page is not a URL page (about:blank, devtools, …).
    pub async fn cdp_current_url(&self) -> Option<String> {
        let client = self.cdp_client.as_ref()?;
        client.get_url().await.ok()
    }

    /// Is the cortex currently running?
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Boot the cortex — starts the background perception loop.
    ///
    /// Takes ownership of:
    /// - `merger`: provides context from the accessibility tree and other streams
    /// - `observer`: AXObserver-enabled accessibility tree for push-based events
    ///
    /// Both are moved into the background task.
    pub async fn boot(
        &mut self,
        mut merger: ContextMerger,
        mut observer: Box<dyn AccessibilityTree>,
    ) -> Result<(), CortexError> {
        if self.running.load(Ordering::Relaxed) {
            return Err(CortexError::AlreadyRunning(self.id.clone()));
        }

        let boot_time = now_ms();
        self.running.store(true, Ordering::Relaxed);

        // Start AXObserver for push-based events
        if let Err(e) = observer.start_observing() {
            warn!(cortex_id = %self.id, "AXObserver start failed (polling-only mode): {}", e);
        }

        // Start audio capture if configured
        if let Some(ref audio) = self.audio_capture {
            let config = self.audio_config.clone().unwrap_or_default();
            if let Err(e) = audio
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .start(config)
            {
                warn!(cortex_id = %self.id, "Audio capture start failed: {}", e);
            }
        }

        // Initial context capture — bootstrap the mental model
        let initial_context = merger.get_context();
        let expected_app = initial_context.app.clone();

        // Find initial focused element
        let focused = initial_context
            .elements
            .iter()
            .find(|el| el.state.focused)
            .map(|el| FocusedElement {
                id: el.id.clone(),
                label: el.label.clone(),
            });

        // Initialize element tracking
        let mut element_seen_count = HashMap::new();
        let mut element_last_seen = HashSet::new();
        for el in &initial_context.elements {
            element_seen_count.insert(el.id.clone(), 1u32);
            element_last_seen.insert(el.id.clone());
        }

        // Update the model
        {
            let mut model = self.model.write().await;
            model.current_context = initial_context;
            model.focused_element = focused;
            model.confidence = 1.0;
            model.refresh_derived(now_ms(), None, None);
        }

        debug!(cortex_id = %self.id, app = %expected_app, "Cortex booted");

        // Spawn the background tick loop
        let model = Arc::clone(&self.model);
        let running = Arc::clone(&self.running);
        let tick_ms = self.tick_ms;
        let cortex_id = self.id.clone();
        let adapters = Arc::clone(&self.adapters);
        let audio_capture = self.audio_capture.clone();
        let tick_count_mirror = Arc::clone(&self.tick_count);
        let last_tick_ms_mirror = Arc::clone(&self.last_tick_ms);
        let refresh_notify = Arc::clone(&self.refresh_notify);

        let handle = tokio::spawn(async move {
            let mut watchdog = ContextWatchdog::new();
            let mut expected_app = expected_app;
            let mut consecutive_action_failures: u32 = 0;
            let mut last_event_ms: Option<u64> = None;
            let mut last_significant_event_ms: Option<u64> = None;
            let mut element_adapter_index: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            let mut interval = tokio::time::interval(std::time::Duration::from_millis(tick_ms));

            loop {
                // Wait for either the next scheduled tick or an out-of-band
                // wake-up from `Cortex::refresh_now`. Either source advances
                // tick_count so callers waiting on a specific tick_count
                // target see progress.
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = refresh_notify.notified() => {}
                }

                if !running.load(Ordering::Relaxed) {
                    // Stop AXObserver on shutdown
                    observer.stop_observing();
                    debug!(cortex_id = %cortex_id, "Cortex tick loop stopped");
                    break;
                }

                let now = now_ms();

                // 1. Get current context from built-in sources
                let mut new_context = merger.get_context();

                // 1a. Drain audio transcripts into context
                if let Some(ref audio) = audio_capture {
                    let transcripts = audio
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .drain_transcripts();
                    if !transcripts.is_empty() {
                        new_context.transcripts = transcripts
                            .into_iter()
                            .map(|t| cel_context::TranscriptEntry {
                                text: t.text,
                                start_ms: t.start_ms,
                                end_ms: t.end_ms,
                                source: match t.source {
                                    cel_audio::AudioSource::Microphone => "microphone",
                                    cel_audio::AudioSource::SystemOutput => "system_output",
                                    cel_audio::AudioSource::Both => "both",
                                }
                                .to_string(),
                                speaker: t.speaker,
                                confidence: t.confidence,
                            })
                            .collect();
                    }
                }

                // 1b. Adapter activation/deactivation based on frontmost app
                let current_app = &new_context.app;
                element_adapter_index.clear();
                let mut active_adapter_names: Vec<String> = Vec::new();

                let mut adapters_guard = adapters.write().await;
                for adapter in adapters_guard.iter_mut() {
                    let lifecycle = adapter.driver.manifest().lifecycle.clone();
                    let frontmost_match = adapter.matches_app(current_app);
                    let should_be_active = if lifecycle.requires_frontmost {
                        frontmost_match
                    } else if lifecycle.background_refresh {
                        frontmost_match || adapter.driver.probe().await
                    } else {
                        frontmost_match
                    };

                    match (adapter.state, should_be_active) {
                        (
                            crate::adapter::AdapterState::Inactive
                            | crate::adapter::AdapterState::Error,
                            true,
                        ) => {
                            // Activate (or retry after a transient activation error).
                            if let Err(e) = adapter.driver.activate().await {
                                warn!(cortex_id = %cortex_id, adapter = %adapter.driver.manifest().name, "Adapter activation failed: {e}");
                                adapter.state = crate::adapter::AdapterState::Error;
                            } else {
                                if lifecycle.bootstrap_on_activate {
                                    if let Err(e) = adapter.driver.bootstrap().await {
                                        warn!(cortex_id = %cortex_id, adapter = %adapter.driver.manifest().name, "Adapter bootstrap failed: {e}");
                                        adapter.state = crate::adapter::AdapterState::Error;
                                    } else {
                                        adapter.state = crate::adapter::AdapterState::Active;
                                        debug!(cortex_id = %cortex_id, adapter = %adapter.driver.manifest().name, "Adapter activated and bootstrapped");
                                    }
                                } else {
                                    adapter.state = crate::adapter::AdapterState::Active;
                                    debug!(cortex_id = %cortex_id, adapter = %adapter.driver.manifest().name, "Adapter activated");
                                }
                            }
                        }
                        (crate::adapter::AdapterState::Active, false) => {
                            // Deactivate
                            let _ = adapter.driver.deactivate().await;
                            adapter.state = crate::adapter::AdapterState::Inactive;
                            debug!(cortex_id = %cortex_id, adapter = %adapter.driver.manifest().name, "Adapter deactivated");
                        }
                        _ => {}
                    }

                    // Read context from active adapters
                    if adapter.state == crate::adapter::AdapterState::Active
                        && adapter.should_read(tick_ms)
                    {
                        match adapter.driver.snapshot().await {
                            Ok(elements) => {
                                let adapter_name = adapter.driver.manifest().name.clone();
                                let confidence = adapter.driver.manifest().context.confidence;
                                for mut el in elements {
                                    // Tag element with adapter source
                                    el.source = cel_context::ContextSource::NativeApi;
                                    el.confidence = confidence;
                                    element_adapter_index
                                        .insert(el.id.clone(), adapter_name.clone());
                                    new_context.elements.push(el);
                                }
                                active_adapter_names.push(adapter_name);
                                adapter.ticks_since_last_read = 0;
                            }
                            Err(e) => {
                                warn!(cortex_id = %cortex_id, adapter = %adapter.driver.manifest().name, "Adapter context read failed: {e}");
                                adapter.ticks_since_last_read += 1;
                            }
                        }
                    } else {
                        adapter.ticks_since_last_read += 1;
                        if adapter.state == crate::adapter::AdapterState::Active {
                            active_adapter_names.push(adapter.driver.manifest().name.clone());
                        }
                    }
                }
                drop(adapters_guard); // Release lock before remaining tick work

                // 2. Poll events: watchdog (polling) + AXObserver (push)
                let network_idle = merger.recent_network_events().is_empty();
                let mut events = watchdog.tick(&new_context, network_idle);

                // Merge push-based AXObserver events
                let ax_events = observer.drain_events();
                if !ax_events.is_empty() {
                    events.extend(watchdog.merge_ax_events(ax_events));
                }

                // 3. Classify significance
                let has_significant = events.iter().any(is_significant_event);
                if !events.is_empty() {
                    last_event_ms = Some(now);
                }
                if has_significant {
                    last_significant_event_ms = Some(now);
                }

                // 4. Read old context for diffing
                let old_context = {
                    let m = model.read().await;
                    m.current_context.clone()
                };

                // 5. Skeleton/spinner detection
                let is_skeleton = is_skeleton_screen(&new_context);

                // 6. Diff
                let raw_diff = diff_contexts(&old_context, &new_context);
                let perception_diff = if is_diff_significant(&raw_diff) {
                    Some(to_perception_diff(&raw_diff))
                } else {
                    None
                };

                // 7. Detect anomalies
                let event_anomalies = detect_anomalies_from_events(&events, &expected_app);
                let context_anomalies = detect_anomalies_from_context(&new_context);

                // 8. Detect major transitions
                let app_changed = new_context.app != old_context.app;
                let window_changed = new_context.window != old_context.window;

                // 9. Element stability tracking
                let current_ids: HashSet<String> =
                    new_context.elements.iter().map(|e| e.id.clone()).collect();

                for el in &new_context.elements {
                    let prev = element_seen_count.get(&el.id).copied().unwrap_or(0);
                    let count = (prev + 1).min(STABLE_THRESHOLD + 1);
                    element_seen_count.insert(el.id.clone(), count);
                }
                for id in &element_last_seen {
                    if !current_ids.contains(id) {
                        element_seen_count.remove(id);
                    }
                }
                element_last_seen = current_ids.clone();

                // Hard cap
                if element_seen_count.len() > MAX_ELEMENT_TRACKING {
                    let excess = element_seen_count.len() - PRUNE_ELEMENT_TARGET;
                    let keys: Vec<String> =
                        element_seen_count.keys().take(excess).cloned().collect();
                    for key in keys {
                        element_seen_count.remove(&key);
                    }
                }

                // Classify stability
                let mut stable_set = HashSet::new();
                let mut volatile_set = HashSet::new();
                for (id, count) in &element_seen_count {
                    if *count >= STABLE_THRESHOLD {
                        stable_set.insert(id.clone());
                    }
                }
                for id in &current_ids {
                    if element_seen_count.get(id).copied().unwrap_or(0) <= 1 {
                        volatile_set.insert(id.clone());
                    }
                }

                // 10. Vision needed
                let actionable_count = new_context
                    .elements
                    .iter()
                    .filter(|el| el.state.enabled && el.state.visible && !el.actions.is_empty())
                    .count();
                let vision_needed =
                    actionable_count < SPARSE_CONTEXT_THRESHOLD || consecutive_action_failures >= 2;

                // 11. Focused element
                let focused = new_context
                    .elements
                    .iter()
                    .find(|el| el.state.focused)
                    .map(|el| FocusedElement {
                        id: el.id.clone(),
                        label: el.label.clone(),
                    });

                // ─── Write to model ──────────────────────────────────────
                {
                    let mut m = model.write().await;

                    // Reset tracking on major transitions
                    if app_changed || window_changed {
                        m.recent_diffs.clear();
                        m.anomaly_queue.clear();
                        m.stability = ElementStability::default();
                        m.temporal.loading = None;
                        m.temporal.error_persisting = None;
                        m.temporal.idle_since = None;
                        m.temporal.stagnant_cycles = 0;

                        element_seen_count.clear();
                        element_last_seen.clear();
                        consecutive_action_failures = 0;

                        if app_changed {
                            expected_app = new_context.app.clone();
                        }
                    }

                    // Context
                    m.current_context = new_context;
                    m.focused_element = focused.clone();
                    m.confidence = 1.0;
                    if has_significant {
                        last_significant_event_ms = None;
                    }

                    // Diff rolling window
                    if let Some(ref diff) = perception_diff {
                        m.recent_diffs.push_back(diff.clone());
                        if m.recent_diffs.len() > MAX_RECENT_DIFFS {
                            m.recent_diffs.pop_front();
                        }
                    }

                    // Temporal: stagnant cycles
                    if perception_diff.is_none() {
                        m.temporal.stagnant_cycles += 1;
                    } else {
                        m.temporal.stagnant_cycles = 0;
                    }

                    // Temporal: idle detection
                    if perception_diff.is_some() || !events.is_empty() {
                        m.temporal.idle_since = None;
                    } else if m.temporal.idle_since.is_none() {
                        m.temporal.idle_since = Some(now);
                    }

                    // Temporal: loading state
                    if is_skeleton {
                        let wait = skeleton_wait_ms(&m.current_context);
                        if wait > 0 {
                            let duration = m
                                .temporal
                                .loading
                                .as_ref()
                                .map(|l| l.duration_ms + tick_ms)
                                .unwrap_or(0);
                            m.temporal.loading = Some(LoadingState {
                                detected: true,
                                duration_ms: duration,
                            });
                        }
                    } else {
                        m.temporal.loading = None;
                    }

                    // Temporal: error persistence
                    let has_error = m.current_context.elements.iter().any(|el| {
                        let label = el.label.as_deref().unwrap_or("").to_lowercase();
                        label.contains("error")
                            || label.contains("failed")
                            || label.contains("exception")
                    });
                    if has_error {
                        if let Some(ref mut err) = m.temporal.error_persisting {
                            err.duration_ms += tick_ms;
                        } else {
                            let msg = m.current_context.elements.iter().find_map(|el| {
                                let label = el.label.as_deref().unwrap_or("").to_lowercase();
                                if label.contains("error") || label.contains("failed") {
                                    el.label.clone()
                                } else {
                                    None
                                }
                            });
                            m.temporal.error_persisting = Some(ErrorState {
                                detected: true,
                                duration_ms: 0,
                                message: msg,
                            });
                        }
                    } else {
                        m.temporal.error_persisting = None;
                    }

                    // Temporal: focus trail
                    if let Some(ref f) = focused {
                        let label = f.label.as_deref().unwrap_or(&f.id);
                        let last = m.temporal.focus_trail.back().map(|s| s.as_str());
                        if last != Some(label) {
                            m.temporal.focus_trail.push_back(label.to_string());
                            if m.temporal.focus_trail.len() > MAX_FOCUS_TRAIL {
                                m.temporal.focus_trail.pop_front();
                            }
                        }
                    }

                    // Stability
                    m.stability = ElementStability {
                        stable: stable_set,
                        volatile: volatile_set,
                    };

                    // Anomaly queue — dedup and TTL
                    for anomaly in event_anomalies.into_iter().chain(context_anomalies) {
                        let is_dup = m.anomaly_queue.iter().any(|q| {
                            q.anomaly_type == anomaly.anomaly_type
                                && q.description == anomaly.description
                                && now.saturating_sub(q.timestamp) < ANOMALY_DEDUP_WINDOW_MS
                        });
                        if !is_dup {
                            m.anomaly_queue.push_back(anomaly);
                        }
                    }
                    let ttl_cutoff = now.saturating_sub(ANOMALY_TTL_MS);
                    while m
                        .anomaly_queue
                        .front()
                        .is_some_and(|a| a.timestamp < ttl_cutoff)
                    {
                        m.anomaly_queue.pop_front();
                    }
                    while m.anomaly_queue.len() > MAX_ANOMALY_QUEUE {
                        m.anomaly_queue.pop_front();
                    }

                    // Vision + meta
                    m.vision_needed = vision_needed;
                    m.cycle_count += 1;
                    m.uptime_ms = now.saturating_sub(boot_time);

                    // Adapter state
                    m.element_adapter_index = element_adapter_index.clone();
                    m.active_adapters = active_adapter_names.clone();
                    m.refresh_derived(now, last_event_ms, last_significant_event_ms);
                }

                // Mirror liveness state into atomics for lock-free external
                // observation. Order matters: last_tick_ms first (so readers
                // that see an advancing tick_count also see a fresh
                // timestamp), then tick_count. Both are Relaxed — consumers
                // only need eventual consistency, not strict ordering.
                last_tick_ms_mirror.store(now, Ordering::Relaxed);
                tick_count_mirror.fetch_add(1, Ordering::Relaxed);

                let cycle_count = model.read().await.cycle_count;
                trace!(
                    cortex_id = %cortex_id,
                    cycle = cycle_count,
                    events = events.len(),
                    significant = has_significant,
                    "Cortex tick"
                );
            }
        });

        self.task_handle = Some(handle);
        Ok(())
    }

    // ─── Target validation (Phase 2) ────────────────────────────────────

    /// Check that all `target_ids` exist in the given context. Returns a
    /// `TargetValidation` reporting any that are missing, so the runner can
    /// replan instead of silently misfiring against stale element IDs.
    ///
    /// Does NOT consult the `MentalModel` directly — callers pass the
    /// context they intend to execute against (typically a post-refresh
    /// snapshot), so validation and dispatch agree on the same element set.
    pub fn validate_targets(
        &self,
        context: &ScreenContext,
        target_ids: &[&str],
    ) -> TargetValidation {
        let missing = target_ids
            .iter()
            .filter(|id| find_element(context, id).is_none())
            .map(|id| (*id).to_string())
            .collect();
        TargetValidation { missing }
    }

    // ─── Liveness API (Phase 1) ─────────────────────────────────────────

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

    /// Force an out-of-band tick and return when it completes. Returns the
    /// `tick_count` after the triggered tick. Useful when a caller needs the
    /// mental model to reflect state as of *now*, not as of the last 200ms
    /// interval boundary.
    ///
    /// `timeout_ms` defaults to 500. If the tick doesn't complete within
    /// that budget (e.g. the merger is hung), returns
    /// `CortexError::RefreshTimeout` and increments `stalled_ticks`.
    ///
    /// Returns `CortexError::NotRunning` if called before `boot()` or after
    /// `shutdown()` — no point waiting on a loop that will never fire.
    pub async fn refresh_now(&self, timeout_ms: Option<u64>) -> Result<u64, CortexError> {
        if !self.running.load(Ordering::Relaxed) {
            return Err(CortexError::NotRunning(self.id.clone()));
        }
        let timeout = std::time::Duration::from_millis(timeout_ms.unwrap_or(500));
        let baseline = self.tick_count.load(Ordering::Relaxed);
        let target = baseline.saturating_add(1);

        // Wake the tick loop. Using notify_one so we don't spam the loop if
        // many callers race — a single tick satisfies them all since they
        // observe the same advancing `tick_count`.
        self.refresh_notify.notify_one();

        let start = std::time::Instant::now();
        let poll = std::time::Duration::from_millis(5);
        loop {
            if self.tick_count.load(Ordering::Relaxed) >= target {
                return Ok(self.tick_count.load(Ordering::Relaxed));
            }
            if start.elapsed() >= timeout {
                self.stalled_ticks.fetch_add(1, Ordering::Relaxed);
                return Err(CortexError::RefreshTimeout {
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// Shutdown the cortex — stops the background loop.
    pub fn shutdown(&self) {
        if !self.running.load(Ordering::Relaxed) {
            return;
        }
        self.running.store(false, Ordering::Relaxed);
        if let Some(ref audio) = self.audio_capture {
            let _ = audio.lock().unwrap_or_else(|p| p.into_inner()).stop();
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

    /// Pre-flight focus check for native-input dispatches (Key, KeyCombo,
    /// Type-without-target-id). These actions dispatch through OS-level
    /// input drivers that target **whatever app is frontmost**. If the
    /// Cortex has a CDP client bound — meaning the caller is driving a
    /// browser — and the frontmost app isn't a browser, the keystrokes
    /// would land in the wrong window (terminal, Claude, editor). Worse,
    /// eval smoke saw this trigger a recovery spiral where Cmd+L gets
    /// typed into the terminal and the goal never escapes.
    ///
    /// The gate:
    ///   1. If no CDP client bound → non-browser goal → let native input fire.
    ///   2. If frontmost is already a browser → proceed.
    ///   3. Otherwise, activate the preferred browser, poll up to
    ///      `poll_ms` for focus to land. Fail with a clear error if it
    ///      never does — refusing to dispatch into the wrong window is
    ///      always safer than guessing.
    ///
    /// Returns `Ok(())` when it's safe to dispatch native input;
    /// `Err(CortexError::ExecutionFailed)` otherwise.
    pub fn ensure_browser_focus(&self, action_kind: &str) -> Result<(), CortexError> {
        // No CDP client = this cortex isn't driving a browser. Native
        // input is the intended primary path; don't gate.
        if self.cdp_client.is_none() {
            return Ok(());
        }

        // `with_native_input_unsafe()` is the caller's explicit opt-in:
        // they've accepted that the session is isolated enough that
        // stray keystrokes are fine, and they want to drive non-browser
        // apps too. The browser-focus guard must defer to that opt-in
        // — otherwise scenarios that hand off to Numbers / Finder /
        // Notes can never send keys, because the guard would keep
        // trying to raise Chrome back to the front.
        if self.allow_native_input {
            return Ok(());
        }

        // Fast path: already focused on a browser.
        if frontmost_is_browser() {
            return Ok(());
        }

        warn!(
            action = action_kind,
            "Native input about to fire while focus is off the CDP browser — activating"
        );
        // Try to raise the preferred CDP browser. Poll ~1.2s for focus
        // to land; frontmost changes via osascript are near-instant on
        // macOS but can take up to ~1s on a busy system.
        let _ = cel_cdp::activate_preferred_browser_target();
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(1_200) {
            if frontmost_is_browser() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(80));
        }

        let frontmost = get_frontmost_app_name().unwrap_or_else(|| "unknown".into());
        Err(CortexError::ExecutionFailed(format!(
            "Focus guard refused {action_kind}: frontmost app is \"{frontmost}\", \
             not the CEL CDP browser. Raising the browser via osascript \
             didn't land focus within 1.2s — aborting rather than sending \
             keystrokes to the wrong window."
        )))
    }

    /// Runtime refusal of `ax_action` / `click` on web content.
    ///
    /// When a CDP client is bound AND the frontmost app is a
    /// browser, `ax:*` target_ids on page content are almost always
    /// wrong: the AX tree for a web page is a brittle projection of
    /// the DOM, and actions routed through it land on whatever
    /// happens to be focused (often nothing). Refusing them here
    /// forces the planner onto the `cdp_eval` path for in-page work,
    /// where CEL has full reliability.
    ///
    /// Returns `Some(reason)` when the action should be refused, or
    /// `None` to let it proceed. `ax:*` targets for browser chrome
    /// (tabs, bookmarks bar) still work — the runner rejects on
    /// target prefix + browser focus, not on element type, and
    /// legitimate browser-chrome AX ids don't come up in web-content
    /// goals in practice.
    fn refuse_ax_on_browser_page(&self, target_id: &str, action: &str) -> Option<String> {
        self.cdp_client.as_ref()?;
        if !target_id.starts_with("ax:") {
            return None;
        }
        if !frontmost_is_browser() {
            return None;
        }
        Some(format!(
            "runtime refuses {action} on `{target_id}`: \
             CDP is bound to a browser; in-page interactions must go through \
             `cdp_eval` (click via DOM, not AX). Switch to a cdp_eval action \
             such as `document.querySelector('...').click()` or \
             `window.location.href = '<url>'`."
        ))
    }

    /// Target-app focus gate. If the last successful `activate_app`
    /// named an app X, and X isn't currently frontmost (another app
    /// has stolen focus — a notification, an editor, the session this
    /// eval was spawned from), re-raise X synchronously before the
    /// keystroke dispatches. Best-effort: failures are swallowed
    /// because the caller is already past the safety gate at this
    /// point — we're trying to un-steal focus, not refuse the action.
    ///
    /// Only fires when `allow_native_input` is on. In browser-only
    /// cortexes the browser-focus gate above already handled this.
    fn ensure_target_app_focus(&self) {
        if !self.allow_native_input {
            return;
        }
        let target = match self.last_activated_app.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let Some(target) = target else {
            return;
        };
        let target_lower = target.to_lowercase();
        if let Some(current) = get_frontmost_app_name() {
            if current.to_lowercase().contains(&target_lower)
                || target_lower.contains(&current.to_lowercase())
            {
                return;
            }
            tracing::debug!(
                target = %target,
                current = %current,
                "Pre-keystroke focus gate: target app not frontmost, re-raising"
            );
        }
        let safe_name = target.replace('"', "\\\"");
        // System Events `set frontmost := true` is the same incantation
        // activate_app uses; re-firing it here keeps the dispatch path
        // simple and consistent with launch-time behavior.
        let script = format!(
            r#"tell application "System Events"
                 repeat with p in (every application process whose name is "{}" or name contains "{}")
                   set frontmost of p to true
                 end repeat
               end tell"#,
            safe_name, safe_name
        );
        let mut command = std::process::Command::new("osascript");
        command.args(["-e", &script]);
        let _ = command_status_with_timeout(command, std::time::Duration::from_secs(2));
        // Short settle so the window server flushes the activation
        // before the keystroke lands. 150ms is empirically enough on
        // fast dev machines; shorter and the next CGEvent races the
        // focus change, longer and every step adds observable latency.
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    /// Execute a planner action through native CEL primitives.
    ///
    /// This is the first migration slice: native/non-browser actions are owned
    /// by Rust. Adapter-dispatched execution can be layered in afterward.
    pub async fn execute(
        &self,
        action: &PlannedAction,
        context: &ScreenContext,
    ) -> Result<crate::adapter::ActionResult, CortexError> {
        use crate::adapter::ActionResult;

        self.notify_action(action_type_str(action)).await;

        // CDP-direct interception: when bound to a CDP client AND the action
        // targets a `dom:*` element, dispatch via CDP and return immediately.
        // This guarantees the action lands in the bound CDP browser regardless
        // of what app the user has focused — preventing the eval from typing
        // into the user's chat window or destroying their open form.
        if let Some(client) = &self.cdp_client {
            if let Some(result) = try_cdp_dispatch(client.as_ref(), action).await? {
                return Ok(result);
            }
        } else if action_dom_target(action).is_some() {
            if let Some(client) = cel_cdp::connect_to_focused_app().await {
                if let Some(result) = try_cdp_dispatch(&client, action).await? {
                    return Ok(result);
                }
            }
        }

        // SAFETY GATE: refuse to fall through to native macOS input drivers
        // unless explicitly opted in via `with_native_input_unsafe()`. Pure
        // data/control actions (Wait, Done, Fail, Extract, NotebookWrites,
        // Batch) and CdpEval still run — only system-I/O actions are gated.
        // See `Cortex::with_native_input_unsafe` for the rationale.
        if !self.allow_native_input && action_requires_native_input(action) {
            return Ok(crate::adapter::ActionResult::fail(format!(
                "cortex refused: action `{}` would dispatch through native macOS input \
                 (mouse/keyboard/AX/app-activation), but allow_native_input=false. \
                 Either bind a CDP client (Cortex::with_cdp_client) for browser-only \
                 execution, or — if you really mean to drive the local machine — \
                 call Cortex::with_native_input_unsafe() at construction. \
                 NEVER enable native input in eval/CI contexts.",
                action_type_str(action),
            )));
        }

        let result = match action {
            PlannedAction::Click { target_id } => {
                if let Some(reason) = self.refuse_ax_on_browser_page(target_id, "click") {
                    return Ok(ActionResult::fail(reason));
                }
                if let Some(element) = find_element(context, target_id) {
                    if try_ax_action(target_id, "click")? {
                        ActionResult::ok()
                    } else if let Some((x, y)) = bounds_center(element) {
                        let mut controller = create_controller()
                            .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                        controller
                            .click(x, y, MouseButton::Left)
                            .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                        ActionResult::ok()
                    } else {
                        ActionResult::fail(format!(
                            "Element \"{target_id}\" has no actionable bounds"
                        ))
                    }
                } else {
                    ActionResult::fail(format!("Element \"{target_id}\" not found"))
                }
            }
            PlannedAction::Type { target_id, text } => {
                // No target_id means "type into whatever's focused" — the
                // exact case the focus gate prevents. Target-bound Type
                // still clicks first, but typing itself still goes OS-level.
                if let Err(e) = self.ensure_browser_focus("type") {
                    return Ok(ActionResult::fail(e.to_string()));
                }
                self.ensure_target_app_focus();
                let mut controller =
                    create_controller().map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                if let Some(target_id) = target_id {
                    if let Some(element) = find_element(context, target_id) {
                        if let Some((x, y)) = bounds_center(element) {
                            controller
                                .click(x, y, MouseButton::Left)
                                .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                        } else {
                            return Ok(ActionResult::fail(format!(
                                "Element \"{target_id}\" has no actionable bounds"
                            )));
                        }
                    } else {
                        return Ok(ActionResult::fail(format!(
                            "Element \"{target_id}\" not found"
                        )));
                    }
                }
                controller
                    .type_text(text)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::Key { key } => {
                if let Err(e) = self.ensure_browser_focus("key") {
                    return Ok(ActionResult::fail(e.to_string()));
                }
                self.ensure_target_app_focus();
                let mut controller =
                    create_controller().map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                controller
                    .key_press(key)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::KeyCombo { keys } => {
                if let Err(e) = self.ensure_browser_focus("key_combo") {
                    return Ok(ActionResult::fail(e.to_string()));
                }
                self.ensure_target_app_focus();
                let mut controller =
                    create_controller().map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
                controller
                    .key_combo(&key_refs)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::SetValue { target_id, value } => {
                if try_set_value(target_id, value)? {
                    ActionResult::ok()
                } else {
                    ActionResult::fail(format!("Could not set value on \"{target_id}\""))
                }
            }
            PlannedAction::Scroll { dx, dy } => {
                let mut controller =
                    create_controller().map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                controller
                    .scroll(*dx, *dy)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::Drag {
                from_target_id,
                to_target_id,
            } => {
                let Some(from_element) = find_element(context, from_target_id) else {
                    return Ok(ActionResult::fail(format!(
                        "Element \"{from_target_id}\" not found"
                    )));
                };
                let Some(to_element) = find_element(context, to_target_id) else {
                    return Ok(ActionResult::fail(format!(
                        "Element \"{to_target_id}\" not found"
                    )));
                };
                let Some((from_x, from_y)) = bounds_center(from_element) else {
                    return Ok(ActionResult::fail(format!(
                        "Element \"{from_target_id}\" has no actionable bounds"
                    )));
                };
                let Some((to_x, to_y)) = bounds_center(to_element) else {
                    return Ok(ActionResult::fail(format!(
                        "Element \"{to_target_id}\" has no actionable bounds"
                    )));
                };
                let mut controller =
                    create_controller().map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                controller
                    .drag(from_x, from_y, to_x, to_y)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::Wait { ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(*ms as u64)).await;
                ActionResult::ok()
            }
            PlannedAction::AxAction {
                target_id,
                action,
                label,
                role_hint,
            } => {
                if let Some(reason) = self.refuse_ax_on_browser_page(target_id, action) {
                    return Ok(ActionResult::fail(reason));
                }
                // Primary: try the planner-supplied target_id. AX ids
                // are bounds-hashed and therefore fragile across tree
                // mutations (animations, focus shifts, popovers).
                match try_ax_action(target_id, action) {
                    Ok(true) => ActionResult::ok(),
                    Ok(false) | Err(_) => {
                        // Fallback: if the LLM supplied a `label`, ask
                        // the live AX tree to resolve role+label → id
                        // and try again. This recovers from the common
                        // stale-hash failure mode without the planner
                        // needing to re-plan.
                        if let Some(lbl) = label.as_deref() {
                            if let Some(resolved) = resolve_ax_by_label(lbl, role_hint.as_deref()) {
                                if try_ax_action(&resolved, action).unwrap_or(false) {
                                    tracing::info!(
                                        target_id = %target_id,
                                        resolved = %resolved,
                                        label = %lbl,
                                        "ax_action fell back to label resolution"
                                    );
                                    return Ok(ActionResult::ok());
                                }
                            }
                        }
                        ActionResult::fail(format!(
                            "AX action \"{action}\" failed on \"{target_id}\"{}",
                            label
                                .as_ref()
                                .map(|l| format!(" (label=\"{l}\" also not found)"))
                                .unwrap_or_default()
                        ))
                    }
                }
            }
            PlannedAction::ActivateApp { app_name } => {
                let result = activate_app_with_verification(app_name)?;
                if result.success {
                    // Remember the target so subsequent Key/KeyCombo/Type
                    // actions can re-raise it if focus drifts. See
                    // `ensure_target_app_focus`.
                    if let Ok(mut guard) = self.last_activated_app.lock() {
                        *guard = Some(app_name.clone());
                    }
                }
                result
            }
            PlannedAction::Select {
                from_x,
                from_y,
                to_x,
                to_y,
            } => {
                let mut controller =
                    create_controller().map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                controller
                    .drag(*from_x, *from_y, *to_x, *to_y)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::Custom {
                adapter,
                action,
                params,
            } => {
                // Route to registered adapter if available
                let adapters = self.adapters.read().await;
                if let Some(registered) = adapters
                    .iter()
                    .find(|a| a.driver.manifest().name == *adapter)
                {
                    if registered.state == crate::adapter::AdapterState::Active {
                        let action_decl = registered.driver.manifest().actions.get(action).cloned();
                        match registered.driver.execute(action, params.clone()).await {
                            Ok(result) => {
                                if result.success
                                    && action_decl
                                        .as_ref()
                                        .map(|decl| decl.requires_verification)
                                        .unwrap_or(false)
                                {
                                    match registered
                                        .driver
                                        .verify_action(action, params, &result)
                                        .await
                                    {
                                        Ok(Some(verified)) => verified,
                                        Ok(None) => result,
                                        Err(e) => ActionResult::fail(format!(
                                            "Adapter \"{adapter}\" verification error: {e}"
                                        )),
                                    }
                                } else {
                                    result
                                }
                            }
                            Err(e) => {
                                ActionResult::fail(format!("Adapter \"{adapter}\" error: {e}"))
                            }
                        }
                    } else {
                        ActionResult::fail(format!(
                            "Adapter \"{adapter}\" is not active (state: {:?})",
                            registered.state
                        ))
                    }
                } else {
                    ActionResult::fail(format!(
                        "No adapter registered for \"{adapter}\". Register it in the Cortex first."
                    ))
                }
            }
            PlannedAction::Batch { actions } => {
                // Execute batch sequentially, stop on first failure
                for (i, sub_action) in actions.iter().enumerate() {
                    let sub_result = Box::pin(self.execute(sub_action, context)).await?;
                    if !sub_result.success {
                        return Ok(ActionResult::fail(format!(
                            "Batch action {}/{} failed: {}",
                            i + 1,
                            actions.len(),
                            sub_result.error.unwrap_or_default()
                        )));
                    }
                }
                ActionResult::ok()
            }
            PlannedAction::Act { instruction } => {
                // Semantic action resolution: find best matching element and click it
                // Simple heuristic — match instruction keywords against element labels
                let lower = instruction.to_lowercase();
                if let Some(el) = context.elements.iter().find(|el| {
                    el.state.visible
                        && !el.actions.is_empty()
                        && el
                            .label
                            .as_ref()
                            .is_some_and(|l| lower.contains(&l.to_lowercase()))
                }) {
                    let click = PlannedAction::Click {
                        target_id: el.id.clone(),
                    };
                    return Box::pin(self.execute(&click, context)).await;
                }
                ActionResult::fail(format!("Could not resolve: {instruction}"))
            }
            PlannedAction::CdpEval { expression } => {
                // Navigation-style cdp_eval (window.location.href = '<url>')
                // must not be dispatched into whatever stale page target
                // connect_to_focused_app() happens to bind. Detect it here
                // and reset CEL's dedicated automation browser to a fresh
                // page target at the requested URL before falling through.
                if let Some(nav_url) = extract_navigation_url(expression) {
                    if let Err(e) = cel_cdp::reset_preferred_target(&nav_url) {
                        tracing::debug!("reset_preferred_target({}) failed: {}", nav_url, e);
                    }
                }
                // Every cdp_eval is preceded by a small, idempotent prelude
                // that patches `HTMLSelectElement.prototype.value` to also
                // match by option text when the supplied value doesn't
                // match an option's `value` attribute. Without this, LLMs
                // writing `select.value = "Technical Support"` hit the HTML
                // spec no-op and forms silently fail validation. Patching
                // at the prototype level means the fix applies regardless
                // of whether the LLM used set_value, cdp_eval, or whatever
                // selector it built internally.
                let full_expression = format!("{CEL_SELECT_PATCH_PRELUDE}\n{expression}");
                match tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let client = cel_cdp::connect_to_focused_app().await;
                        match client {
                            Some(c) => match c.evaluate(&full_expression).await {
                                Ok(result) => {
                                    Ok(serde_json::to_string(&result).unwrap_or_default())
                                }
                                Err(e) => Err(format!("CDP eval failed: {e}")),
                            },
                            None => Err("No CDP target available".into()),
                        }
                    })
                }) {
                    Ok(result) => ActionResult {
                        success: true,
                        error: None,
                        data: Some(serde_json::Value::String(result)),
                    },
                    Err(e) => ActionResult::fail(e),
                }
            }
            PlannedAction::Navigate { url } => {
                if let Err(e) = cel_cdp::reset_preferred_target(url) {
                    tracing::debug!("reset_preferred_target({}) failed: {}", url, e);
                }
                let sanitized = url.replace('\'', "\\'");
                let expression = format!(
                    "(function() {{ window.location.href = '{}'; return 'navigating'; }})()",
                    sanitized
                );
                let full_expression = format!("{CEL_SELECT_PATCH_PRELUDE}\n{expression}");
                match tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let client = cel_cdp::connect_to_focused_app().await;
                        match client {
                            Some(c) => match c.evaluate(&full_expression).await {
                                Ok(result) => {
                                    Ok(serde_json::to_string(&result).unwrap_or_default())
                                }
                                Err(e) => Err(format!("CDP eval failed: {e}")),
                            },
                            None => Err("No CDP target available".into()),
                        }
                    })
                }) {
                    Ok(result) => ActionResult {
                        success: true,
                        error: None,
                        data: Some(serde_json::Value::String(result)),
                    },
                    Err(e) => ActionResult::fail(e),
                }
            }
            PlannedAction::WriteCells {
                app,
                sheet,
                table,
                writes,
                verify,
            } => {
                self.dispatch_write_cells(app, sheet.as_deref(), table.as_deref(), writes, *verify)
                    .await
            }
            PlannedAction::ReadCells {
                app,
                sheet,
                table,
                cell_refs,
            } => {
                self.dispatch_read_cells(app, sheet.as_deref(), table.as_deref(), cell_refs)
                    .await
            }
            PlannedAction::ExtractWithFallback {
                name,
                selectors,
                parse_as,
            } => self.dispatch_extract_with_fallback(name, selectors, parse_as),
            PlannedAction::Extract { .. }
            | PlannedAction::Done { .. }
            | PlannedAction::Fail { .. }
            | PlannedAction::NotebookWrites { .. } => ActionResult::ok(),
        };

        if result.success {
            self.report_action_success().await;
        } else {
            self.report_action_failure().await;
        }

        Ok(result)
    }

    /// Extract a single value from the focused CDP page by trying a
    /// list of candidate selectors in order and parsing the first
    /// match. Replaces the "LLM hand-writes document.querySelector in
    /// a loop" failure mode — the runtime owns the retry/parse
    /// machinery and the planner just supplies the selector candidates
    /// plus a logical `name` under which the result is persisted.
    ///
    /// Selector entry is auto-detected:
    ///   * Starts with `function` / `(function` / `(() =>` / `return` →
    ///     treated as a raw JS expression, evaluated directly.
    ///   * Otherwise treated as a CSS selector; wrapped into
    ///     `document.querySelector(SEL)?.textContent ?? null`.
    ///
    /// Returns `ActionResult::ok` with `data = { "name": ..., "value":
    /// <parsed>, "selector_used": <which one hit>, "raw": <raw string> }`
    /// on success. On failure (no selector yielded a non-null value)
    /// returns `ActionResult::fail` with a diagnostic listing every
    /// selector tried and what each yielded — this goes into the
    /// planner's history so the next turn sees exactly what was
    /// tried and why it didn't work.
    fn dispatch_extract_with_fallback(
        &self,
        name: &str,
        selectors: &[String],
        parse_as: &str,
    ) -> crate::adapter::ActionResult {
        use crate::adapter::ActionResult;
        if selectors.is_empty() {
            return ActionResult::fail(format!(
                "extract_with_fallback({name}): empty selector list — provide at least one candidate"
            ));
        }
        let mut diagnostics: Vec<String> = Vec::with_capacity(selectors.len());
        for sel in selectors {
            let expr = build_extract_expression(sel);
            let eval = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let client = cel_cdp::connect_to_focused_app().await;
                    match client {
                        Some(c) => c.evaluate(&expr).await.map_err(|e| e.to_string()),
                        None => Err("No CDP target available".into()),
                    }
                })
            });
            let raw = match eval {
                Ok(v) => v,
                Err(e) => {
                    diagnostics.push(format!("[{sel}] cdp error: {e}"));
                    continue;
                }
            };
            let raw_str = cdp_value_to_string(&raw);
            if raw_str.is_none() {
                diagnostics.push(format!("[{sel}] selector yielded null"));
                continue;
            }
            let raw_str = raw_str.unwrap();
            let parsed = match parse_extracted(&raw_str, parse_as) {
                Some(v) => v,
                None => {
                    diagnostics.push(format!(
                        "[{sel}] parse_as={parse_as} failed on raw={:?}",
                        truncate(&raw_str, 60)
                    ));
                    continue;
                }
            };
            let data = serde_json::json!({
                "name": name,
                "value": parsed,
                "selector_used": sel,
                "raw": raw_str,
            });
            return ActionResult {
                success: true,
                error: None,
                data: Some(data),
            };
        }
        ActionResult::fail(format!(
            "extract_with_fallback({name}): no selector yielded a usable value — tried {} candidates. {}",
            selectors.len(),
            diagnostics.join("; ")
        ))
    }

    /// Route a `WriteCells` action to the correct scripting backend.
    /// Currently only Numbers is wired up; other apps return a clean
    /// runtime error so the planner can pivot instead of silently
    /// falling back to a path we know produces garbage (keystrokes).
    #[cfg(target_os = "macos")]
    fn with_numbers_document_bootstrap<T, F>(
        &self,
        operation_name: &str,
        mut operation: F,
    ) -> Result<T, InputError>
    where
        F: FnMut() -> Result<T, InputError>,
    {
        match operation() {
            Ok(value) => Ok(value),
            Err(original_error) if should_attempt_numbers_document_bootstrap(&original_error) => {
                warn!(
                    operation = operation_name,
                    error = %original_error,
                    "Numbers scripting unavailable; attempting document bootstrap"
                );
                let bootstrap_result = bootstrap_numbers_document();
                if let Err(ref bootstrap_error) = bootstrap_result {
                    warn!(
                        operation = operation_name,
                        error = bootstrap_error,
                        "Numbers document bootstrap did not confirm success before retry"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(900));
                match operation() {
                    Ok(value) => Ok(value),
                    Err(retry_error) => {
                        if let Err(bootstrap_error) = bootstrap_result {
                            Err(InputError::Failed(format!(
                                "{operation_name} retry failed after Numbers bootstrap attempt ({bootstrap_error}). \
                                 Original error: {original_error}. Retry error: {retry_error}"
                            )))
                        } else {
                            Err(retry_error)
                        }
                    }
                }
            }
            Err(error) => Err(error),
        }
    }

    async fn dispatch_adapter_standard_action(
        &self,
        app: &str,
        action: &str,
        params: serde_json::Value,
    ) -> Option<crate::adapter::ActionResult> {
        let adapters = self.adapters.read().await;
        let registered = adapters.iter().find(|candidate| {
            candidate.state == crate::adapter::AdapterState::Active
                && (candidate.driver.manifest().name.eq_ignore_ascii_case(app)
                    || candidate.matches_app(app))
                && candidate.driver.manifest().actions.contains_key(action)
        })?;

        let action_decl = registered.driver.manifest().actions.get(action).cloned();
        match registered.driver.execute(action, params.clone()).await {
            Ok(result) => {
                if result.success
                    && action_decl
                        .as_ref()
                        .map(|decl| decl.requires_verification)
                        .unwrap_or(false)
                {
                    match registered
                        .driver
                        .verify_action(action, &params, &result)
                        .await
                    {
                        Ok(Some(verified)) => Some(verified),
                        Ok(None) => Some(result),
                        Err(err) => Some(crate::adapter::ActionResult::fail(format!(
                            "Adapter \"{}\" verification error: {err}",
                            registered.driver.manifest().name
                        ))),
                    }
                } else {
                    Some(result)
                }
            }
            Err(err) => Some(crate::adapter::ActionResult::fail(format!(
                "Adapter \"{}\" execution error for {action}: {err}",
                registered.driver.manifest().name
            ))),
        }
    }

    async fn dispatch_write_cells(
        &self,
        app: &str,
        sheet: Option<&str>,
        table: Option<&str>,
        writes: &[cel_contracts::CellWrite],
        verify: bool,
    ) -> crate::adapter::ActionResult {
        use crate::adapter::ActionResult;
        let adapter_params = serde_json::json!({
            "sheet": sheet,
            "table": table,
            "verify": verify,
            "writes": writes
                .iter()
                .map(|write| serde_json::json!({
                    "cell_ref": write.cell_ref,
                    "value": write.value,
                }))
                .collect::<Vec<_>>(),
        });
        if let Some(result) = self
            .dispatch_adapter_standard_action(app, "write_cells", adapter_params)
            .await
        {
            return result;
        }
        if !app.eq_ignore_ascii_case("Numbers") {
            return ActionResult::fail(format!(
                "write_cells currently only supports app=\"Numbers\"; got \"{app}\". \
                 Use Numbers or fall back to cdp_eval for web spreadsheets."
            ));
        }
        #[cfg(target_os = "macos")]
        {
            let cell_writes: Vec<cel_input::CellWrite> = writes
                .iter()
                .map(|w| cel_input::CellWrite {
                    cell_ref: w.cell_ref.clone(),
                    value: w.value.clone(),
                })
                .collect();
            match self.with_numbers_document_bootstrap("write_cells", || {
                cel_input::write_numbers_cells(sheet, table, &cell_writes, verify)
            }) {
                Ok(readbacks) => {
                    dismiss_numbers_dialog_if_present();
                    if verify {
                        // Compare readback vs requested, with numeric
                        // tolerance (Numbers canonicalizes "108432.50"
                        // → "108432.5", "$108,432.50" → "108432.5").
                        let mut mismatches = Vec::new();
                        for (w, got) in cell_writes.iter().zip(readbacks.iter()) {
                            if !cells_match(&w.value, got) {
                                mismatches.push(format!(
                                    "{}: wrote \"{}\" got \"{}\"",
                                    w.cell_ref, w.value, got
                                ));
                            }
                        }
                        if !mismatches.is_empty() {
                            return ActionResult::fail(format!(
                                "write_cells verification failed: {}",
                                mismatches.join("; ")
                            ));
                        }
                    }
                    let data = serde_json::json!({
                        "app": app,
                        "writes": cell_writes
                            .iter()
                            .zip(readbacks.iter().chain(std::iter::repeat(&String::new())))
                            .map(|(w, got)| {
                                serde_json::json!({
                                    "ref": w.cell_ref,
                                    "requested": w.value,
                                    "readback": got,
                                })
                            })
                            .collect::<Vec<_>>(),
                    });
                    ActionResult {
                        success: true,
                        error: None,
                        data: Some(data),
                    }
                }
                Err(e) => ActionResult::fail(format!("write_cells failed: {e}")),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (sheet, table, writes, verify);
            ActionResult::fail("write_cells requires macOS (AppleScript backend)".to_string())
        }
    }

    /// Deterministic spreadsheet cell reads from the app model.
    ///
    /// This is the read-side twin of `write_cells`: use it when AX does
    /// not faithfully surface spreadsheet contents and we need app truth
    /// instead of UI guesswork.
    async fn dispatch_read_cells(
        &self,
        app: &str,
        sheet: Option<&str>,
        table: Option<&str>,
        cell_refs: &[String],
    ) -> crate::adapter::ActionResult {
        use crate::adapter::ActionResult;
        let adapter_params = serde_json::json!({
            "sheet": sheet,
            "table": table,
            "cell_refs": cell_refs,
        });
        if let Some(result) = self
            .dispatch_adapter_standard_action(app, "read_cells", adapter_params)
            .await
        {
            return result;
        }
        if !app.eq_ignore_ascii_case("Numbers") {
            return ActionResult::fail(format!(
                "read_cells currently only supports app=\"Numbers\"; got \"{app}\"."
            ));
        }
        #[cfg(target_os = "macos")]
        {
            match self.with_numbers_document_bootstrap("read_cells", || {
                cel_input::read_numbers_cells(sheet, table, cell_refs)
            }) {
                Ok(readbacks) => {
                    dismiss_numbers_dialog_if_present();
                    let data = serde_json::json!({
                        "app": app,
                        "reads": cell_refs
                            .iter()
                            .zip(readbacks.iter())
                            .map(|(cell_ref, value)| {
                                serde_json::json!({
                                    "ref": cell_ref,
                                    "value": value,
                                })
                            })
                            .collect::<Vec<_>>(),
                    });
                    ActionResult {
                        success: true,
                        error: None,
                        data: Some(data),
                    }
                }
                Err(e) => ActionResult::fail(format!("read_cells failed: {e}")),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (sheet, table, cell_refs);
            ActionResult::fail("read_cells requires macOS (AppleScript backend)".to_string())
        }
    }
}

/// Detect whether a selector-string entry is a raw JS expression
/// (common prefixes the LLM uses) vs a bare CSS selector, and wrap
/// bare selectors into `querySelector` calls that safely fall through
/// to `null` on miss.
fn build_extract_expression(sel: &str) -> String {
    let trimmed = sel.trim();
    let looks_like_js = trimmed.starts_with("function")
        || trimmed.starts_with("(function")
        || trimmed.starts_with("(() =>")
        || trimmed.starts_with("(()=>")
        || trimmed.starts_with("return ")
        || trimmed.starts_with("document.")
        || trimmed.starts_with("window.")
        || trimmed.contains("=>");
    if looks_like_js {
        trimmed.to_string()
    } else if trimmed.contains(":contains(") || trimmed.contains(":has(") {
        let escaped = trimmed.replace('\\', "\\\\").replace('\'', "\\'");
        format!(
            "(function() {{
                const selector = '{escaped}';
                const textOf = (el) => (el && el.textContent != null ? el.textContent.trim() : '');
                const includesText = (el, needle) => textOf(el).includes(needle);

                const rowMatch = selector.match(/^([a-zA-Z0-9_-]+):has\\(([a-zA-Z0-9_-]+):contains\\((['\\\"])(.*?)\\3\\)\\)\\s+([a-zA-Z0-9_-]+):nth-child\\((\\d+)\\)$/);
                if (rowMatch) {{
                    const [, rowTag, innerTag, , needle, cellTag, nth] = rowMatch;
                    const rows = Array.from(document.querySelectorAll(rowTag));
                    for (const row of rows) {{
                        const match = Array.from(row.querySelectorAll(innerTag)).find((child) => includesText(child, needle));
                        if (!match) continue;
                        const idx = Math.max(parseInt(nth, 10) - 1, 0);
                        const cells = Array.from(row.querySelectorAll(cellTag));
                        const target = cells[idx];
                        return target ? textOf(target) || null : null;
                    }}
                    return null;
                }}

                const adjacentMatch = selector.match(/^([a-zA-Z0-9_-]+):contains\\((['\\\"])(.*?)\\2\\)\\s*\\+\\s*([a-zA-Z0-9_-]+)$/);
                if (adjacentMatch) {{
                    const [, baseTag, , needle, siblingTag] = adjacentMatch;
                    const bases = Array.from(document.querySelectorAll(baseTag));
                    for (const base of bases) {{
                        if (!includesText(base, needle)) continue;
                        let sibling = base.nextElementSibling;
                        while (sibling) {{
                            if (sibling.matches(siblingTag)) {{
                                return textOf(sibling) || null;
                            }}
                            sibling = sibling.nextElementSibling;
                        }}
                    }}
                    return null;
                }}

                const siblingNthMatch = selector.match(/^([a-zA-Z0-9_-]+):contains\\((['\\\"])(.*?)\\2\\)\\s*~\\s*([a-zA-Z0-9_-]+):nth-of-type\\((\\d+)\\)$/);
                if (siblingNthMatch) {{
                    const [, baseTag, , needle, siblingTag, nth] = siblingNthMatch;
                    const bases = Array.from(document.querySelectorAll(baseTag));
                    for (const base of bases) {{
                        if (!includesText(base, needle) || !base.parentElement) continue;
                        const idx = Math.max(parseInt(nth, 10) - 1, 0);
                        const matches = Array.from(base.parentElement.children).filter((child) => child.matches(siblingTag));
                        const target = matches[idx];
                        return target ? textOf(target) || null : null;
                    }}
                    return null;
                }}

                const containsOnlyMatch = selector.match(/^([a-zA-Z0-9_-]+):contains\\((['\\\"])(.*?)\\2\\)$/);
                if (containsOnlyMatch) {{
                    const [, tag, , needle] = containsOnlyMatch;
                    const match = Array.from(document.querySelectorAll(tag)).find((el) => includesText(el, needle));
                    return match ? textOf(match) || null : null;
                }}

                const fallback = document.querySelector(selector);
                return fallback ? (fallback.textContent == null ? null : fallback.textContent.trim()) : null;
            }})()"
        )
    } else {
        // Bare CSS selector. Escape single quotes for JS-string embedding.
        let escaped = trimmed.replace('\\', "\\\\").replace('\'', "\\'");
        format!(
            "(function() {{ var el = document.querySelector('{escaped}'); \
             return el ? (el.textContent == null ? null : el.textContent.trim()) : null; }})()"
        )
    }
}

/// Flatten a CDP `Runtime.evaluate` result into a string we can parse.
/// Returns `None` when the JS side returned `null`/`undefined` or an
/// empty result object.
fn cdp_value_to_string(v: &serde_json::Value) -> Option<String> {
    // The client already extracts `result.result.value` — the raw value
    // is at the top of `v`. Accept strings, numbers, booleans; reject
    // null/undefined/missing.
    if v.is_null() {
        return None;
    }
    if let Some(s) = v.as_str() {
        if s.is_empty() {
            return None;
        }
        return Some(s.to_string());
    }
    if let Some(n) = v.as_f64() {
        return Some(n.to_string());
    }
    if let Some(b) = v.as_bool() {
        return Some(b.to_string());
    }
    // Object/array: stringify. This catches cases where the JS returned
    // a node ref (rare) or an object.
    let s = serde_json::to_string(v).ok()?;
    if s == "null" || s == "\"\"" {
        return None;
    }
    Some(s)
}

/// Parse the raw string yielded by a selector according to the
/// planner's `parse_as` hint. Unknown hints fall back to "text".
fn parse_extracted(raw: &str, parse_as: &str) -> Option<serde_json::Value> {
    let cleaned = raw.trim();
    match parse_as.to_lowercase().as_str() {
        "float" | "number" => {
            let stripped: String = cleaned
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                .collect();
            stripped.parse::<f64>().ok().map(|n| {
                serde_json::Number::from_f64(n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::String(raw.to_string()))
            })
        }
        "int" | "integer" => {
            let stripped: String = cleaned
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            stripped
                .parse::<i64>()
                .ok()
                .map(|n| serde_json::Value::Number(n.into()))
        }
        _ => Some(serde_json::Value::String(cleaned.to_string())),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

/// Loose equality for write_cells verification. Numbers canonicalizes
/// numeric input when the cell format is Number: `"108432.50"` becomes
/// `"108432.5"`, `"$108,432.50"` becomes `"108432.5"`. Compare as
/// floats when both sides parse; otherwise fall back to trimmed
/// string equality.
#[cfg(target_os = "macos")]
fn cells_match(requested: &str, got: &str) -> bool {
    let r = requested.trim();
    let g = got.trim();
    if r == g {
        return true;
    }
    let strip = |s: &str| {
        s.replace(['$', ',', ' '], "")
            .trim_end_matches('%')
            .to_string()
    };
    let rn = strip(r).parse::<f64>().ok();
    let gn = strip(g).parse::<f64>().ok();
    match (rn, gn) {
        (Some(a), Some(b)) => (a - b).abs() < 1e-6 * a.abs().max(1.0),
        _ => false,
    }
}

fn find_element<'a>(
    context: &'a ScreenContext,
    target_id: &str,
) -> Option<&'a cel_context::ContextElement> {
    context.elements.iter().find(|el| el.id == target_id)
}

fn bounds_center(element: &cel_context::ContextElement) -> Option<(i32, i32)> {
    let bounds = element.bounds.as_ref()?;
    Some((
        bounds.x + (bounds.width as i32 / 2),
        bounds.y + (bounds.height as i32 / 2),
    ))
}

/// Recognize navigation-style `cdp_eval` and extract the destination URL.
///
/// Matches patterns the planner is known to emit, including:
///   * `window.location.href = '<url>'`
///   * `window.location.href='<url>'`
///   * `location.href = "<url>"`
///   * `(function() { window.location.href = '<url>'; return 'navigating'; })()`
///
/// Returns `None` for non-navigation evals. A returned URL has had surrounding
/// quotes stripped and is safe to hand to `reset_preferred_target`.
fn extract_navigation_url(expression: &str) -> Option<String> {
    let normalized = expression.trim();
    let needle = "location.href";
    let idx = normalized.find(needle)?;
    let after = &normalized[idx + needle.len()..];
    let eq = after.find('=')?;
    let rest = after[eq + 1..].trim_start();
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let quote = bytes[0];
    if quote != b'"' && quote != b'\'' && quote != b'`' {
        return None;
    }
    let tail = &rest[1..];
    let end = tail.find(quote as char)?;
    let url = &tail[..end];
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
}

fn try_ax_action(target_id: &str, action: &str) -> Result<bool, CortexError> {
    let tree = cel_accessibility::create_tree();
    tree.perform_action(target_id, action)
        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))
}

/// Resolve a label (and optional role hint) to a live AX element id
/// by querying the accessibility tree right now. Used as a fallback
/// when the planner-supplied hash id isn't found — typically because
/// the tree mutated between plan time and dispatch time (animations,
/// focus shift, modal appearing). Returns the first visible match.
fn resolve_ax_by_label(label: &str, role_hint: Option<&str>) -> Option<String> {
    let tree = cel_accessibility::create_tree();
    let role = role_hint.and_then(parse_role_hint);
    let matches = tree.find_elements(role.as_ref(), Some(label)).ok()?;
    matches.into_iter().find(|e| e.state.visible).map(|e| e.id)
}

/// Map a free-form role string from the LLM ("button", "AXButton",
/// "text field", …) onto [`cel_accessibility::ElementRole`]. Unknown
/// roles return `None` so the fallback search matches on label alone.
fn parse_role_hint(hint: &str) -> Option<cel_accessibility::ElementRole> {
    use cel_accessibility::ElementRole;
    let normalized = hint
        .trim()
        .to_ascii_lowercase()
        .trim_start_matches("ax")
        .replace(['_', '-', ' '], "");
    Some(match normalized.as_str() {
        "button" => ElementRole::Button,
        "input" | "textfield" | "text" => ElementRole::Input,
        "checkbox" => ElementRole::Checkbox,
        "radiobutton" | "radio" => ElementRole::RadioButton,
        "combobox" | "popupbutton" => ElementRole::ComboBox,
        "slider" => ElementRole::Slider,
        "menu" => ElementRole::Menu,
        "menuitem" => ElementRole::MenuItem,
        "tab" => ElementRole::Tab,
        "tabitem" => ElementRole::TabItem,
        "link" => ElementRole::Link,
        "image" => ElementRole::Image,
        "list" => ElementRole::List,
        "listitem" | "row" | "tablerow" => ElementRole::ListItem,
        "cell" | "tablecell" => ElementRole::TableCell,
        "window" => ElementRole::Window,
        "dialog" => ElementRole::Dialog,
        "group" => ElementRole::Group,
        "toolbar" => ElementRole::Toolbar,
        "" => return None,
        _ => return None,
    })
}

fn try_set_value(target_id: &str, value: &str) -> Result<bool, CortexError> {
    let tree = cel_accessibility::create_tree();
    tree.set_value(target_id, value)
        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))
}

/// Try to dispatch the action through CDP. Returns:
///  * `Ok(Some(result))` — we handled it (succeeded or failed via CDP)
///  * `Ok(None)` — not a browser-targeted action; caller should fall back
///    to the native execution path
///
/// Targets a `dom:*` element by parsing the embedded backend_node_id (the
/// element id format is `dom:<element_type>:<id>` per the CDP context pump
/// in cel-eval). For typing we use Runtime.evaluate to set the value AND
/// dispatch input/change events (otherwise React/Vue forms ignore the
/// programmatic value). For clicks we use Runtime.evaluate to find the
/// element and call .click() — element-level, not coordinate-level, so it
/// works with shadow DOM and is robust against scroll position.
async fn try_cdp_dispatch(
    client: &cel_cdp::CdpClient,
    action: &PlannedAction,
) -> Result<Option<crate::adapter::ActionResult>, CortexError> {
    let target = action_dom_target(action);
    let Some(target) = target else {
        return Ok(None);
    };
    if !target.starts_with("dom:") {
        return Ok(None);
    }

    // dom:<role>:<id>   — id is the JS-stable handle we wrote into the model.
    // For elements pumped from cel_cdp::extract_page_content, the id is the
    // CDP backend_node_id (when available). The selector we use is
    // `[data-cel-id]` if present, otherwise we fall back to backend_node_id.
    // Practical resolution: query the DOM by walking the interactive index
    // we recorded — but cleanest is to round-trip via JS that reads the
    // backend_node_id off element.dataset.celBackendId or scans for the
    // element's stable signature. For this slice we use a simple scheme:
    // query by matching element_type and falling back to text content.
    let parts: Vec<&str> = target.splitn(3, ':').collect();
    let (role, id_part) = match parts.as_slice() {
        ["dom", role, id] => (*role, *id),
        _ => return Ok(None),
    };

    match action {
        PlannedAction::Click { .. } | PlannedAction::AxAction { .. } => {
            let js = build_click_js(role, id_part);
            let res = client
                .evaluate(&js)
                .await
                .map_err(|e| CortexError::ExecutionFailed(format!("cdp click: {e}")))?;
            Ok(Some(check_cdp_ok(res, "clicked")))
        }
        PlannedAction::SetValue { value, .. } => {
            let js = build_set_value_js(role, id_part, value);
            let res = client
                .evaluate(&js)
                .await
                .map_err(|e| CortexError::ExecutionFailed(format!("cdp set_value: {e}")))?;
            Ok(Some(check_cdp_ok(res, "set")))
        }
        PlannedAction::Type { text, .. } => {
            // Browser-safe Type: focus + set value + dispatch input/change.
            let js = build_set_value_js(role, id_part, text);
            let res = client
                .evaluate(&js)
                .await
                .map_err(|e| CortexError::ExecutionFailed(format!("cdp type: {e}")))?;
            Ok(Some(check_cdp_ok(res, "typed")))
        }
        _ => Ok(None),
    }
}

fn action_dom_target(action: &PlannedAction) -> Option<&str> {
    match action {
        PlannedAction::Click { target_id }
        | PlannedAction::SetValue { target_id, .. }
        | PlannedAction::AxAction { target_id, .. }
        | PlannedAction::Drag {
            from_target_id: target_id,
            ..
        } => target_id.starts_with("dom:").then_some(target_id.as_str()),
        PlannedAction::Type {
            target_id: Some(target_id),
            ..
        } => target_id.starts_with("dom:").then_some(target_id.as_str()),
        _ => None,
    }
}

fn check_cdp_ok(res: serde_json::Value, op: &'static str) -> crate::adapter::ActionResult {
    use crate::adapter::ActionResult;
    let v = res
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(res);
    match v {
        serde_json::Value::String(s) if s.starts_with("ok:") => ActionResult::ok(),
        serde_json::Value::String(s) => ActionResult::fail(format!("cdp {op}: {s}")),
        serde_json::Value::Bool(true) => ActionResult::ok(),
        other => ActionResult::fail(format!("cdp {op}: unexpected result {other}")),
    }
}

/// Build JS that finds an element by backend_node_id (from the interactive
/// element index we wrote into the cortex model) and clicks it. Uses a
/// best-effort path: try matching `data-cel-backend-id` first, then walk
/// interactive elements by matching role + text/aria-label + index.
fn build_click_js(role: &str, id_part: &str) -> String {
    // For elements indexed by integer position in the interactive list, we
    // walk all matching-role elements and pick the one whose 0-based index
    // among visible interactive elements equals id_part (when parseable as
    // integer). Otherwise we treat id_part as a text/aria-label substring.
    let role_js = serde_json::to_string(role).unwrap_or_else(|_| "\"button\"".into());
    let id_js = serde_json::to_string(id_part).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(() => {{
            const role = {role_js};
            const idPart = {id_js};
            const tagFor = (r) => ({{
                button: ['button', 'a[role="button"]', 'input[type="submit"]', 'input[type="button"]'],
                link: ['a[href]'],
                input: ['input:not([type="submit"]):not([type="button"])', 'textarea'],
                textarea: ['textarea'],
                select: ['select'],
                checkbox: ['input[type="checkbox"]'],
                radio: ['input[type="radio"]'],
            }})[r] || ['*'];
            const sels = tagFor(role).join(',');
            const candidates = Array.from(document.querySelectorAll(sels))
                .filter(el => el.offsetParent !== null);
            // numeric → index into the visible candidate list
            let target = null;
            const asNum = parseInt(idPart, 10);
            if (!isNaN(asNum) && String(asNum) === idPart) {{
                target = candidates[asNum] || null;
            }}
            // text/aria fallback
            if (!target) {{
                const needle = String(idPart).toLowerCase();
                target = candidates.find(el => {{
                    const t = (el.innerText || el.value || el.getAttribute('aria-label') || '').toLowerCase();
                    return t.includes(needle);
                }}) || null;
            }}
            if (!target) return 'no-match:' + role + ':' + idPart;
            target.scrollIntoView({{ block: 'center', inline: 'center' }});
            target.click();
            return 'ok:click';
        }})()"#
    )
}

fn build_set_value_js(role: &str, id_part: &str, value: &str) -> String {
    let role_js = serde_json::to_string(role).unwrap_or_else(|_| "\"input\"".into());
    let id_js = serde_json::to_string(id_part).unwrap_or_else(|_| "\"\"".into());
    let value_js = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(() => {{
            const role = {role_js};
            const idPart = {id_js};
            const value = {value_js};
            const tagFor = (r) => ({{
                input: ['input:not([type="submit"]):not([type="button"])', 'textarea'],
                textarea: ['textarea'],
                select: ['select'],
                searchfield: ['input[type="search"]', 'input[type="text"]'],
            }})[r] || ['input', 'textarea', 'select'];
            const sels = tagFor(role).join(',');
            const candidates = Array.from(document.querySelectorAll(sels))
                .filter(el => el.offsetParent !== null);
            let target = null;
            const asNum = parseInt(idPart, 10);
            if (!isNaN(asNum) && String(asNum) === idPart) {{
                target = candidates[asNum] || null;
            }}
            if (!target) {{
                const needle = String(idPart).toLowerCase();
                target = candidates.find(el => {{
                    const t = (el.placeholder || el.name || el.id || el.getAttribute('aria-label') || '').toLowerCase();
                    return t.includes(needle);
                }}) || null;
            }}
            if (!target) return 'no-match:' + role + ':' + idPart;
            target.focus();

            const dispatchValueEvent = (el, type) => {{
                const init = {{
                    bubbles: true,
                    cancelable: type === 'beforeinput',
                    composed: true,
                    inputType: 'insertReplacementText',
                    data: String(value),
                }};
                try {{
                    if (typeof InputEvent === 'function' && (type === 'beforeinput' || type === 'input')) {{
                        return el.dispatchEvent(new InputEvent(type, init));
                    }}
                }} catch (_) {{}}
                return el.dispatchEvent(new Event(type, {{
                    bubbles: true,
                    cancelable: type === 'beforeinput',
                    composed: true,
                }}));
            }};

            const setNativeValue = (el, next) => {{
                if (el.isContentEditable || !('value' in el)) {{
                    el.textContent = String(next);
                    return;
                }}
                const tag = (el.tagName || '').toUpperCase();
                const proto = tag === 'TEXTAREA'
                    ? HTMLTextAreaElement.prototype
                    : tag === 'SELECT'
                        ? HTMLSelectElement.prototype
                        : HTMLInputElement.prototype;
                const ownSetter = Object.getOwnPropertyDescriptor(el, 'value')?.set;
                const protoSetter = Object.getOwnPropertyDescriptor(proto, 'value')?.set
                    || Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), 'value')?.set;
                if (protoSetter && ownSetter !== protoSetter) protoSetter.call(el, next);
                else if (ownSetter) ownSetter.call(el, next);
                else el.value = next;
            }};

            const commitValue = (el, next) => {{
                const proceed = dispatchValueEvent(el, 'beforeinput');
                if (!proceed) return false;
                setNativeValue(el, next);
                dispatchValueEvent(el, 'input');
                el.dispatchEvent(new Event('change', {{ bubbles: true, composed: true }}));
                return true;
            }};

            // <select> elements: HTML spec says `el.value = X` silently fails
            // unless X matches an option's `value` attribute exactly. The
            // planner is often handed the display text ("Technical Support")
            // rather than the option value ("support"), so we resolve against
            // both. Case-insensitive, trim-normalized — tolerant of whitespace
            // drift. Return 'no-option' so the caller sees a distinct signal
            // when the value didn't match any option (vs 'no-match' for
            // missing element).
            if (target.tagName === 'SELECT') {{
                const needle = String(value).trim().toLowerCase();
                const opts = Array.from(target.options);
                let picked = opts.find((o) => o.value === value);
                if (!picked) picked = opts.find((o) => (o.value || '').trim().toLowerCase() === needle);
                if (!picked) picked = opts.find((o) => (o.textContent || '').trim().toLowerCase() === needle);
                if (!picked) picked = opts.find((o) => (o.textContent || '').trim().toLowerCase().includes(needle));
                if (!picked) return 'no-option:' + role + ':' + idPart + ':' + String(value).slice(0, 40);
                setNativeValue(target, picked.value);
                dispatchValueEvent(target, 'input');
                target.dispatchEvent(new Event('change', {{ bubbles: true, composed: true }}));
                return 'ok:select:' + (picked.value || '').slice(0, 60);
            }}

            // React/Vue/Svelte sometimes track value in their own state;
            // firing beforeinput + input + change is the browser-like commit
            // path that keeps filtered lists and framework state in sync.
            if (!commitValue(target, value)) return 'canceled:beforeinput';
            return 'ok:set:' + ((target.value || target.textContent || '')).slice(0, 60);
        }})()"#
    )
}

/// Whether the action would (without CDP routing) dispatch through native
/// macOS input drivers (mouse, keyboard, AX, app activation). These actions
/// can affect any application the user has focused — they must NOT run
/// from eval/CI contexts unless `Cortex::with_native_input_unsafe()` was
/// explicitly opted into.
///
/// Pure data/control actions (Wait, Done, Fail, Extract, NotebookWrites,
/// CdpEval, Batch) never touch native input and always run.
fn action_requires_native_input(action: &PlannedAction) -> bool {
    match action {
        // System I/O — gated.
        PlannedAction::Click { .. }
        | PlannedAction::Type { .. }
        | PlannedAction::Key { .. }
        | PlannedAction::KeyCombo { .. }
        | PlannedAction::SetValue { .. }
        | PlannedAction::Scroll { .. }
        | PlannedAction::Drag { .. }
        | PlannedAction::AxAction { .. }
        | PlannedAction::ActivateApp { .. }
        | PlannedAction::Select { .. }
        | PlannedAction::Act { .. }
        | PlannedAction::Custom { .. }
        // write_cells fires osascript → system events → target app;
        // treat it like any other native-input action for gating.
        | PlannedAction::WriteCells { .. } => true,
        // Pure / control / browser-safe — always allowed.
        PlannedAction::Wait { .. }
        | PlannedAction::Done { .. }
        | PlannedAction::Fail { .. }
        | PlannedAction::Extract { .. }
        | PlannedAction::NotebookWrites { .. }
        | PlannedAction::CdpEval { .. }
        | PlannedAction::Navigate { .. }
        | PlannedAction::ReadCells { .. }
        // extract_with_fallback runs over CDP only — no native input.
        | PlannedAction::ExtractWithFallback { .. } => false,
        // Batch is a wrapper — recurse and require native input only if any
        // inner action does.
        PlannedAction::Batch { actions } => {
            actions.iter().any(action_requires_native_input)
        }
    }
}

fn action_type_str(action: &PlannedAction) -> &str {
    match action {
        PlannedAction::Click { .. } => "click",
        PlannedAction::Type { .. } => "type",
        PlannedAction::Key { .. } => "key",
        PlannedAction::KeyCombo { .. } => "key_combo",
        PlannedAction::SetValue { .. } => "set_value",
        PlannedAction::Scroll { .. } => "scroll",
        PlannedAction::Drag { .. } => "drag",
        PlannedAction::Wait { .. } => "wait",
        PlannedAction::Custom { .. } => "custom",
        PlannedAction::Extract { .. } => "extract",
        PlannedAction::Batch { .. } => "batch",
        PlannedAction::Act { .. } => "act",
        PlannedAction::Done { .. } => "done",
        PlannedAction::Fail { .. } => "fail",
        PlannedAction::AxAction { .. } => "ax_action",
        PlannedAction::ActivateApp { .. } => "activate_app",
        PlannedAction::Select { .. } => "select",
        PlannedAction::CdpEval { .. } => "cdp_eval",
        PlannedAction::Navigate { .. } => "navigate",
        PlannedAction::NotebookWrites { .. } => "notebook_writes",
        PlannedAction::WriteCells { .. } => "write_cells",
        PlannedAction::ReadCells { .. } => "read_cells",
        PlannedAction::ExtractWithFallback { .. } => "extract_with_fallback",
    }
}

/// Result of `Cortex::validate_targets`. Empty `missing` ⇒ all targets
/// were found in the provided context.
#[derive(Debug, Clone)]
pub struct TargetValidation {
    pub missing: Vec<String>,
}

impl TargetValidation {
    pub fn is_ok(&self) -> bool {
        self.missing.is_empty()
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
}

/// Browser app names recognized for CDP-aware activation.
const CDP_BROWSERS: &[&str] = &[
    "chrome", "chromium", "brave", "edge", "opera", "vivaldi", "arc",
];

/// Injected as a prelude to every PlannedAction::CdpEval. Patches two
/// gotchas that LLMs routinely hit when driving browser forms through JS:
///
/// 1. `<select>.value = X` silently no-ops when X isn't an option's `value`
///    attribute. Planners frequently supply display text ("Technical
///    Support") rather than the underlying value ("support"). We resolve
///    both and set the canonical value.
///
/// 2. `<form>.submit()` programmatic submit BYPASSES the submit event,
///    meaning preventDefault-based handlers (which show success UI, run
///    client-side validation, etc.) never fire. We wrap it so the submit
///    event dispatches first — if nothing prevents it, the native submit
///    still runs as before. Net effect: `form.submit()` now behaves like
///    clicking the submit button for handler-firing purposes.
///
/// Both patches are idempotent (guard flag) and auto-reinstall after
/// navigation (globals evaporate with the page). Patches are best-effort
/// — a failure in Object.defineProperty etc. silently falls through to
/// native behavior rather than blocking the caller's eval.
const CEL_SELECT_PATCH_PRELUDE: &str = r#"(() => {
    if (window.__celSelectPatched) return;
    try {
        // ── 1. Patch <select>.value for display-text assignments. ──
        const selProto = HTMLSelectElement.prototype;
        const desc = Object.getOwnPropertyDescriptor(selProto, 'value');
        if (desc && desc.set) {
            const originalSet = desc.set;
            const originalGet = desc.get;
            Object.defineProperty(selProto, 'value', {
                configurable: true,
                enumerable: desc.enumerable,
                get() { return originalGet.call(this); },
                set(v) {
                    const opts = Array.from(this.options);
                    if (opts.some((o) => o.value === v)) {
                        originalSet.call(this, v);
                        this.dispatchEvent(new Event('change', { bubbles: true }));
                        return;
                    }
                    const needle = String(v).trim().toLowerCase();
                    const match = opts.find((o) => (o.value || '').trim().toLowerCase() === needle)
                        || opts.find((o) => (o.textContent || '').trim().toLowerCase() === needle)
                        || opts.find((o) => (o.textContent || '').trim().toLowerCase().includes(needle));
                    if (match) {
                        originalSet.call(this, match.value);
                        this.dispatchEvent(new Event('change', { bubbles: true }));
                    } else {
                        originalSet.call(this, v);
                    }
                },
            });
        }
    } catch (e) {}

    try {
        // ── 2. Patch <form>.submit() so it fires a submit event first. ──
        // HTMLFormElement.submit() by spec BYPASSES the submit event,
        // which means preventDefault handlers that show success UI
        // never run. Wrap it: dispatch a cancelable submit event, then
        // fall through to native submit only if not prevented. Pages
        // that use form.submit() without handlers behave identically;
        // pages that have handlers now respect them.
        const formProto = HTMLFormElement.prototype;
        const originalSubmit = formProto.submit;
        formProto.submit = function () {
            const ev = new Event('submit', { bubbles: true, cancelable: true });
            const proceeded = this.dispatchEvent(ev);
            if (proceeded && !ev.defaultPrevented) {
                originalSubmit.call(this);
            }
        };
    } catch (e) {}

    window.__celSelectPatched = true;
})();"#;

/// Activate an app using AppleScript and verify it became frontmost.
/// For browsers, prefer activating CEL's dedicated CDP browser instance when
/// one already exists. This avoids drifting to a different Chrome instance.
fn activate_app_with_verification(
    app_name: &str,
) -> Result<crate::adapter::ActionResult, CortexError> {
    use crate::adapter::ActionResult;

    let is_browser = CDP_BROWSERS
        .iter()
        .any(|b| app_name.to_lowercase().contains(b));

    let activated_preferred_browser = is_browser && cel_cdp::activate_preferred_browser_target();

    if !activated_preferred_browser {
        // Escalating-aggression activation. macOS apps don't always
        // "win" frontmost against whatever process currently has focus
        // — a plain `tell app to activate` can silently lose the race
        // when the runtime is spawned from a session where another app
        // keeps grabbing events. So we fire three things in order:
        //   1. AppleScript activate to launch / wake the app.
        //   2. `open -a` as a safety net for apps that ignore AS.
        //   3. `System Events` sets `frontmost := true` on the process
        //      directly, which is the closest AppleScript gets to
        //      NSRunningApplication.activateIgnoringOtherApps. This is
        //      what actually pins the app to the front when another
        //      session process is fighting for focus.
        let safe_name = app_name.replace('"', "\\\"");
        let mut activate_command = std::process::Command::new("osascript");
        activate_command.args([
            "-e",
            &format!("tell application \"{safe_name}\" to activate"),
        ]);
        let _ = command_status_with_timeout(activate_command, std::time::Duration::from_secs(2));
        let mut open_command = std::process::Command::new("open");
        open_command.arg("-a").arg(app_name);
        let _ = command_status_with_timeout(open_command, std::time::Duration::from_secs(3));
        // Force-frontmost via System Events. Run twice with a short
        // gap — the first call tends to wake the process, the second
        // actually flips frontmost once the window server catches up.
        let force_frontmost = format!(
            r#"tell application "System Events"
                 repeat with p in (every application process whose name is "{safe_name}" or name contains "{safe_name}")
                   set frontmost of p to true
                 end repeat
               end tell"#
        );
        let mut frontmost_command = std::process::Command::new("osascript");
        frontmost_command.args(["-e", &force_frontmost]);
        let _ = command_status_with_timeout(frontmost_command, std::time::Duration::from_secs(2));
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut frontmost_retry = std::process::Command::new("osascript");
        frontmost_retry.args(["-e", &force_frontmost]);
        let _ = command_status_with_timeout(frontmost_retry, std::time::Duration::from_secs(2));
    }

    // Poll to verify the app actually became frontmost. Cold-start
    // launches (Numbers, Pages, Keynote) can take 4–6s when iCloud
    // sync kicks in on first open, so the ceiling is generous —
    // failing too early was causing legitimate launches to misreport.
    let target_lower = app_name.to_lowercase();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(400));
        if let Some(frontmost) = get_frontmost_app_name() {
            if frontmost.to_lowercase().contains(&target_lower)
                || target_lower.contains(&frontmost.to_lowercase())
            {
                return Ok(ActionResult::ok());
            }
        }
    }

    // Second chance: even if another app technically holds the
    // frontmost flag (common on macOS when a modal dialog is layered
    // over a launching app), Numbers / Pages / etc. are effectively
    // "the active app" if their process is visible with a window.
    // NSWorkspace's frontmost heuristics can flicker during launch;
    // deferring to "is the app running with a visible window" gives
    // a more useful readout for the downstream sub-goal.
    if app_is_running_with_visible_window(&target_lower) {
        return Ok(ActionResult::ok());
    }

    Ok(ActionResult::fail(format!(
        "App \"{app_name}\" was activated but did not become frontmost"
    )))
}

/// Best-effort check: is the named app in the running-apps list with
/// at least one on-screen window? Useful during the narrow window
/// after `open -a` where the app exists but hasn't won frontmost yet.
fn app_is_running_with_visible_window(name_lower: &str) -> bool {
    let script = format!(
        r#"tell application "System Events"
             set hits to (name of application processes whose name contains "{}" or "{}" contains name)
             return length of hits
           end tell"#,
        name_lower, name_lower
    );
    {
        let mut command = std::process::Command::new("osascript");
        command.args(["-e", &script]);
        command_output_with_timeout(command, std::time::Duration::from_secs(1))
    }
    .and_then(|out| {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        s.parse::<u32>().ok()
    })
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// Get the name of the current frontmost application via System Events.
/// True when the currently-frontmost macOS app name matches one of the
/// CDP-capable browsers CEL knows about. Used by the focus gate so
/// native-input actions don't fire into the wrong window.
fn frontmost_is_browser() -> bool {
    let Some(name) = get_frontmost_app_name() else {
        return false;
    };
    let lower = name.to_lowercase();
    CDP_BROWSERS.iter().any(|b| lower.contains(b))
}

fn get_frontmost_app_name() -> Option<String> {
    let mut command = std::process::Command::new("osascript");
    command.args([
        "-e",
        "tell application \"System Events\" to name of first process whose frontmost is true",
    ]);
    let output = command_output_with_timeout(command, std::time::Duration::from_secs(1))?;
    if output.status.success() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn should_attempt_numbers_document_bootstrap(error: &InputError) -> bool {
    matches!(
        error,
        InputError::ScriptingUnavailable { app, .. } if app.eq_ignore_ascii_case("Numbers")
    )
}

#[cfg(target_os = "macos")]
fn bootstrap_numbers_document() -> Result<(), String> {
    let mut attempts = Vec::new();

    match activate_app_with_verification("Numbers") {
        Ok(_) => attempts.push("activated Numbers".to_string()),
        Err(err) => attempts.push(format!("activate_app failed: {err}")),
    }

    if numbers_document_ready() {
        attempts.push("existing Numbers document already scriptable".into());
        return Ok(());
    }

    if let Some(document_path) = NUMBERS_DOCUMENT_BOOTSTRAP_CANDIDATES
        .iter()
        .copied()
        .find(|path| std::path::Path::new(path).exists())
    {
        let mut open_command = std::process::Command::new("open");
        open_command.arg(document_path);
        match command_status_with_timeout(open_command, std::time::Duration::from_secs(5)) {
            Some(status) if status.success() => {
                attempts.push(format!("opened sample document {}", document_path));
                std::thread::sleep(std::time::Duration::from_millis(1400));
                record_numbers_reactivation(&mut attempts);
                if numbers_document_ready() {
                    return Ok(());
                }
            }
            Some(status) => attempts.push(format!(
                "open sample document exited with status {:?}",
                status.code()
            )),
            None => attempts.push("open sample document timed out".into()),
        }
    } else {
        attempts.push("no bundled Numbers sample document found".into());
    }

    if let Some(template_path) = NUMBERS_BLANK_TEMPLATE_CANDIDATES
        .iter()
        .copied()
        .find(|path| std::path::Path::new(path).exists())
    {
        let mut open_command = std::process::Command::new("open");
        open_command.arg(template_path);
        match command_status_with_timeout(open_command, std::time::Duration::from_secs(5)) {
            Some(status) if status.success() => {
                attempts.push(format!("opened template {}", template_path));
                std::thread::sleep(std::time::Duration::from_millis(1200));
                if numbers_document_ready() {
                    record_numbers_reactivation(&mut attempts);
                    return Ok(());
                }
            }
            Some(status) => attempts.push(format!(
                "open template exited with status {:?}",
                status.code()
            )),
            None => attempts.push("open template timed out".into()),
        }
    } else {
        attempts.push("no bundled Numbers blank template found".into());
    }

    if let Some(clicked) = click_numbers_dialog_button(&[
        "New Spreadsheet",
        "New Document",
        "Create Document",
        "Create",
        "Blank",
    ]) {
        attempts.push(format!("clicked {}", clicked));
        record_numbers_reactivation(&mut attempts);
        if numbers_document_ready() {
            return Ok(());
        }
    }

    if send_system_keystroke("n", true) {
        attempts.push("sent Cmd+N".into());
        std::thread::sleep(std::time::Duration::from_millis(800));
        if let Some(clicked) = click_numbers_dialog_button(&[
            "New Spreadsheet",
            "New Document",
            "Create Document",
            "Create",
            "Blank",
        ]) {
            attempts.push(format!("clicked {}", clicked));
            record_numbers_reactivation(&mut attempts);
            if numbers_document_ready() {
                return Ok(());
            }
        }
    } else {
        attempts.push("failed to send Cmd+N".into());
    }

    if send_system_key_code(36) {
        attempts.push("sent Return".into());
        std::thread::sleep(std::time::Duration::from_millis(600));
        record_numbers_reactivation(&mut attempts);
        if numbers_document_ready() {
            return Ok(());
        }
        return Err(attempts.join("; "));
    }

    attempts.push("failed to send Return".into());
    Err(attempts.join("; "))
}

#[cfg(target_os = "macos")]
fn record_numbers_reactivation(attempts: &mut Vec<String>) {
    match activate_app_with_verification("Numbers") {
        Ok(_) => attempts.push("re-activated Numbers".into()),
        Err(err) => attempts.push(format!("re-activate Numbers failed: {err}")),
    }
}

#[cfg(target_os = "macos")]
fn numbers_document_ready() -> bool {
    let probe_refs = vec![String::from("A1")];
    cel_input::read_numbers_cells(None, None, &probe_refs).is_ok()
}

#[cfg(target_os = "macos")]
fn dismiss_numbers_dialog_if_present() {
    if let Some(label) = click_numbers_dialog_button_via_ax(&["Cancel"]) {
        trace!(button = %label, "dismissed Numbers dialog via AX cancel");
        std::thread::sleep(std::time::Duration::from_millis(300));
        return;
    }
    if let Some(label) = click_numbers_dialog_button_via_system_events(&["Cancel"]) {
        trace!(button = %label, "dismissed Numbers dialog via System Events cancel");
        std::thread::sleep(std::time::Duration::from_millis(300));
        return;
    }
    if send_system_key_code(53) {
        trace!("dismissed Numbers dialog via Escape");
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

#[cfg(target_os = "macos")]
fn click_numbers_dialog_button(candidates: &[&str]) -> Option<String> {
    for _ in 0..5 {
        if let Some(label) = click_numbers_dialog_button_via_ax(candidates) {
            return Some(label);
        }
        if let Some(label) = click_numbers_dialog_button_via_system_events(candidates) {
            return Some(label);
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    None
}

#[cfg(target_os = "macos")]
fn click_numbers_dialog_button_via_ax(candidates: &[&str]) -> Option<String> {
    let tree = cel_accessibility::create_tree();
    for candidate in candidates {
        let matches = tree
            .find_elements(Some(&ElementRole::Button), Some(candidate))
            .ok()?;
        for element in matches {
            if !element.state.enabled || !element.state.visible {
                continue;
            }
            if tree.perform_action(&element.id, "click").ok()? {
                let label = element.label.unwrap_or_else(|| (*candidate).to_string());
                return Some(label);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn click_numbers_dialog_button_via_system_events(candidates: &[&str]) -> Option<String> {
    let quoted_candidates = candidates
        .iter()
        .map(|candidate| {
            let escaped = candidate.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(", ");

    let script = format!(
        r#"set targetNames to {{{quoted_candidates}}}
tell application "System Events"
  repeat with p in (every application process whose name is "Numbers" or name contains "Numbers")
    repeat with w in windows of p
      repeat with uiElem in entire contents of w
        try
          set buttonName to (name of uiElem) as text
          repeat with targetName in targetNames
            if buttonName contains (targetName as text) then
              click uiElem
              return buttonName
            end if
          end repeat
        end try
      end repeat
    end repeat
  end repeat
end tell
return ""#
    );

    let mut command = std::process::Command::new("osascript");
    command.args(["-e", &script]);
    let output = command_output_with_timeout(command, std::time::Duration::from_secs(2))?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

#[cfg(target_os = "macos")]
fn send_system_keystroke(key: &str, command_down: bool) -> bool {
    let escaped = key.replace('\\', "\\\\").replace('"', "\\\"");
    let script = if command_down {
        format!(r#"tell application "System Events" to keystroke "{escaped}" using command down"#)
    } else {
        format!(r#"tell application "System Events" to keystroke "{escaped}""#)
    };
    let mut command = std::process::Command::new("osascript");
    command.args(["-e", &script]);
    command_status_with_timeout(command, std::time::Duration::from_secs(2))
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn send_system_key_code(key_code: u16) -> bool {
    let mut command = std::process::Command::new("osascript");
    command.args([
        "-e",
        &format!("tell application \"System Events\" to key code {key_code}"),
    ]);
    command_status_with_timeout(command, std::time::Duration::from_secs(2))
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_extract_expression_wraps_bare_css_selector() {
        let expr = build_extract_expression("fin-streamer[data-field='price']");
        // Should wrap with querySelector + textContent + null-guard
        assert!(expr.contains("document.querySelector"));
        assert!(expr.contains("textContent"));
        // Original selector must be present, with the inner ' escaped
        // for JS string embedding.
        assert!(expr.contains(r"fin-streamer[data-field=\'price\']"));
    }

    #[test]
    fn build_extract_expression_passes_raw_js_through() {
        let js = "(function() { return document.title; })()";
        assert_eq!(build_extract_expression(js), js);
        let arrow = "(() => document.title)()";
        assert_eq!(build_extract_expression(arrow), arrow);
    }

    #[test]
    fn build_extract_expression_supports_contains_and_has_selectors() {
        let expr = build_extract_expression("tr:has(td:contains('EMP-0742')) td:nth-child(2)");
        assert!(expr.contains("rowMatch"));
        assert!(expr.contains("adjacentMatch"));
        assert!(expr.contains("siblingNthMatch"));
        assert!(expr.contains("containsOnlyMatch"));
        assert!(expr.contains("EMP-0742"));
    }

    #[test]
    fn parse_extracted_float_strips_currency() {
        let parsed = parse_extracted("$108,432.50", "float").unwrap();
        // Numbers round-trip via serde_json::Number — compare as f64.
        assert_eq!(parsed.as_f64().unwrap(), 108432.50);
    }

    #[test]
    fn parse_extracted_int_handles_negative() {
        let parsed = parse_extracted("-42", "int").unwrap();
        assert_eq!(parsed.as_i64().unwrap(), -42);
    }

    #[test]
    fn parse_extracted_text_trims() {
        let parsed = parse_extracted("  hello  ", "text").unwrap();
        assert_eq!(parsed.as_str().unwrap(), "hello");
    }

    #[test]
    fn parse_extracted_unknown_hint_falls_back_to_text() {
        let parsed = parse_extracted("BTC", "weirdo_format").unwrap();
        assert_eq!(parsed.as_str().unwrap(), "BTC");
    }

    #[test]
    fn cdp_value_to_string_rejects_null_and_empty() {
        assert!(cdp_value_to_string(&serde_json::Value::Null).is_none());
        assert!(cdp_value_to_string(&serde_json::Value::String(String::new())).is_none());
    }

    #[test]
    fn cdp_value_to_string_returns_str() {
        let v = serde_json::Value::String("hello".into());
        assert_eq!(cdp_value_to_string(&v).unwrap(), "hello");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn numbers_bootstrap_only_triggers_for_numbers_scripting_unavailable() {
        assert!(should_attempt_numbers_document_bootstrap(
            &InputError::ScriptingUnavailable {
                app: "Numbers".into(),
                reason: "no open document".into(),
            }
        ));
        assert!(!should_attempt_numbers_document_bootstrap(
            &InputError::Failed("random".into())
        ));
        assert!(!should_attempt_numbers_document_bootstrap(
            &InputError::ScriptingUnavailable {
                app: "Pages".into(),
                reason: "no open document".into(),
            }
        ));
    }

    #[test]
    fn test_context_fingerprint_stable() {
        let ctx = ScreenContext {
            app: "Test".into(),
            window: "Window".into(),
            elements: vec![],
            network_events: vec![],
            http_events: vec![],
            timestamp_ms: 0,
            screen_width: None,
            screen_height: None,
            clipboard: None,
            window_list: vec![],
            audio: None,
            power: None,
            running_apps: vec![],
            recent_files: vec![],
            transcripts: vec![],
        };
        assert_eq!(context_fingerprint(&ctx), context_fingerprint(&ctx));
    }

    #[test]
    fn test_context_fingerprint_differs() {
        let ctx1 = ScreenContext {
            app: "App1".into(),
            window: "W".into(),
            elements: vec![],
            network_events: vec![],
            http_events: vec![],
            timestamp_ms: 0,
            screen_width: None,
            screen_height: None,
            clipboard: None,
            window_list: vec![],
            audio: None,
            power: None,
            running_apps: vec![],
            recent_files: vec![],
            transcripts: vec![],
        };
        let ctx2 = ScreenContext {
            app: "App2".into(),
            ..ctx1.clone()
        };
        assert_ne!(context_fingerprint(&ctx1), context_fingerprint(&ctx2));
    }

    #[test]
    fn test_is_significant_event() {
        assert!(is_significant_event(&CelEvent::SheetCreated));
        assert!(is_significant_event(&CelEvent::LayoutChanged));
        assert!(!is_significant_event(&CelEvent::NetworkIdle));
        assert!(!is_significant_event(&CelEvent::WindowMoved));
    }

    #[test]
    fn test_cortex_new() {
        let cortex = Cortex::new("test-1".into());
        assert_eq!(cortex.id, "test-1");
        assert!(!cortex.is_running());
    }

    // ─── build_set_value_js: <select> handling (eval-smoke Fix) ───────

    #[test]
    fn set_value_js_has_dedicated_select_branch() {
        let js = build_set_value_js("select", "0", "support");
        // The select-specific branch must exist (otherwise `<select>` calls
        // silently no-op when the planner supplies a display text instead
        // of an option value).
        assert!(
            js.contains("target.tagName === 'SELECT'"),
            "expected select branch in set_value JS"
        );
        // Falls back through three lookup tiers: exact value, case-
        // insensitive value, then textContent match. All three should be
        // visible in the emitted script.
        assert!(
            js.contains("o.value === value"),
            "expected exact-value match"
        );
        assert!(
            js.contains("o.textContent"),
            "expected textContent-based fallback"
        );
        // The 'no-option' sentinel lets the runner distinguish "couldn't
        // find the option" from "couldn't find the element".
        assert!(
            js.contains("'no-option:'"),
            "expected distinct no-option error sentinel"
        );
    }

    #[test]
    fn set_value_js_input_path_unchanged_for_non_selects() {
        // Regression guard: the select branch must not swallow the input
        // path. The original `input`-role code (native setter + input +
        // change events) must still be present for textarea / input use.
        let js = build_set_value_js("input", "0", "hello");
        assert!(js.contains("setNativeValue"));
        assert!(js.contains("HTMLTextAreaElement.prototype"));
        assert!(js.contains("new InputEvent(type, init)"));
        assert!(js.contains("dispatchValueEvent(el, 'beforeinput')"));
        assert!(js.contains("dispatchValueEvent(el, 'input')"));
        assert!(js.contains("new Event('change'"));
    }
}
