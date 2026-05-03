//! Integration tests for the Cortex perception engine.
//!
//! Uses `Cortex::isolated()` (StubAccessibility — no OS permissions needed)
//! and cel_audio::StubAudioCapture to verify the engine lifecycle without hardware.
#![allow(dead_code)]

use cel_accessibility::StubAccessibility;
use cel_audio::{
    AudioCapture, AudioChunk, AudioConfig, AudioError, AudioSource, StubAudioCapture,
    TranscriptChunk,
};
use cel_cortex::Cortex;

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
