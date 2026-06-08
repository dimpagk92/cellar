//! Integration tests for the Cortex perception engine.
//!
//! Uses `Cortex::isolated()` (StubAccessibility — no OS permissions needed)
//! and cel_audio::StubAudioCapture to verify the engine lifecycle without hardware.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cel_accessibility::{
    AccessibilityElement, AccessibilityError, AccessibilityEvent, AccessibilityTree, ElementRole,
    StubAccessibility,
};
use cel_audio::{
    AudioCapture, AudioChunk, AudioConfig, AudioError, AudioSource, StubAudioCapture,
    TranscriptChunk,
};
use cel_context::{ContextElement, ContextSource};
use cel_cortex::adapter::{LifecycleDeclaration, VerificationDeclaration};
use cel_cortex::daemon_bridge::DaemonBridge;
use cel_cortex::{
    ActionResult, AdapterDriver, AdapterError, AdapterManifest, ContextDeclaration, Cortex,
};
use cellar_types::event::{Event, EventKind, EventSource};

// ─── MockAudioCapture ────────────────────────────────────────────────────────

/// Audio capture mock that records whether start/stop were called and allows
/// injecting transcript chunks to verify cortex drains them into ScreenContext.
struct MockAudioCapture {
    started: bool,
    pending_transcripts: Vec<TranscriptChunk>,
}

impl MockAudioCapture {
    fn new() -> Self {
        Self {
            started: false,
            pending_transcripts: Vec::new(),
        }
    }

    fn push_transcript(&mut self, text: &str, start_ms: u64, end_ms: u64) {
        self.pending_transcripts.push(TranscriptChunk {
            text: text.into(),
            start_ms,
            end_ms,
            speaker: None,
            confidence: Some(0.9),
            source: AudioSource::Microphone,
        });
    }
}

impl AudioCapture for MockAudioCapture {
    fn start(&mut self, _config: AudioConfig) -> Result<(), AudioError> {
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.started = false;
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.started
    }

    fn drain_transcripts(&mut self) -> Vec<TranscriptChunk> {
        std::mem::take(&mut self.pending_transcripts)
    }

    fn drain_raw_chunks(&mut self) -> Vec<AudioChunk> {
        Vec::new()
    }
}

// ─── Lifecycle tests ─────────────────────────────────────────────────────────

#[test]
fn test_cortex_new_not_running() {
    let cortex = Cortex::new("test-new".into());
    assert_eq!(cortex.id, "test-new");
    assert!(!cortex.is_running());
}

#[test]
fn test_cortex_isolated_returns_pair() {
    let (cortex, _merger) = Cortex::isolated("iso-test");
    assert_eq!(cortex.id, "iso-test");
    assert!(!cortex.is_running());
}

#[test]
fn test_cortex_with_tick_ms_builder() {
    let (cortex, _) = Cortex::isolated("tick-test");
    let cortex = cortex.with_tick_ms(50);
    assert_eq!(cortex.id, "tick-test");
    assert!(!cortex.is_running());
}

#[tokio::test]
async fn test_cortex_boot_sets_running() {
    let (mut cortex, merger) = Cortex::isolated("boot-test");
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();
    assert!(cortex.is_running());
    cortex.shutdown();
}

#[tokio::test]
async fn test_cortex_shutdown_stops_running() {
    let (mut cortex, merger) = Cortex::isolated("shutdown-test");
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();
    assert!(cortex.is_running());
    cortex.shutdown();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    assert!(!cortex.is_running());
}

#[tokio::test]
async fn test_cortex_boot_twice_fails() {
    let (mut cortex, merger) = Cortex::isolated("double-boot");
    let (_, merger2) = Cortex::isolated("dummy");
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();
    let result = cortex.boot(merger2, Box::new(StubAccessibility)).await;
    assert!(result.is_err(), "second boot should return AlreadyRunning");
    cortex.shutdown();
}

#[tokio::test]
async fn test_cortex_mental_model_populated_after_boot() {
    let (mut cortex, merger) = Cortex::isolated("model-test");
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    let model_arc = cortex.model();
    let snapshot = model_arc.read().await.current_context.clone();
    // StubAccessibility returns an empty element tree — context must be initialized
    let _ = snapshot.elements.len(); // must not panic

    cortex.shutdown();
}

// ─── Audio integration tests ─────────────────────────────────────────────────

#[tokio::test]
async fn test_cortex_boots_with_stub_audio() {
    let capture = Box::new(StubAudioCapture::new());
    let config = AudioConfig {
        transcribe: false,
        ..Default::default()
    };

    let (cortex, merger) = Cortex::isolated("stub-audio");
    let mut cortex = cortex.with_audio(capture, config);
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();
    assert!(cortex.is_running());
    cortex.shutdown();
}

#[tokio::test]
async fn test_cortex_model_has_transcript_field_after_boot() {
    // Boot with stub audio; transcripts field should be empty but present
    let capture = Box::new(StubAudioCapture::new());
    let config = AudioConfig {
        transcribe: false,
        ..Default::default()
    };

    let (cortex, merger) = Cortex::isolated("transcript-field");
    let mut cortex = cortex.with_audio(capture, config);
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    let model_arc = cortex.model();
    let snapshot = model_arc.read().await.current_context.clone();
    // transcripts field must exist (Vec, may be empty)
    let _ = snapshot.transcripts.len();

    cortex.shutdown();
}

// ─── Adapter pipeline integration ────────────────────────────────────────────

/// Counts probe / activate / get_context / deactivate calls so the test
/// can assert the cortex tick loop walked the full lifecycle, not just
/// happened to find pre-seeded elements.
#[derive(Default)]
struct AdapterCallLog {
    probes: AtomicU32,
    activates: AtomicU32,
    get_contexts: AtomicU32,
    deactivates: AtomicU32,
}

/// Stub adapter for the integration test. Returns a fixed `dom:*`
/// element on every `get_context()` so we can prove the cortex tick
/// loop pumps adapter elements through to `model.current_context`.
/// Mirrors the contract `BrowserAdapter` (and any future native
/// adapter) implements — without exercising any real I/O.
struct StubBrowserAdapter {
    manifest: AdapterManifest,
    log: Arc<AdapterCallLog>,
    /// What `probe()` reports. Toggle via `set_probe(false)` to verify
    /// the cortex deactivates an adapter that has gone unhealthy.
    probe_result: std::sync::atomic::AtomicBool,
}

impl StubBrowserAdapter {
    fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                name: "stub-browser".into(),
                display_name: "Stub Browser".into(),
                // No app_patterns to match — the lifecycle gate (background_refresh
                // + probe) is the activation path, mirroring how BrowserAdapter
                // operates against a headless target with no AX surface.
                app_patterns: Vec::new(),
                platform: vec!["macos".into(), "linux".into(), "windows".into()],
                runtime: "native".into(),
                entrypoint: None,
                manifest_alias: None,
                manifest_extends: None,
                context: ContextDeclaration {
                    element_types: vec!["button".into()],
                    refresh_ms: 50,
                    confidence: 0.9,
                    truth_surface: "browser_dom".into(),
                },
                lifecycle: LifecycleDeclaration {
                    requires_frontmost: false,
                    background_refresh: true,
                    bootstrap_on_activate: false,
                    response_timeout_ms: None,
                },
                verification: VerificationDeclaration::default(),
                actions: HashMap::new(),
            },
            log: Arc::new(AdapterCallLog::default()),
            probe_result: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn log(&self) -> Arc<AdapterCallLog> {
        Arc::clone(&self.log)
    }
}

#[async_trait]
impl AdapterDriver for StubBrowserAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    async fn activate(&mut self) -> Result<(), AdapterError> {
        self.log.activates.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<(), AdapterError> {
        self.log.deactivates.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError> {
        self.log.get_contexts.fetch_add(1, Ordering::SeqCst);
        Ok(vec![ContextElement {
            id: "dom:button:stub-submit".into(),
            label: Some("Submit".into()),
            description: None,
            element_type: "button".into(),
            value: None,
            bounds: None,
            state: cel_accessibility::ElementState::default(),
            parent_id: None,
            actions: vec!["press".into(), "click".into()],
            confidence: 0.88,
            // Pre-tag — the cortex tick loop overrides this to NativeApi
            // unconditionally (cortex.rs ~L677). The test assertion below
            // verifies that override is observable, which is the contract
            // adapter authors rely on for source attribution.
            source: ContextSource::AccessibilityTree,
            content_role: cel_context::ContentRole::Interactive,
            properties: HashMap::new(),
        }])
    }

    async fn execute(
        &self,
        _action: &str,
        _params: serde_json::Value,
    ) -> Result<ActionResult, AdapterError> {
        Err(AdapterError::ExecutionFailed("stub: no actions".into()))
    }

    async fn probe(&self) -> bool {
        self.log.probes.fetch_add(1, Ordering::SeqCst);
        self.probe_result.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn registered_adapter_elements_flow_into_screen_context() {
    // The contract claim of the AdapterDriver framework: register an
    // adapter that returns `dom:*` elements, boot the cortex, wait for
    // a tick, and those elements appear in `model.current_context`.
    // BrowserAdapter and every native adapter (Numbers, Excel, …)
    // depend on this contract — pinning it here means a future
    // refactor of the cortex tick loop that drops adapter aggregation
    // breaks this test loudly instead of silently breaking every
    // adapter at runtime.
    let (cortex, merger) = Cortex::isolated("adapter-pipeline");
    let mut cortex = cortex.with_tick_ms(50);
    let stub = StubBrowserAdapter::new();
    let log = stub.log();
    cortex.register_adapter(Box::new(stub));
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    // Let the tick loop fire at least twice — once for probe→activate,
    // once for the first get_context after activation. 200ms is 4x the
    // 50ms tick interval, so flake-tolerant under macOS scheduler jitter.
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    let snapshot = cortex.model().read().await.current_context.clone();
    let dom_elements: Vec<&ContextElement> = snapshot
        .elements
        .iter()
        .filter(|e| e.id.starts_with("dom:"))
        .collect();

    assert!(
        !dom_elements.is_empty(),
        "stub adapter dom:* elements should be in current_context (saw {} elements total: {:?})",
        snapshot.elements.len(),
        snapshot.elements.iter().map(|e| &e.id).collect::<Vec<_>>()
    );

    let stub_button = dom_elements
        .iter()
        .find(|e| e.id == "dom:button:stub-submit")
        .expect("the stub-submit button should be present");
    // The cortex tick loop tags adapter elements based on the manifest's
    // `truth_surface`. Our stub declares "browser_dom" so its elements
    // should land as `Cdp`, distinguishable from the `NativeApi` tier
    // used by Numbers/Excel/SAP. Downstream telemetry (SourceSummary)
    // and dashboards rely on this distinction to attribute perception
    // sources correctly.
    assert_eq!(stub_button.source, ContextSource::Cdp);

    // The cortex tick loop must call probe → activate → get_context in
    // order. Without these calls the contract is broken even if elements
    // happen to appear (e.g. via some other bug-paved code path).
    assert!(
        log.probes.load(Ordering::SeqCst) >= 1,
        "probe should be called at least once"
    );
    assert!(
        log.activates.load(Ordering::SeqCst) >= 1,
        "activate should be called at least once"
    );
    assert!(
        log.get_contexts.load(Ordering::SeqCst) >= 1,
        "get_context should be called at least once after activation"
    );

    cortex.shutdown();
}

// ─── DaemonBridge forwarding tests ────────────────────────────────────────────

/// AccessibilityTree mock that replays a fixed queue of AX events on the first
/// `drain_events()` call (subsequent calls return empty, mirroring real
/// observer drain semantics). Tree reads delegate to `StubAccessibility`.
struct QueuedObserver {
    pending: Vec<AccessibilityEvent>,
}

impl QueuedObserver {
    fn new(pending: Vec<AccessibilityEvent>) -> Self {
        Self { pending }
    }
}

impl AccessibilityTree for QueuedObserver {
    fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError> {
        StubAccessibility.get_tree()
    }

    fn find_elements(
        &self,
        role: Option<&ElementRole>,
        label: Option<&str>,
    ) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
        StubAccessibility.find_elements(role, label)
    }

    fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError> {
        StubAccessibility.focused_element()
    }

    fn drain_events(&mut self) -> Vec<AccessibilityEvent> {
        std::mem::take(&mut self.pending)
    }
}

/// DaemonBridge mock that records every forwarded event into a shared buffer.
#[derive(Clone, Default)]
struct RecordingBridge {
    events: Arc<Mutex<Vec<Event>>>,
}

impl DaemonBridge for RecordingBridge {
    fn forward(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }
}

#[tokio::test]
async fn test_daemon_bridge_forwards_app_focus_and_window_open() {
    let bridge = RecordingBridge::default();
    let recorded = bridge.events.clone();

    let observer = QueuedObserver::new(vec![
        AccessibilityEvent::AppActivated {
            app_name: Some("Safari".into()),
        },
        AccessibilityEvent::WindowCreated {
            title: Some("Login".into()),
        },
        // Now maps to EventKind::MenuOpened (forwarding is total).
        AccessibilityEvent::MenuOpened,
    ]);

    let (cortex, merger) = Cortex::isolated("bridge-ax");
    let mut cortex = cortex.with_tick_ms(20).with_daemon_bridge(Arc::new(bridge));
    cortex.boot(merger, Box::new(observer)).await.unwrap();

    // Let a few ticks run so the queued events are drained + forwarded.
    tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
    cortex.shutdown();

    let events = recorded.lock().unwrap();
    let app_focused = events
        .iter()
        .find(|e| e.kind == EventKind::AppFocused)
        .expect("AppFocused event should be forwarded");
    assert_eq!(app_focused.source, EventSource::CortexAx);
    assert_eq!(
        app_focused.data.get("app").and_then(|v| v.as_str()),
        Some("Safari")
    );

    let window_opened = events
        .iter()
        .find(|e| e.kind == EventKind::WindowOpened)
        .expect("WindowOpened event should be forwarded");
    assert_eq!(window_opened.source, EventSource::CortexAx);
    assert_eq!(
        window_opened.data.get("title").and_then(|v| v.as_str()),
        Some("Login")
    );

    // Forwarding is now total: MenuOpened maps to EventKind::MenuOpened.
    let menu_opened = events
        .iter()
        .find(|e| e.kind == EventKind::MenuOpened)
        .expect("MenuOpened event should be forwarded");
    assert_eq!(menu_opened.source, EventSource::CortexAx);

    // Every forwarded AX event must carry the CortexAx source.
    assert!(
        events.iter().all(|e| e.source == EventSource::CortexAx),
        "all AX-derived events use the CortexAx source"
    );
}

#[tokio::test]
async fn test_no_daemon_bridge_means_no_forwarding_panic() {
    // Without a bridge the tick loop must run cleanly even when AX events fire.
    let observer = QueuedObserver::new(vec![AccessibilityEvent::AppActivated {
        app_name: Some("Finder".into()),
    }]);
    let (cortex, merger) = Cortex::isolated("no-bridge");
    let mut cortex = cortex.with_tick_ms(20);
    cortex.boot(merger, Box::new(observer)).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(60)).await;
    assert!(cortex.is_running());
    cortex.shutdown();
}

// ─── Audio + Network stream forwarding tests ──────────────────────────────────

/// NetworkMonitor mock that replays a fixed queue of connections on the first
/// `drain_events()` call (subsequent calls return empty).
struct MockNetworkMonitor {
    pending: Vec<cel_network::ConnectionEvent>,
    started: bool,
}

impl MockNetworkMonitor {
    fn new(pending: Vec<cel_network::ConnectionEvent>) -> Self {
        Self {
            pending,
            started: false,
        }
    }
}

impl cel_network::NetworkMonitor for MockNetworkMonitor {
    fn start(&mut self) -> Result<(), cel_network::NetworkError> {
        self.started = true;
        Ok(())
    }
    fn stop(&mut self) -> Result<(), cel_network::NetworkError> {
        self.started = false;
        Ok(())
    }
    fn drain_events(&mut self) -> Vec<cel_network::ConnectionEvent> {
        std::mem::take(&mut self.pending)
    }
    fn is_running(&self) -> bool {
        self.started
    }
}

fn sample_connection() -> cel_network::ConnectionEvent {
    cel_network::ConnectionEvent {
        timestamp_ms: 1_000,
        protocol: "tcp".into(),
        local_addr: "192.168.1.100".into(),
        local_port: 54321,
        remote_addr: "93.184.216.34".into(),
        remote_port: 443,
        state: "ESTABLISHED".into(),
        service: Some("https".into()),
        process_name: Some("Safari".into()),
        pid: Some(42),
    }
}

#[tokio::test]
async fn test_daemon_bridge_forwards_audio_activity_and_transcript() {
    let bridge = RecordingBridge::default();
    let recorded = bridge.events.clone();

    let mut capture = MockAudioCapture::new();
    capture.push_transcript("hello world", 0, 1_000);

    let (cortex, merger) = Cortex::isolated("bridge-audio");
    let mut cortex = cortex
        .with_tick_ms(20)
        .with_audio(Box::new(capture), AudioConfig::default())
        .with_audio_transcript_forwarding(true)
        .with_daemon_bridge(Arc::new(bridge));
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
    cortex.shutdown();

    let events = recorded.lock().unwrap();

    let started = events
        .iter()
        .find(|e| e.kind == EventKind::AudioCaptureStarted)
        .expect("AudioCaptureStarted should be forwarded at boot");
    assert_eq!(started.source, EventSource::CortexAudio);
    assert_eq!(
        started.data.get("source").and_then(|v| v.as_str()),
        Some("both")
    );

    let transcript = events
        .iter()
        .find(|e| e.kind == EventKind::AudioTranscript)
        .expect("AudioTranscript should be forwarded when enabled");
    assert_eq!(transcript.source, EventSource::CortexAudio);
    assert_eq!(
        transcript.data.get("text").and_then(|v| v.as_str()),
        Some("hello world")
    );

    let stopped = events
        .iter()
        .find(|e| e.kind == EventKind::AudioCaptureStopped)
        .expect("AudioCaptureStopped should be forwarded at shutdown");
    assert_eq!(stopped.source, EventSource::CortexAudio);
}

#[tokio::test]
async fn test_audio_transcript_forwarding_gated_off_by_default() {
    let bridge = RecordingBridge::default();
    let recorded = bridge.events.clone();

    let mut capture = MockAudioCapture::new();
    capture.push_transcript("secret words", 0, 1_000);

    let (cortex, merger) = Cortex::isolated("audio-gated");
    let mut cortex = cortex
        .with_tick_ms(20)
        .with_audio(Box::new(capture), AudioConfig::default())
        // NOTE: transcript forwarding intentionally NOT enabled.
        .with_daemon_bridge(Arc::new(bridge));
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
    cortex.shutdown();

    let events = recorded.lock().unwrap();
    // Activity event is always forwarded...
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::AudioCaptureStarted));
    // ...but transcript CONTENT must NOT be forwarded when the gate is off.
    assert!(
        !events.iter().any(|e| e.kind == EventKind::AudioTranscript),
        "transcript content must stay local unless explicitly enabled"
    );
}

#[tokio::test]
async fn test_daemon_bridge_forwards_network_connection_opened() {
    let bridge = RecordingBridge::default();
    let recorded = bridge.events.clone();

    let monitor = MockNetworkMonitor::new(vec![sample_connection()]);

    let (cortex, merger) = Cortex::isolated("bridge-network");
    let mut cortex = cortex
        .with_tick_ms(20)
        .with_network(Box::new(monitor))
        .with_daemon_bridge(Arc::new(bridge));
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
    cortex.shutdown();

    let events = recorded.lock().unwrap();
    let conn = events
        .iter()
        .find(|e| e.kind == EventKind::NetworkConnectionOpened)
        .expect("NetworkConnectionOpened should be forwarded");
    assert_eq!(conn.source, EventSource::CortexNetwork);
    assert_eq!(
        conn.data.get("remote_addr").and_then(|v| v.as_str()),
        Some("93.184.216.34")
    );
    assert_eq!(
        conn.data.get("remote_port").and_then(|v| v.as_u64()),
        Some(443)
    );
    assert_eq!(
        conn.data.get("service").and_then(|v| v.as_str()),
        Some("https")
    );
    assert_eq!(conn.data.get("pid").and_then(|v| v.as_u64()), Some(42));
}

#[tokio::test]
async fn test_daemon_bridge_forwards_input_events() {
    let bridge = RecordingBridge::default();
    let recorded = bridge.events.clone();

    let mut cap = cel_input::StubInputCapture::new();
    cap.push(cel_input::CapturedInput::KeyDown {
        keycode: 4,
        chars: Some("h".into()),
    });
    cap.push(cel_input::CapturedInput::MouseButton {
        button: cel_input::MouseButton::Left,
        pressed: true,
        x: 12.0,
        y: 34.0,
    });
    cap.push(cel_input::CapturedInput::Scroll { dx: 0, dy: -3 });

    let (cortex, merger) = Cortex::isolated("bridge-input");
    let mut cortex = cortex
        .with_tick_ms(20)
        .with_input_capture(Box::new(cap))
        .with_input_content_forwarding(true)
        .with_daemon_bridge(Arc::new(bridge));
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
    cortex.shutdown();

    let events = recorded.lock().unwrap();

    let key = events
        .iter()
        .find(|e| e.kind == EventKind::KeyboardInput)
        .expect("KeyboardInput should be forwarded");
    assert_eq!(key.source, EventSource::CortexInput);
    assert_eq!(
        key.data.get("pressed").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(key.data.get("keycode").and_then(|v| v.as_u64()), Some(4));
    assert_eq!(key.data.get("text").and_then(|v| v.as_str()), Some("h"));

    let btn = events
        .iter()
        .find(|e| e.kind == EventKind::PointerButton)
        .expect("PointerButton should be forwarded");
    assert_eq!(btn.source, EventSource::CortexInput);
    assert_eq!(
        btn.data.get("button").and_then(|v| v.as_str()),
        Some("left")
    );
    assert_eq!(
        btn.data.get("pressed").and_then(|v| v.as_bool()),
        Some(true)
    );

    assert!(
        events.iter().any(|e| e.kind == EventKind::PointerScroll),
        "PointerScroll should be forwarded"
    );
}

#[tokio::test]
async fn test_input_content_gated_off_by_default() {
    let bridge = RecordingBridge::default();
    let recorded = bridge.events.clone();

    let mut cap = cel_input::StubInputCapture::new();
    cap.push(cel_input::CapturedInput::KeyDown {
        keycode: 4,
        chars: Some("secret".into()),
    });

    let (cortex, merger) = Cortex::isolated("input-gated");
    let mut cortex = cortex
        .with_tick_ms(20)
        .with_input_capture(Box::new(cap))
        // NOTE: content forwarding intentionally NOT enabled.
        .with_daemon_bridge(Arc::new(bridge));
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
    cortex.shutdown();

    let events = recorded.lock().unwrap();
    let key = events
        .iter()
        .find(|e| e.kind == EventKind::KeyboardInput)
        .expect("KeyboardInput (keycode) should still be forwarded");
    // Keycode is present, but typed CONTENT must be withheld by default.
    assert!(key.data.contains_key("keycode"));
    assert!(
        !key.data.contains_key("text"),
        "keystroke text must be gated off unless explicitly enabled"
    );
}
