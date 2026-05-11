//! Integration tests for the Cortex perception engine.
//!
//! Uses `Cortex::isolated()` (StubAccessibility — no OS permissions needed)
//! and cel_audio::StubAudioCapture to verify the engine lifecycle without hardware.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use cel_accessibility::StubAccessibility;
use cel_audio::{
    AudioCapture, AudioChunk, AudioConfig, AudioError, AudioSource, StubAudioCapture,
    TranscriptChunk,
};
use cel_context::{ContextElement, ContextSource};
use cel_cortex::adapter::{LifecycleDeclaration, VerificationDeclaration};
use cel_cortex::{
    ActionResult, AdapterDriver, AdapterError, AdapterManifest, ContextDeclaration, Cortex,
};

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
