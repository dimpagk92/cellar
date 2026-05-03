//! Transcription backend trait and OpenAI-compatible implementation.
//!
//! # Usage
//!
//! ```no_run
//! use cel_audio::{CpalCapture, AudioCapture, AudioConfig};
//! use cel_audio::transcribe::{WhisperApiConfig, WhisperApiTranscriber};
//! use std::sync::Arc;
//!
//! let transcriber = Arc::new(WhisperApiTranscriber::new(WhisperApiConfig {
//!     api_key: std::env::var("OPENAI_API_KEY").unwrap(),
//!     ..WhisperApiConfig::default()
//! }));
//!
//! let mut capture = CpalCapture::new();
//! capture.set_transcriber(transcriber);
//! capture.start(AudioConfig::default()).unwrap();
//! ```

use crate::{AudioSource, TranscriptChunk};
use std::collections::VecDeque;

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Transcription backend. Implemented by [`WhisperApiTranscriber`] and any
/// custom backends (local whisper-rs, Deepgram, etc.).
pub trait Transcriber: Send + Sync {
    /// Transcribe a pre-encoded WAV file (any sample rate/channels accepted).
    /// Returns `None` if the segment is empty, silent, or the call fails.
    fn transcribe(
        &self,
        wav_bytes: &[u8],
        source: AudioSource,
        start_ms: u64,
        end_ms: u64,
    ) -> Option<TranscriptChunk>;
}

// ─── WhisperApiTranscriber ────────────────────────────────────────────────────

/// Config for any OpenAI-compatible `/v1/audio/transcriptions` endpoint.
/// Works with OpenAI's hosted API or any self-hosted server
/// (faster-whisper, whisper.cpp server, Groq, etc.).
#[derive(Debug, Clone)]
pub struct WhisperApiConfig {
    /// Full URL, e.g. `"https://api.openai.com/v1/audio/transcriptions"`.
    pub endpoint: String,
    /// Bearer token. For local servers this may be `"none"` or empty.
    pub api_key: String,
    /// Model name sent to the endpoint. Ignored by servers that only have one model.
    pub model: String,
    /// ISO 639-1 language hint (optional). Improves accuracy and speed.
    pub language: Option<String>,
    /// HTTP timeout per request.
    pub timeout_secs: u64,
}

impl Default for WhisperApiConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.openai.com/v1/audio/transcriptions".into(),
            api_key: String::new(),
            model: "whisper-1".into(),
            language: None,
            timeout_secs: 30,
        }
    }
}

pub struct WhisperApiTranscriber {
    config: WhisperApiConfig,
    client: reqwest::blocking::Client,
}

impl WhisperApiTranscriber {
    pub fn new(config: WhisperApiConfig) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .expect("failed to build HTTP client");
        Self { config, client }
    }
}

impl Transcriber for WhisperApiTranscriber {
    fn transcribe(
        &self,
        wav_bytes: &[u8],
        source: AudioSource,
        start_ms: u64,
        end_ms: u64,
    ) -> Option<TranscriptChunk> {
        use reqwest::blocking::multipart;

        let part = multipart::Part::bytes(wav_bytes.to_vec())
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .ok()?;

        let mut form = multipart::Form::new()
            .text("model", self.config.model.clone())
            .text("response_format", "json")
            .part("file", part);

        if let Some(ref lang) = self.config.language {
            form = form.text("language", lang.clone());
        }

        let mut req = self.client.post(&self.config.endpoint).multipart(form);

        if !self.config.api_key.is_empty() {
            req = req.bearer_auth(&self.config.api_key);
        }

        let response = match req.send() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Whisper API request failed: {}", e);
                return None;
            }
        };

        if !response.status().is_success() {
            tracing::warn!(
                "Whisper API error {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            );
            return None;
        }

        let json: serde_json::Value = match response.json() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Whisper API response parse failed: {}", e);
                return None;
            }
        };

        let text = json.get("text")?.as_str()?.trim().to_string();
        if text.is_empty() {
            return None;
        }

        tracing::debug!("Whisper transcript [{:?}]: {:?}", source, text);

        Some(TranscriptChunk {
            text,
            start_ms,
            end_ms,
            speaker: None,
            confidence: None,
            source,
        })
    }
}

// ─── Resampling ──────────────────────────────────────────────────────────────

/// Downmix interleaved multi-channel samples to mono by averaging channels.
pub fn downmix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels as usize;
    if ch <= 1 {
        return samples.to_vec();
    }
    samples
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

/// Linear-interpolation resample from src_rate to dst_rate (mono input).
pub fn resample_mono(samples: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src_rate == dst_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = src_rate as f64 / dst_rate as f64;
    let dst_len = ((samples.len() as f64) / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(dst_len);
    for i in 0..dst_len {
        let pos = i as f64 * ratio;
        let idx = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let s0 = samples[idx.min(samples.len() - 1)];
        let s1 = samples[(idx + 1).min(samples.len() - 1)];
        out.push(s0 + (s1 - s0) * frac);
    }
    out
}

/// Prepare samples for Whisper: downmix to mono then resample to 16 kHz.
pub fn prepare_for_whisper(samples: &[f32], src_rate: u32, channels: u16) -> Vec<f32> {
    let mono = downmix_to_mono(samples, channels);
    resample_mono(&mono, src_rate, 16_000)
}

// ─── WAV encoding ────────────────────────────────────────────────────────────

/// Encode f32 samples as a 16-bit PCM WAV file in memory.
/// Accepts any sample rate and channel count.
pub fn encode_wav(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let pcm: Vec<i16> = samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect();

    let data_len = pcm.len() * 2;
    let mut wav = Vec::with_capacity(44 + data_len);

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&((data_len + 36) as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&(channels * 2).to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in &pcm {
        wav.extend_from_slice(&s.to_le_bytes());
    }

    wav
}

// ─── VAD (Voice Activity Detection) ─────────────────────────────────────────

/// RMS energy threshold below which a frame is considered silence.
const ENERGY_THRESHOLD: f32 = 0.008;
/// Consecutive speech frames required to open a segment.
const SPEECH_CONFIRM_FRAMES: usize = 3;
/// Consecutive silence frames required to close a segment (~300ms at 100ms poll).
const SILENCE_END_FRAMES: usize = 15;
/// Maximum segment length in samples at native rate before forced flush (30s).
const MAX_SEGMENT_SECS: f32 = 30.0;

pub struct VadState {
    // Accumulated speech segment
    segment: Vec<f32>,
    segment_start_ms: u64,
    // State machine
    in_speech: bool,
    silence_frames: usize,
    speech_frames: usize,
    // Device properties (set on first push)
    native_sr: u32,
    native_ch: u16,
}

pub struct SpeechSegment {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub start_ms: u64,
    pub end_ms: u64,
}

impl VadState {
    pub fn new() -> Self {
        Self {
            segment: Vec::new(),
            segment_start_ms: 0,
            in_speech: false,
            silence_frames: 0,
            speech_frames: 0,
            native_sr: 48_000,
            native_ch: 1,
        }
    }

    /// Push a chunk of samples. Returns a completed segment when speech ends.
    pub fn push(
        &mut self,
        samples: &[f32],
        sample_rate: u32,
        channels: u16,
        chunk_start_ms: u64,
    ) -> Option<SpeechSegment> {
        self.native_sr = sample_rate;
        self.native_ch = channels;

        let rms = rms_energy(samples);
        let is_speech = rms >= ENERGY_THRESHOLD;

        if is_speech {
            self.speech_frames += 1;
            self.silence_frames = 0;

            if !self.in_speech && self.speech_frames >= SPEECH_CONFIRM_FRAMES {
                self.in_speech = true;
                self.segment_start_ms = chunk_start_ms;
                tracing::debug!("VAD: speech opened (rms={:.4})", rms);
            }
        } else {
            self.silence_frames += 1;
            self.speech_frames = 0;
        }

        if self.in_speech {
            self.segment.extend_from_slice(samples);
        }

        let max_samples = (MAX_SEGMENT_SECS * sample_rate as f32 * channels as f32) as usize;
        let segment_too_long = self.segment.len() >= max_samples;
        let speech_ended =
            self.in_speech && !is_speech && self.silence_frames >= SILENCE_END_FRAMES;

        if speech_ended || segment_too_long {
            self.flush(chunk_start_ms)
        } else {
            None
        }
    }

    fn flush(&mut self, end_ms: u64) -> Option<SpeechSegment> {
        let samples = std::mem::take(&mut self.segment);
        self.in_speech = false;
        self.silence_frames = 0;
        self.speech_frames = 0;

        let min_samples = self.native_sr as usize / 2 * self.native_ch as usize; // 0.5s
        if samples.len() < min_samples {
            tracing::debug!("VAD: discarding short segment ({} samples)", samples.len());
            return None;
        }

        tracing::debug!(
            "VAD: speech closed — {} samples (~{:.1}s)",
            samples.len(),
            samples.len() as f32 / (self.native_sr as f32 * self.native_ch as f32)
        );

        Some(SpeechSegment {
            samples,
            sample_rate: self.native_sr,
            channels: self.native_ch,
            start_ms: self.segment_start_ms,
            end_ms,
        })
    }
}

impl Default for VadState {
    fn default() -> Self {
        Self::new()
    }
}

fn rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

// ─── Background transcription thread ─────────────────────────────────────────

pub(crate) struct TranscriptionHandle {
    stop_tx: std::sync::mpsc::SyncSender<()>,
    _thread: std::thread::JoinHandle<()>,
}

impl TranscriptionHandle {
    pub(crate) fn spawn(
        transcriber: std::sync::Arc<dyn Transcriber>,
        mic_buf: std::sync::Arc<parking_lot::Mutex<VecDeque<crate::AudioChunk>>>,
        sys_buf: std::sync::Arc<parking_lot::Mutex<VecDeque<crate::AudioChunk>>>,
        transcript_out: std::sync::Arc<parking_lot::Mutex<VecDeque<TranscriptChunk>>>,
        source: AudioSource,
    ) -> Self {
        let (stop_tx, stop_rx) = std::sync::mpsc::sync_channel(1);

        let thread = std::thread::Builder::new()
            .name("cel-transcription".into())
            .spawn(move || {
                let mut mic_vad = VadState::new();
                let mut sys_vad = VadState::new();

                loop {
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }

                    // Drain and process mic chunks
                    if matches!(source, AudioSource::Microphone | AudioSource::Both) {
                        let chunks: Vec<_> = mic_buf.lock().drain(..).collect();
                        for chunk in chunks {
                            if let Some(seg) = mic_vad.push(
                                &chunk.samples,
                                chunk.sample_rate,
                                chunk.channels,
                                chunk.timestamp_ms,
                            ) {
                                let whisper_samples = prepare_for_whisper(
                                    &seg.samples,
                                    seg.sample_rate,
                                    seg.channels,
                                );
                                let wav = encode_wav(&whisper_samples, 16_000, 1);
                                if let Some(t) = transcriber.transcribe(
                                    &wav,
                                    AudioSource::Microphone,
                                    seg.start_ms,
                                    seg.end_ms,
                                ) {
                                    transcript_out.lock().push_back(t);
                                }
                            }
                        }
                    }

                    // Drain and process system audio chunks
                    if matches!(source, AudioSource::SystemOutput | AudioSource::Both) {
                        let chunks: Vec<_> = sys_buf.lock().drain(..).collect();
                        for chunk in chunks {
                            if let Some(seg) = sys_vad.push(
                                &chunk.samples,
                                chunk.sample_rate,
                                chunk.channels,
                                chunk.timestamp_ms,
                            ) {
                                let whisper_samples = prepare_for_whisper(
                                    &seg.samples,
                                    seg.sample_rate,
                                    seg.channels,
                                );
                                let wav = encode_wav(&whisper_samples, 16_000, 1);
                                if let Some(t) = transcriber.transcribe(
                                    &wav,
                                    AudioSource::SystemOutput,
                                    seg.start_ms,
                                    seg.end_ms,
                                ) {
                                    transcript_out.lock().push_back(t);
                                }
                            }
                        }
                    }

                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            })
            .expect("failed to spawn transcription thread");

        Self {
            stop_tx,
            _thread: thread,
        }
    }

    pub(crate) fn stop(self) {
        let _ = self.stop_tx.try_send(());
        // Thread exits within 100ms + any in-progress API call
    }
}
