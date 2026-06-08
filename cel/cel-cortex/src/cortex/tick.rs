//! The background tick loop and its event-forwarding helpers.
//!
//! `Cortex::boot` drives the perception cycle: drain the watchdog / AX / audio
//! / network / input streams, merge them into a `ScreenContext`, diff, classify
//! stability, and publish the `MentalModel`. The `*_to_bridge_event` free fns
//! translate observed events into `cellar_types::Event`s forwarded to the
//! daemon bridge. Also holds `refresh_now` (out-of-band tick) and `wait_for_url`.

use super::cdp::decode_png_to_frame;
use super::focus::frontmost_is_browser;
use super::targets::normalise_url;
use super::*;

/// Check if a CelEvent is significant (triggers full context read).
pub(crate) fn is_significant_event(event: &CelEvent) -> bool {
    matches!(
        event,
        CelEvent::TreeChanged { .. }
            | CelEvent::ValueChanged { .. }
            | CelEvent::WindowCreated { .. }
            | CelEvent::SheetCreated
            | CelEvent::LayoutChanged
    )
}

/// Add a data field only when the value is present — keeps `Event.data` free of
/// empty-string noise so rule expressions can rely on `data.x exists`.
fn with_opt(ev: Event, key: &str, val: Option<&str>) -> Event {
    match val {
        Some(v) => ev.with_data(key, v),
        None => ev,
    }
}

/// Stable snake_case label for an audio source, used in `audio_*` bridge events.
fn audio_source_str(src: &cel_audio::AudioSource) -> &'static str {
    match src {
        cel_audio::AudioSource::Microphone => "microphone",
        cel_audio::AudioSource::SystemOutput => "system_output",
        cel_audio::AudioSource::Both => "both",
    }
}

/// Stable snake_case label for a mouse button, used in `pointer_*` bridge events.
fn mouse_button_str(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    }
}

/// Map a raw CDP event frame to a daemon bridge [`Event`]. Returns `None` for
/// event methods we don't forward. Pure, for unit testing.
pub(crate) fn cdp_event_to_bridge_event(frame: &serde_json::Value) -> Option<Event> {
    let method = frame.get("method").and_then(|m| m.as_str())?;
    match method {
        "Page.loadEventFired" => {
            let mut ev = Event::now(EventSource::CortexCdp, EventKind::PageLoaded);
            if let Some(ts) = frame
                .get("params")
                .and_then(|p| p.get("timestamp"))
                .and_then(|t| t.as_f64())
            {
                ev = ev.with_data("timestamp", ts);
            }
            Some(ev)
        }
        _ => None,
    }
}

/// Map a captured input event to the daemon bridge [`Event`]. Pure, for unit
/// testing. `forward_content` gates whether typed characters are attached to
/// `KeyboardInput`; keycodes and pointer coordinates are always included.
pub(crate) fn input_to_bridge_event(ci: &cel_input::CapturedInput, forward_content: bool) -> Event {
    use cel_input::CapturedInput as I;
    match ci {
        I::KeyDown { keycode, chars } => {
            let mut ev = Event::now(EventSource::CortexInput, EventKind::KeyboardInput)
                .with_data("keycode", *keycode as u64)
                .with_data("pressed", true);
            if forward_content {
                if let Some(text) = chars {
                    ev = ev.with_data("text", text.clone());
                }
            }
            ev
        }
        I::KeyUp { keycode } => Event::now(EventSource::CortexInput, EventKind::KeyboardInput)
            .with_data("keycode", *keycode as u64)
            .with_data("pressed", false),
        I::MouseMoved { x, y } => Event::now(EventSource::CortexInput, EventKind::PointerMoved)
            .with_data("x", *x)
            .with_data("y", *y),
        I::MouseButton {
            button,
            pressed,
            x,
            y,
        } => Event::now(EventSource::CortexInput, EventKind::PointerButton)
            .with_data("button", mouse_button_str(*button))
            .with_data("pressed", *pressed)
            .with_data("x", *x)
            .with_data("y", *y),
        I::Scroll { dx, dy } => Event::now(EventSource::CortexInput, EventKind::PointerScroll)
            .with_data("delta_x", *dx)
            .with_data("delta_y", *dy),
    }
}

/// Map a push-based [`AccessibilityEvent`] to the daemon event-bus [`Event`]
/// the rule matcher consumes. Every AX variant maps to a kind (the user opted
/// into forwarding the full stream), so this match is intentionally exhaustive:
/// when `AccessibilityEvent` grows a variant, this fails to compile until the
/// `cellar-types` catalog and this mapping are both updated.
pub(crate) fn ax_event_to_bridge_event(ax: &AccessibilityEvent) -> Event {
    use AccessibilityEvent as A;
    let mk = |kind| Event::now(EventSource::CortexAx, kind);
    match ax {
        // Application lifecycle
        A::AppActivated { app_name } => {
            with_opt(mk(EventKind::AppFocused), "app", app_name.as_deref())
        }
        A::AppDeactivated { app_name } => {
            with_opt(mk(EventKind::AppDeactivated), "app", app_name.as_deref())
        }
        A::AppHidden { app_name } => with_opt(mk(EventKind::AppHidden), "app", app_name.as_deref()),
        A::AppShown { app_name } => with_opt(mk(EventKind::AppShown), "app", app_name.as_deref()),
        // Window lifecycle
        A::WindowCreated { title } => {
            with_opt(mk(EventKind::WindowOpened), "title", title.as_deref())
        }
        A::WindowMoved => mk(EventKind::WindowMoved),
        A::WindowResized => mk(EventKind::WindowResized),
        A::WindowMinimized => mk(EventKind::WindowMinimized),
        A::WindowRestored => mk(EventKind::WindowRestored),
        A::MainWindowChanged => mk(EventKind::MainWindowChanged),
        // Menus / sheets
        A::MenuOpened => mk(EventKind::MenuOpened),
        A::MenuClosed => mk(EventKind::MenuClosed),
        A::SheetCreated => mk(EventKind::SheetOpened),
        // Element-level
        A::FocusChanged { element_id } => with_opt(
            mk(EventKind::FocusChanged),
            "element_id",
            element_id.as_deref(),
        ),
        A::ValueChanged {
            element_id,
            new_value,
        } => with_opt(
            mk(EventKind::ValueChanged).with_data("element_id", element_id.clone()),
            "new_value",
            new_value.as_deref(),
        ),
        A::TitleChanged {
            element_id,
            new_title,
        } => with_opt(
            with_opt(
                mk(EventKind::TitleChanged),
                "element_id",
                element_id.as_deref(),
            ),
            "new_title",
            new_title.as_deref(),
        ),
        A::SelectionChanged => mk(EventKind::SelectionChanged),
        A::RowCountChanged => mk(EventKind::RowCountChanged),
        A::LayoutChanged => mk(EventKind::LayoutChanged),
        A::ElementDestroyed => mk(EventKind::ElementDestroyed),
        A::AnnouncementRequested { text } => with_opt(
            mk(EventKind::AnnouncementRequested),
            "text",
            text.as_deref(),
        ),
        A::HelpTagShown => mk(EventKind::HelpTagShown),
    }
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

impl Cortex {
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

        // Install CDP-screenshot vision fallback so headless Linux hosts
        // (no monitors, no X server) still get vision when a browser is
        // bound. The closure captures an Arc-clone of the cdp_client
        // slot so a LATER `bind_browser_cdp_url` is visible to it at
        // call time. Without this, every benchmark / agent run on the
        // Hetzner box scored 0/5 partly because the planner never saw
        // a screenshot — gemini-flash had to reason from DOM text only.
        let cdp_handle = Arc::clone(&self.cdp_client);
        merger = merger.with_cdp_screenshot_fallback(move || {
            let client = {
                let guard = cdp_handle.lock().ok()?;
                Arc::clone(guard.as_ref()?)
            };
            // capture_screenshot is async; run it in whatever runtime
            // happens to be current. On the bench harness the merger
            // tick runs inside tokio::main, so try_current() succeeds.
            let handle = tokio::runtime::Handle::try_current().ok()?;
            let png = tokio::task::block_in_place(|| handle.block_on(client.capture_screenshot()))
                .ok()?;
            decode_png_to_frame(&png).ok()
        });

        // Start AXObserver for push-based events
        if let Err(e) = observer.start_observing() {
            warn!(cortex_id = %self.id, "AXObserver start failed (polling-only mode): {}", e);
        }

        // Start audio capture if configured
        if let Some(ref audio) = self.audio_capture {
            let config = self.audio_config.clone().unwrap_or_default();
            let source_label = audio_source_str(&config.source);
            match audio
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .start(config)
            {
                Ok(()) => {
                    // Activity event — no audio content, safe to forward always.
                    if let Some(ref bridge) = self.daemon_bridge {
                        bridge.forward(
                            Event::now(EventSource::CortexAudio, EventKind::AudioCaptureStarted)
                                .with_data("source", source_label),
                        );
                    }
                }
                Err(e) => {
                    warn!(cortex_id = %self.id, "Audio capture start failed: {}", e);
                }
            }
        }

        // Start network monitor if configured
        if let Some(ref net) = self.network_monitor {
            if let Err(e) = net.lock().unwrap_or_else(|p| p.into_inner()).start() {
                warn!(cortex_id = %self.id, "Network monitor start failed: {}", e);
            }
        }

        // Start input capture if configured
        if let Some(ref input) = self.input_capture {
            if let Err(e) = input.lock().unwrap_or_else(|p| p.into_inner()).start() {
                warn!(cortex_id = %self.id, "Input capture start failed: {}", e);
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
        let network_monitor = self.network_monitor.clone();
        let forward_audio_transcripts = self.forward_audio_transcripts;
        let input_capture = self.input_capture.clone();
        let forward_input_content = self.forward_input_content;
        let tick_count_mirror = Arc::clone(&self.tick_count);
        let last_tick_ms_mirror = Arc::clone(&self.last_tick_ms);
        let refresh_notify = Arc::clone(&self.refresh_notify);
        let daemon_bridge = self.daemon_bridge.clone();
        let cdp_for_bridge = Arc::clone(&self.cdp_client);

        let handle = tokio::spawn(async move {
            let mut watchdog = ContextWatchdog::new();
            let mut expected_app = expected_app;
            let mut consecutive_action_failures: u32 = 0;
            let mut last_event_ms: Option<u64> = None;
            let mut last_significant_event_ms: Option<u64> = None;
            let mut element_adapter_index: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            // Last URL forwarded to the daemon bridge — used to suppress duplicates.
            let mut last_bridge_url: Option<String> = None;
            // Whether Page-domain events are enabled on the current CDP client.
            // Reset whenever the client is (re)bound so we re-enable Page.
            let mut cdp_page_enabled = false;
            // Throttle for network connection polling: lsof is a subprocess, so
            // we poll ~1s rather than on every ~200ms tick.
            let mut last_net_poll_ms: u64 = 0;
            // Throttle for high-frequency pointer-move forwarding (~4/s).
            let mut last_pointer_fwd_ms: u64 = 0;
            // Throttle for ambient CDP auto-bind attempts (ms since epoch of the
            // last probe). Only used when a daemon bridge is set but no CDP
            // client is bound yet — see the bridge section in the tick body.
            let mut last_cdp_probe_ms: u64 = 0;

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
                        // Bridge: forward transcript CONTENT only when explicitly
                        // enabled — it's privacy-sensitive. Iterate by reference so
                        // the context-building consume below still owns the data.
                        if forward_audio_transcripts {
                            if let Some(ref bridge) = daemon_bridge {
                                for t in &transcripts {
                                    let mut ev = Event::now(
                                        EventSource::CortexAudio,
                                        EventKind::AudioTranscript,
                                    )
                                    .with_data("text", t.text.clone())
                                    .with_data("source", audio_source_str(&t.source))
                                    .with_data("start_ms", t.start_ms)
                                    .with_data("end_ms", t.end_ms);
                                    if let Some(ref speaker) = t.speaker {
                                        ev = ev.with_data("speaker", speaker.clone());
                                    }
                                    if let Some(confidence) = t.confidence {
                                        ev = ev.with_data("confidence", confidence as f64);
                                    }
                                    bridge.forward(ev);
                                }
                            }
                        }
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
                                        // Force immediate context read on the activation tick —
                                        // without this, should_read() returns false for the first
                                        // refresh_ms / tick_ms ticks (e.g. 300 ms / 200 ms = 2
                                        // ticks skipped before DOM elements land in the model).
                                        adapter.ticks_since_last_read = u64::MAX;
                                        debug!(cortex_id = %cortex_id, adapter = %adapter.driver.manifest().name, "Adapter activated and bootstrapped");
                                    }
                                } else {
                                    adapter.state = crate::adapter::AdapterState::Active;
                                    // Same as above — force immediate read.
                                    adapter.ticks_since_last_read = u64::MAX;
                                    debug!(cortex_id = %cortex_id, adapter = %adapter.driver.manifest().name, "Adapter activated");
                                }
                            }
                        }
                        (crate::adapter::AdapterState::Active, false) => {
                            // Deactivate
                            let _ = adapter.driver.deactivate().await;
                            adapter.state = crate::adapter::AdapterState::Inactive;
                            // Drop the cached snapshot so a stale element set
                            // can't be replayed if the adapter reactivates later.
                            adapter.last_elements.clear();
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
                                // Adapter manifests can declare a preferred truth surface
                                // (e.g. "browser_dom" for the Rust browser adapter,
                                // "document_model" for Numbers). When that's "browser_dom"
                                // we tag elements as `Cdp` so SourceSummary / dashboards /
                                // anomaly detection can tell DOM-backed perception apart
                                // from generic native-API perception. Other surfaces fold
                                // into `NativeApi` for back-compat with adapters that
                                // pre-date the distinction (Numbers/Excel/SAP/Bloomberg).
                                let source = if adapter.driver.manifest().context.truth_surface
                                    == "browser_dom"
                                {
                                    cel_context::ContextSource::Cdp
                                } else {
                                    cel_context::ContextSource::NativeApi
                                };
                                let mut tagged = Vec::with_capacity(elements.len());
                                for mut el in elements {
                                    el.source = source.clone();
                                    el.confidence = confidence;
                                    tagged.push(el);
                                }
                                for el in &tagged {
                                    element_adapter_index
                                        .insert(el.id.clone(), adapter_name.clone());
                                    new_context.elements.push(el.clone());
                                }
                                // Cache the tagged snapshot so skip ticks (where
                                // refresh_ms > tick_ms means should_read is false)
                                // can replay it into new_context.elements instead
                                // of dropping adapter elements out of the model.
                                adapter.last_elements = tagged;
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
                            let adapter_name = adapter.driver.manifest().name.clone();
                            // Replay the cached snapshot on this skip tick.
                            // merger.get_context() rebuilds new_context from
                            // scratch every tick and doesn't know about adapters,
                            // so without this replay adapter elements vanish from
                            // current_context on every tick that doesn't hit
                            // should_read. That hits every adapter with
                            // refresh_ms > tick_ms (browser-rs, Numbers, Excel,
                            // SAP, Bloomberg all declare 200ms+).
                            for el in &adapter.last_elements {
                                element_adapter_index.insert(el.id.clone(), adapter_name.clone());
                                new_context.elements.push(el.clone());
                            }
                            active_adapter_names.push(adapter_name);
                        }
                    }
                }
                drop(adapters_guard); // Release lock before remaining tick work

                // OCR last-resort fallback (opt-in CEL_OCR_FALLBACK): now that
                // every structured source (AX, vision, CDP, adapters) has merged
                // into new_context, recognize on-screen text on AX-less surfaces
                // (canvas / WebGL / PDF / image-only) and add only lines no
                // existing element already covers. Self-gating + deduped inside
                // run_ocr_fallback, so this is a no-op unless the model is still
                // sparse and the env flag is set.
                let ocr_extra = merger.run_ocr_fallback(&new_context.elements);
                if !ocr_extra.is_empty() {
                    new_context.elements.extend(ocr_extra);
                }

                // 2. Poll events: watchdog (polling) + AXObserver (push)
                let network_idle = merger.recent_network_events().is_empty();
                let mut events = watchdog.tick(&new_context, network_idle);

                // Merge push-based AXObserver events
                let ax_events = observer.drain_events();
                // Bridge: forward the full AX event stream to the daemon
                // BEFORE consuming ax_events so we can iterate by reference.
                // Mapping is total (see ax_event_to_bridge_event); the bridge's
                // queue is lossy so high-frequency events can't stall the tick.
                if let Some(ref bridge) = daemon_bridge {
                    for ax_ev in &ax_events {
                        bridge.forward(ax_event_to_bridge_event(ax_ev));
                    }
                }
                if !ax_events.is_empty() {
                    events.extend(watchdog.merge_ax_events(ax_events));
                }

                // Bridge: forward newly observed network connections. Gated on a
                // bridge being set (the only consumer) and throttled — lsof is a
                // subprocess. drain_events() returns only connections opened since
                // the last poll; closes aren't tracked (open-only by design).
                if let (Some(bridge), Some(net)) = (&daemon_bridge, &network_monitor) {
                    if now.saturating_sub(last_net_poll_ms) >= NETWORK_POLL_INTERVAL_MS {
                        last_net_poll_ms = now;
                        let conns = net.lock().unwrap_or_else(|p| p.into_inner()).drain_events();
                        for c in conns {
                            let mut ev = Event::now(
                                EventSource::CortexNetwork,
                                EventKind::NetworkConnectionOpened,
                            )
                            .with_data("protocol", c.protocol)
                            .with_data("remote_addr", c.remote_addr)
                            .with_data("remote_port", c.remote_port)
                            .with_data("local_addr", c.local_addr)
                            .with_data("local_port", c.local_port)
                            .with_data("state", c.state);
                            if let Some(service) = c.service {
                                ev = ev.with_data("service", service);
                            }
                            if let Some(process_name) = c.process_name {
                                ev = ev.with_data("process_name", process_name);
                            }
                            if let Some(pid) = c.pid {
                                ev = ev.with_data("pid", pid);
                            }
                            bridge.forward(ev);
                        }
                    }
                }

                // Bridge: forward captured input. Pointer moves are coalesced
                // (POINTER_FWD_INTERVAL_MS); keys, buttons, and scroll forward
                // individually. Per-event shaping lives in input_to_bridge_event.
                if let (Some(bridge), Some(input)) = (&daemon_bridge, &input_capture) {
                    let captured = input
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .drain_events();
                    for ci in &captured {
                        if matches!(ci, cel_input::CapturedInput::MouseMoved { .. }) {
                            if now.saturating_sub(last_pointer_fwd_ms) < POINTER_FWD_INTERVAL_MS {
                                continue;
                            }
                            last_pointer_fwd_ms = now;
                        }
                        bridge.forward(input_to_bridge_event(ci, forward_input_content));
                    }
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

                // Bridge: forward URL changes to the daemon when CDP is active.
                // Only runs when a bridge is wired (the desktop app sets one);
                // the MCP server and eval harness leave `daemon_bridge` unset,
                // so this whole block — including the auto-bind below — is inert
                // for them and never touches their explicit CDP clients.
                if let Some(ref bridge) = daemon_bridge {
                    // Ambient CDP auto-bind. If no client is bound yet, a
                    // browser is frontmost, and we haven't probed in the last
                    // 3s, try to connect. On success the client is stored into
                    // the SHARED `cdp_for_bridge` (== `self.cdp_client`), so the
                    // URL read below and every cdp_eval / navigate path reuse
                    // it. This is what makes `url_changed` flow agnostically —
                    // no host has to call `bind_browser_cdp_url` explicitly.
                    let have_client = cdp_for_bridge
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_some();
                    if !have_client
                        && frontmost_is_browser()
                        && now.saturating_sub(last_cdp_probe_ms) >= CDP_PROBE_INTERVAL_MS
                    {
                        last_cdp_probe_ms = now;
                        if let Some(c) = cel_cdp::connect_to_focused_app().await {
                            // Route through the shared single-writer primitive so the
                            // auto-bound client lands in `cdp_for_bridge`
                            // (== self.cdp_client) AND propagates to the registered
                            // adapters — the app's in-process browser adapter then
                            // perceives the cortex's exact client instead of separately
                            // re-discovering its own.
                            install_cdp_client(&cdp_for_bridge, &adapters, Arc::new(c)).await;
                            last_bridge_url = None; // force a url_changed emit
                            cdp_page_enabled = false; // re-enable Page on new client
                            tracing::info!("cortex: ambient CDP auto-bound for daemon bridge");
                        }
                    }

                    let cdp_opt = cdp_for_bridge
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone();
                    if let Some(client) = cdp_opt {
                        // Best-effort: enable Page-domain events so the browser
                        // emits `Page.loadEventFired`. Done once per client.
                        if !cdp_page_enabled && client.enable_page().await.is_ok() {
                            cdp_page_enabled = true;
                        }
                        match client.get_url().await {
                            Ok(url) => {
                                if Some(&url) != last_bridge_url.as_ref() {
                                    bridge.forward(
                                        Event::now(EventSource::CortexCdp, EventKind::UrlChanged)
                                            .with_data("url", url.clone()),
                                    );
                                    last_bridge_url = Some(url);
                                }
                            }
                            // Client died (browser closed / WebSocket dropped).
                            // Clear it so the auto-bind above re-binds on a later
                            // tick (throttled to every 3s, so no thrash).
                            Err(_) => {
                                *cdp_for_bridge.lock().unwrap_or_else(|p| p.into_inner()) = None;
                                last_bridge_url = None;
                                cdp_page_enabled = false;
                            }
                        }

                        // Forward buffered CDP events (Page.loadEventFired, …).
                        // Non-blocking: drains only what's already queued.
                        for frame in client.drain_cdp_events().await {
                            if let Some(ev) = cdp_event_to_bridge_event(&frame) {
                                bridge.forward(ev);
                            }
                        }
                    }
                }

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

    /// Wait until the CDP page's current URL matches `expected_url`
    /// (modulo query string, fragment, and trailing slash), then force a
    /// fresh tick so perception's element cache reflects the new page
    /// rather than whatever the previous URL had loaded.
    ///
    /// This closes a perception-staleness gap that the eval surfaced
    /// between scenarios: the cortex's previous tick captured the
    /// previous fixture's elements (e.g. a sign-in page from
    /// `dynamic-spa.html`), the CDP page then navigated to the next
    /// fixture (`stale-state.html`), but the cached `MentalModel.
    /// current_context.elements` still contained the stale "sign in"
    /// button because the next tick hadn't run yet. The agent reads
    /// stale perception, decides it needs to log in, and burns its
    /// budget on a goal that doesn't apply to this scenario.
    ///
    /// Two phases:
    /// 1. Poll `cdp_current_url()` until it normalises equal to
    ///    `expected_url` (or the timeout fires).
    /// 2. Call `refresh_now()` so the cortex tick captures the new
    ///    page's elements before the caller reads perception.
    ///
    /// Callers in eval / canonical_runner contexts should pass the
    /// fixture URL the test setup just navigated to. URL comparison
    /// strips query and fragment so `?refresh=1` and `#section` don't
    /// cause spurious mismatches.
    pub async fn wait_for_url(
        &self,
        expected_url: &str,
        timeout_ms: u64,
    ) -> Result<(), CortexError> {
        if !self.running.load(Ordering::Relaxed) {
            return Err(CortexError::NotRunning(self.id.clone()));
        }
        let target = normalise_url(expected_url);
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let poll = std::time::Duration::from_millis(50);

        loop {
            let current = self.cdp_current_url().await;
            if let Some(ref url) = current {
                if normalise_url(url) == target {
                    break;
                }
            }
            if start.elapsed() >= timeout {
                return Err(CortexError::WaitForUrlTimeout {
                    expected: expected_url.to_string(),
                    observed: current,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
            }
            tokio::time::sleep(poll).await;
        }

        // Phase 2: tick AFTER the URL match so perception's element
        // cache reflects the new page. Without this the next read of
        // `model.current_context.elements` may still hold whatever the
        // previous URL had — exactly the regression this method exists
        // to close.
        let remaining = timeout
            .saturating_sub(start.elapsed())
            .max(std::time::Duration::from_millis(500));
        let _ = self.refresh_now(Some(remaining.as_millis() as u64)).await; // Best-effort: a stalled tick shouldn't fail an
                                                                            // otherwise-successful URL wait.
        Ok(())
    }
}
