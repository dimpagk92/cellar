//! CEL Audio — microphone and system-output capture as a live context stream.
//!
//! # Framing
//!
//! Audio is treated as a *context signal*, not a transcript archive. The
//! capture layer exposes a short rolling buffer (default ~60s) of raw
//! samples plus a parallel stream of transcript chunks. Downstream fusion
//! can consume either shape; nothing here persists by default.
//!
//! # Backends
//!
//! - **Microphone**: cpal → CoreAudio (macOS), ALSA/PipeWire (Linux),
//!   WASAPI (Windows). Works on all platforms with a microphone.
//! - **System output** (what the user hears):
//!   - Linux: auto-detected via PulseAudio/PipeWire `*.monitor` devices.
//!   - macOS: requires a virtual loopback device (Blackhole recommended).
//!     Returns `AudioError::Unavailable` if none found.
//!   - Windows: WASAPI loopback, auto-detected.
//!
//! # Transcription
//!
//! `drain_transcripts()` currently returns empty — wire a Whisper endpoint
//! at the `cel-cortex` level to fill it.

use serde::{Deserialize, Serialize};

mod capture;
pub mod transcribe;

pub use capture::CpalCapture;
pub use transcribe::{Transcriber, WhisperApiConfig, WhisperApiTranscriber};

/// Audio input source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioSource {
    /// Microphone / default input device.
    Microphone,
    /// Loopback / system output (what the user is hearing).
    SystemOutput,
    /// Both channels captured and tagged separately.
    Both,
}

/// Capture + transcription configuration.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub source: AudioSource,
    /// Target sample rate for downstream processing. 16 000 Hz is the
    /// Whisper canonical input; capture may resample internally.
    pub sample_rate: u32,
    pub channels: u16,
    /// Rolling buffer window in seconds. Older samples are dropped.
    pub ring_buffer_secs: u32,
    /// If true, run local transcription and emit [`TranscriptChunk`]s.
    pub transcribe: bool,
    /// If true, redact samples during password-field focus (requires
    /// a11y-layer focus signal).
    pub redact_on_password_focus: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            source: AudioSource::Both,
            sample_rate: 16_000,
            channels: 1,
            ring_buffer_secs: 60,
            transcribe: true,
            redact_on_password_focus: true,
        }
    }
}

/// A raw audio chunk — interleaved `f32` samples.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub source: AudioSource,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    /// Unix-ms timestamp of the chunk's first sample.
    pub timestamp_ms: u64,
}

/// A transcribed segment of audio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptChunk {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Speaker label if diarization is available; `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Model confidence in the transcript (0.0–1.0). `None` when the
    /// model doesn't expose one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub source: AudioSource,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("Audio capture not available on this platform")]
    Unavailable,
    #[error("Permission denied — microphone or screen-recording access not granted")]
    PermissionDenied,
    #[error("Audio device error: {0}")]
    DeviceError(String),
    #[error("Transcription failed: {0}")]
    TranscriptionFailed(String),
    #[error("Already capturing")]
    AlreadyCapturing,
    #[error("Not capturing")]
    NotCapturing,
}

/// Platform-agnostic audio capture provider.
///
/// Implementations must be safe to call from a non-blocking polling loop:
/// `drain_*` never waits and returns whatever is buffered.
pub trait AudioCapture: Send + Sync {
    /// Begin capture with the given configuration.
    fn start(&mut self, config: AudioConfig) -> Result<(), AudioError>;

    /// Stop capture.
    fn stop(&mut self) -> Result<(), AudioError>;

    /// Whether a capture session is currently active.
    fn is_capturing(&self) -> bool;

    /// Pull available transcript chunks since the last call.
    /// Returns `Vec::new()` if transcription is disabled or no new text.
    fn drain_transcripts(&mut self) -> Vec<TranscriptChunk>;

    /// Pull raw audio chunks since the last call. Rarely used by the
    /// runtime — exposed for advanced consumers (e.g. VAD inspectors).
    fn drain_raw_chunks(&mut self) -> Vec<AudioChunk> {
        Vec::new()
    }
}

/// No-op fallback used when cpal fails to initialize.
pub struct StubAudioCapture {
    started: bool,
}

impl StubAudioCapture {
    pub fn new() -> Self {
        Self { started: false }
    }
}

impl Default for StubAudioCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioCapture for StubAudioCapture {
    fn start(&mut self, _config: AudioConfig) -> Result<(), AudioError> {
        if self.started {
            return Err(AudioError::AlreadyCapturing);
        }
        tracing::warn!("cel-audio: using stub capture (no audio device available)");
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        if !self.started {
            return Err(AudioError::NotCapturing);
        }
        self.started = false;
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.started
    }

    fn drain_transcripts(&mut self) -> Vec<TranscriptChunk> {
        Vec::new()
    }
}

/// Create a platform-appropriate audio capture provider.
///
/// Returns a [`capture::CpalCapture`] backed by the platform's default
/// audio host. Falls back to [`StubAudioCapture`] only if cpal cannot
/// initialize (e.g., no audio hardware at all).
pub fn create_audio_capture() -> Box<dyn AudioCapture> {
    Box::new(capture::CpalCapture::new())
}

/// List available input devices and whether each looks like a loopback
/// (system-audio) source. Useful for UI device pickers.
pub fn list_input_devices() -> Vec<(String, bool)> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    host.input_devices()
        .map(|devs| {
            devs.filter_map(|d| {
                let name = d.name().ok()?;
                let is_loopback = name.contains(".monitor")
                    || name.to_lowercase().contains("blackhole")
                    || name.to_lowercase().contains("soundflower")
                    || name.to_lowercase().contains("loopback");
                Some((name, is_loopback))
            })
            .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_targets_whisper_16khz_mono() {
        let cfg = AudioConfig::default();
        assert_eq!(cfg.sample_rate, 16_000);
        assert_eq!(cfg.channels, 1);
        assert_eq!(cfg.ring_buffer_secs, 60);
        assert!(cfg.transcribe);
        assert!(cfg.redact_on_password_focus);
    }

    #[test]
    fn stub_lifecycle() {
        let mut a = StubAudioCapture::new();
        assert!(!a.is_capturing());
        a.start(AudioConfig::default()).unwrap();
        assert!(a.is_capturing());
        assert!(a.drain_transcripts().is_empty());
        a.stop().unwrap();
        assert!(!a.is_capturing());
    }

    #[test]
    fn stub_rejects_double_start() {
        let mut a = StubAudioCapture::new();
        a.start(AudioConfig::default()).unwrap();
        assert!(matches!(
            a.start(AudioConfig::default()),
            Err(AudioError::AlreadyCapturing)
        ));
    }

    #[test]
    fn stub_rejects_stop_when_idle() {
        let mut a = StubAudioCapture::new();
        assert!(matches!(a.stop(), Err(AudioError::NotCapturing)));
    }

    #[test]
    fn create_returns_working_instance() {
        let mut a = create_audio_capture();
        a.start(AudioConfig::default()).unwrap();
        assert!(a.is_capturing());
        a.stop().unwrap();
    }

    /// Requires real audio hardware. Run with:
    ///   cargo test -p cel-audio -- mic_delivers_samples --ignored --nocapture
    #[test]
    #[ignore]
    fn mic_delivers_samples() {
        let mut a = create_audio_capture();
        a.start(AudioConfig {
            source: AudioSource::Microphone,
            ..AudioConfig::default()
        })
        .expect("microphone unavailable — no audio device");

        // Give the callback thread time to fill the first chunk
        std::thread::sleep(std::time::Duration::from_millis(300));

        let chunks = a.drain_raw_chunks();
        a.stop().unwrap();

        assert!(!chunks.is_empty(), "no audio chunks received after 300ms");
        let total_samples: usize = chunks.iter().map(|c| c.samples.len()).sum();
        assert!(
            total_samples > 0,
            "chunks received but all empty (total_samples=0)"
        );
        println!(
            "mic_delivers_samples: {} chunks, {} samples @ {} Hz",
            chunks.len(),
            total_samples,
            chunks[0].sample_rate
        );
    }

    /// Same as above but for system output (loopback device required).
    #[test]
    #[ignore]
    fn system_output_delivers_samples() {
        let devices = list_input_devices();
        let has_loopback = devices.iter().any(|(_, is_loop)| *is_loop);
        if !has_loopback {
            println!("system_output_delivers_samples: skipped — no loopback device found");
            println!("  Devices: {:?}", devices);
            return;
        }

        let mut a = create_audio_capture();
        a.start(AudioConfig {
            source: AudioSource::SystemOutput,
            ..AudioConfig::default()
        })
        .expect("system output unavailable");

        std::thread::sleep(std::time::Duration::from_millis(300));

        let chunks = a.drain_raw_chunks();
        a.stop().unwrap();

        assert!(
            !chunks.is_empty(),
            "no system audio chunks received after 300ms"
        );
        let total_samples: usize = chunks.iter().map(|c| c.samples.len()).sum();
        println!(
            "system_output_delivers_samples: {} chunks, {} samples @ {} Hz",
            chunks.len(),
            total_samples,
            chunks[0].sample_rate
        );
    }

    #[test]
    fn transcript_chunk_serializes() {
        let c = TranscriptChunk {
            text: "hello".into(),
            start_ms: 0,
            end_ms: 500,
            speaker: Some("S1".into()),
            confidence: Some(0.93),
            source: AudioSource::Microphone,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("hello"));
        assert!(json.contains("microphone"));
    }
}
