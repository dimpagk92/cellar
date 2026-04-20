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
use cel_context::{CelEvent, ContextMerger, ContextWatchdog, ScreenContext};
use cel_input::{create_controller, MouseButton};
use cel_planner::PlannedAction;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};
use tracing::{debug, trace, warn};

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
            .map(|c| c.element.label.clone().unwrap_or_else(|| c.element.id.clone()))
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
        let mut guard = self.adapters.try_write().expect("adapters lock not contested before boot");
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
            if let Err(e) = audio.lock().unwrap_or_else(|p| p.into_inner()).start(config) {
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
            let mut element_adapter_index: std::collections::HashMap<String, String> = std::collections::HashMap::new();

            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(tick_ms));

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
                    let transcripts = audio.lock().unwrap_or_else(|p| p.into_inner()).drain_transcripts();
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
                                }.to_string(),
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
                    let should_be_active = adapter.matches_app(current_app);

                    match (adapter.state, should_be_active) {
                        (
                            crate::adapter::AdapterState::Inactive | crate::adapter::AdapterState::Error,
                            true,
                        ) => {
                            // Activate (or retry after a transient activation error).
                            if let Err(e) = adapter.driver.activate().await {
                                warn!(cortex_id = %cortex_id, adapter = %adapter.driver.manifest().name, "Adapter activation failed: {e}");
                                adapter.state = crate::adapter::AdapterState::Error;
                            } else {
                                adapter.state = crate::adapter::AdapterState::Active;
                                debug!(cortex_id = %cortex_id, adapter = %adapter.driver.manifest().name, "Adapter activated");
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
                    if adapter.state == crate::adapter::AdapterState::Active && adapter.should_read(tick_ms) {
                        match adapter.driver.get_context().await {
                            Ok(elements) => {
                                let adapter_name = adapter.driver.manifest().name.clone();
                                let confidence = adapter.driver.manifest().context.confidence;
                                for mut el in elements {
                                    // Tag element with adapter source
                                    el.source = cel_context::ContextSource::NativeApi;
                                    el.confidence = confidence;
                                    element_adapter_index.insert(el.id.clone(), adapter_name.clone());
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
                let vision_needed = actionable_count < SPARSE_CONTEXT_THRESHOLD
                    || consecutive_action_failures >= 2;

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
                        .map_or(false, |a| a.timestamp < ttl_cutoff)
                    {
                        m.anomaly_queue.pop_front();
                    }
                    while m.anomaly_queue.len() > MAX_ANOMALY_QUEUE {
                        m.anomaly_queue.pop_front();
                    }

                    // Vision + meta
                    m.vision_needed = vision_needed;
                    m.cycle_count += 1;
                    m.age_ms = 0; // Just updated
                    m.uptime_ms = now.saturating_sub(boot_time);

                    // Adapter state
                    m.element_adapter_index = element_adapter_index.clone();
                    m.active_adapters = active_adapter_names.clone();
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
                        ActionResult::fail(format!("Element \"{target_id}\" has no actionable bounds"))
                    }
                } else {
                    ActionResult::fail(format!("Element \"{target_id}\" not found"))
                }
            }
            PlannedAction::Type { target_id, text } => {
                let mut controller = create_controller()
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
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
                        return Ok(ActionResult::fail(format!("Element \"{target_id}\" not found")));
                    }
                }
                controller
                    .type_text(text)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::Key { key } => {
                let mut controller = create_controller()
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                controller
                    .key_press(key)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::KeyCombo { keys } => {
                let mut controller = create_controller()
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
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
                let mut controller = create_controller()
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
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
                    return Ok(ActionResult::fail(format!("Element \"{from_target_id}\" not found")));
                };
                let Some(to_element) = find_element(context, to_target_id) else {
                    return Ok(ActionResult::fail(format!("Element \"{to_target_id}\" not found")));
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
                let mut controller = create_controller()
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                controller
                    .drag(from_x, from_y, to_x, to_y)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::Wait { ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(*ms as u64)).await;
                ActionResult::ok()
            }
            PlannedAction::AxAction { target_id, action } => {
                if try_ax_action(target_id, action)? {
                    ActionResult::ok()
                } else {
                    ActionResult::fail(format!("AX action \"{action}\" failed on \"{target_id}\""))
                }
            }
            PlannedAction::ActivateApp { app_name } => {
                activate_app_with_verification(app_name)?
            }
            PlannedAction::Select {
                from_x,
                from_y,
                to_x,
                to_y,
            } => {
                let mut controller = create_controller()
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                controller
                    .drag(*from_x, *from_y, *to_x, *to_y)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::Custom { adapter, action, params } => {
                // Route to registered adapter if available
                let adapters = self.adapters.read().await;
                if let Some(registered) = adapters.iter().find(|a| a.driver.manifest().name == *adapter) {
                    if registered.state == crate::adapter::AdapterState::Active {
                        match registered.driver.execute(action, params.clone()).await {
                            Ok(r) => r,
                            Err(e) => ActionResult::fail(format!("Adapter \"{adapter}\" error: {e}")),
                        }
                    } else {
                        ActionResult::fail(format!("Adapter \"{adapter}\" is not active (state: {:?})", registered.state))
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
                            i + 1, actions.len(),
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
                    el.state.visible && !el.actions.is_empty()
                        && el.label.as_ref().map_or(false, |l| lower.contains(&l.to_lowercase()))
                }) {
                    let click = PlannedAction::Click { target_id: el.id.clone() };
                    return Box::pin(self.execute(&click, context)).await;
                }
                ActionResult::fail(format!("Could not resolve: {instruction}"))
            }
            PlannedAction::CdpEval { expression } => {
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
                                Ok(result) => Ok(serde_json::to_string(&result).unwrap_or_default()),
                                Err(e) => Err(format!("CDP eval failed: {e}")),
                            },
                            None => Err("No CDP target available".into()),
                        }
                    })
                }) {
                    Ok(result) => ActionResult { success: true, error: None, data: Some(serde_json::Value::String(result)) },
                    Err(e) => ActionResult::fail(e),
                }
            }
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
}

fn find_element<'a>(context: &'a ScreenContext, target_id: &str) -> Option<&'a cel_context::ContextElement> {
    context.elements.iter().find(|el| el.id == target_id)
}

fn bounds_center(element: &cel_context::ContextElement) -> Option<(i32, i32)> {
    let bounds = element.bounds.as_ref()?;
    Some((
        bounds.x + (bounds.width as i32 / 2),
        bounds.y + (bounds.height as i32 / 2),
    ))
}

fn try_ax_action(target_id: &str, action: &str) -> Result<bool, CortexError> {
    let tree = cel_accessibility::create_tree();
    tree.perform_action(target_id, action)
        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))
}

fn try_set_value(target_id: &str, value: &str) -> Result<bool, CortexError> {
    let tree = cel_accessibility::create_tree();
    tree.set_value(target_id, value)
        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))
}

/// Try to dispatch the action through CDP. Returns:
///  * `Ok(Some(result))` — we handled it (succeeded or failed via CDP)
///  * `Ok(None)` — not a browser-targeted action; caller should fall back
///                 to the native execution path
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

    let target = match action {
        PlannedAction::Click { target_id }
        | PlannedAction::SetValue { target_id, .. }
        | PlannedAction::AxAction { target_id, .. }
        | PlannedAction::Drag { from_target_id: target_id, .. } => Some(target_id.as_str()),
        PlannedAction::Type { target_id: Some(t), .. } => Some(t.as_str()),
        _ => None,
    };
    let Some(target) = target else { return Ok(None) };
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
        PlannedAction::Click { .. }
        | PlannedAction::AxAction { .. } => {
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

fn check_cdp_ok(res: serde_json::Value, op: &'static str) -> crate::adapter::ActionResult {
    use crate::adapter::ActionResult;
    let v = res.get("result").and_then(|r| r.get("value")).cloned().unwrap_or(res);
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
                target.value = picked.value;
                target.dispatchEvent(new Event('input', {{ bubbles: true }}));
                target.dispatchEvent(new Event('change', {{ bubbles: true }}));
                return 'ok:select:' + (picked.value || '').slice(0, 60);
            }}

            // React/Vue/Svelte sometimes track value in their own state;
            // setting via the native setter and dispatching input is the
            // canonical safe pattern for <input>/<textarea>.
            const proto = Object.getPrototypeOf(target);
            const setter = Object.getOwnPropertyDescriptor(proto, 'value');
            if (setter && setter.set) setter.set.call(target, value);
            else target.value = value;
            target.dispatchEvent(new Event('input', {{ bubbles: true }}));
            target.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return 'ok:set:' + (target.value || '').slice(0, 60);
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
        | PlannedAction::Custom { .. } => true,
        // Pure / control / browser-safe — always allowed.
        PlannedAction::Wait { .. }
        | PlannedAction::Done { .. }
        | PlannedAction::Fail { .. }
        | PlannedAction::Extract { .. }
        | PlannedAction::NotebookWrites { .. }
        | PlannedAction::CdpEval { .. } => false,
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
        PlannedAction::NotebookWrites { .. } => "notebook_writes",
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
const CDP_BROWSERS: &[&str] = &["chrome", "chromium", "brave", "edge", "opera", "vivaldi", "arc"];

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
        // Use AppleScript to activate — more reliable than `open -a` for
        // already-running apps like Finder, and works for all apps.
        let activate_script = format!(
            "tell application \"{}\" to activate",
            app_name.replace('"', "\\\"")
        );
        let status = std::process::Command::new("osascript")
            .args(["-e", &activate_script])
            .status()
            .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;

        if !status.success() {
            // Fallback: try `open -a` (handles apps that don't respond to AppleScript)
            let fallback = std::process::Command::new("open")
                .arg("-a")
                .arg(app_name)
                .status()
                .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
            if !fallback.success() {
                return Ok(ActionResult::fail(format!(
                    "Failed to activate app \"{app_name}\""
                )));
            }
        }
    }

    // Poll to verify the app actually became frontmost (up to 2 seconds)
    let target_lower = app_name.to_lowercase();
    for _ in 0..4 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(frontmost) = get_frontmost_app_name() {
            if frontmost.to_lowercase().contains(&target_lower)
                || target_lower.contains(&frontmost.to_lowercase())
            {
                return Ok(ActionResult::ok());
            }
        }
    }

    Ok(ActionResult::fail(format!(
        "App \"{app_name}\" was activated but did not become frontmost"
    )))
}

/// Get the name of the current frontmost application via System Events.
fn get_frontmost_app_name() -> Option<String> {
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to name of first process whose frontmost is true",
        ])
        .output()
        .ok()?;
    if output.status.success() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(js.contains("Object.getPrototypeOf(target)"));
        assert!(js.contains("setter.set.call(target, value)"));
        assert!(js.contains("new Event('input'"));
        assert!(js.contains("new Event('change'"));
    }
}
