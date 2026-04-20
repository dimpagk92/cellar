//! Real audio capture backend using cpal.
//!
//! # Microphone
//! Uses the platform's default input device via cpal (CoreAudio on macOS,
//! ALSA/PipeWire on Linux, WASAPI on Windows).
//!
//! # System output (what the user hears)
//! Platform-specific:
//! - **Linux**: PulseAudio/PipeWire always exposes `*.monitor` devices —
//!   we auto-detect the first one. No extra setup required.
//! - **macOS**: Requires a virtual loopback device (Blackhole, SoundFlower,
//!   etc.) OR macOS 12.3+ with Screen Recording permission.
//!   We scan for known loopback devices by name prefix and use the first
//!   match. If none found, `AudioSource::SystemOutput` returns
//!   `AudioError::Unavailable` with install instructions.
//! - **Windows**: WASAPI loopback is detected automatically via cpal.
//!
//! Transcription is not implemented here — `drain_transcripts()` returns
//! empty. Wire in `whisper-rs` or a local Whisper HTTP endpoint at the
//! `cel-cortex` level.

use crate::transcribe::{TranscriptionHandle, Transcriber};
use crate::{AudioCapture, AudioChunk, AudioConfig, AudioError, AudioSource, TranscriptChunk};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ─── Ring buffer ─────────────────────────────────────────────────────────────

/// Fixed-capacity ring buffer for f32 audio samples.
struct RingBuffer {
    buf: VecDeque<f32>,
    capacity: usize,
}

impl RingBuffer {
    fn new(sample_rate: u32, channels: u16, ring_secs: u32) -> Self {
        let capacity = sample_rate as usize * channels as usize * ring_secs as usize;
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn push_slice(&mut self, samples: &[f32]) {
        for &s in samples {
            if self.buf.len() == self.capacity {
                self.buf.pop_front();
            }
            self.buf.push_back(s);
        }
    }
}

// ─── Shared capture state ────────────────────────────────────────────────────

struct CaptureState {
    ring: RingBuffer,
    chunks: VecDeque<AudioChunk>,
}

impl CaptureState {
    fn new(config: &AudioConfig) -> Self {
        Self {
            ring: RingBuffer::new(config.sample_rate, config.channels, config.ring_buffer_secs),
            chunks: VecDeque::new(),
        }
    }

    /// Push samples, add to internal chunk queue, and return a clone for
    /// the transcription buffer.
    fn push_returning(
        &mut self,
        source: AudioSource,
        samples: &[f32],
        sample_rate: u32,
        channels: u16,
    ) -> AudioChunk {
        self.ring.push_slice(samples);
        let chunk = AudioChunk {
            source,
            samples: samples.to_vec(),
            sample_rate,
            channels,
            timestamp_ms: now_ms(),
        };
        self.chunks.push_back(chunk.clone());
        while self.chunks.len() > 200 {
            self.chunks.pop_front();
        }
        chunk
    }

    fn drain_chunks(&mut self) -> Vec<AudioChunk> {
        self.chunks.drain(..).collect()
    }
}

// ─── Stream handle ───────────────────────────────────────────────────────────

/// Keeps cpal streams alive for their duration. Drop = stop.
struct StreamHandles {
    _mic: Option<cpal::Stream>,
    _sys: Option<cpal::Stream>,
}

// cpal::Stream on CoreAudio holds platform-specific non-Send/Sync types, but
// the streams are accessed only through &mut CpalCapture (start/stop) and
// their internal callbacks run on the audio thread without sharing the handle.
unsafe impl Send for StreamHandles {}
unsafe impl Sync for StreamHandles {}

// ─── CpalCapture ─────────────────────────────────────────────────────────────

pub struct CpalCapture {
    started: bool,
    /// Raw chunk buffer — drained by `drain_raw_chunks()`.
    mic_raw: Arc<Mutex<CaptureState>>,
    sys_raw: Arc<Mutex<CaptureState>>,
    /// Transcription input buffers — drained by the background thread.
    mic_tx: Arc<Mutex<VecDeque<AudioChunk>>>,
    sys_tx: Arc<Mutex<VecDeque<AudioChunk>>>,
    /// Completed transcripts — drained by `drain_transcripts()`.
    transcript_out: Arc<Mutex<VecDeque<TranscriptChunk>>>,
    streams: Option<StreamHandles>,
    transcription: Option<TranscriptionHandle>,
    transcriber: Option<Arc<dyn Transcriber>>,
    config: Option<AudioConfig>,
}

impl CpalCapture {
    pub fn new() -> Self {
        let dummy = AudioConfig::default();
        Self {
            started: false,
            mic_raw: Arc::new(Mutex::new(CaptureState::new(&dummy))),
            sys_raw: Arc::new(Mutex::new(CaptureState::new(&dummy))),
            mic_tx: Arc::new(Mutex::new(VecDeque::new())),
            sys_tx: Arc::new(Mutex::new(VecDeque::new())),
            transcript_out: Arc::new(Mutex::new(VecDeque::new())),
            streams: None,
            transcription: None,
            transcriber: None,
            config: None,
        }
    }

    /// Attach a transcription backend. Call before `start()`.
    /// Once capture is running, transcripts become available via
    /// `drain_transcripts()` after each speech segment closes (~0.3s silence).
    pub fn set_transcriber(&mut self, t: Arc<dyn Transcriber>) {
        self.transcriber = Some(t);
    }

    fn start_mic_stream(
        _config: &AudioConfig,
        raw: Arc<Mutex<CaptureState>>,
        tx: Arc<Mutex<VecDeque<AudioChunk>>>,
    ) -> Result<cpal::Stream, AudioError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| AudioError::DeviceError("No default input device found".into()))?;

        // Use the device's native config — resampling to 16 kHz for Whisper
        // happens downstream. Forcing 16kHz here breaks devices that only
        // support 44100/48000 Hz.
        let supported = device
            .default_input_config()
            .map_err(|e| AudioError::DeviceError(e.to_string()))?;

        let sr = supported.sample_rate().0;
        let ch = supported.channels();
        let cpal_config = supported.config();

        macro_rules! mic_callback {
            ($data:expr) => {{
                let chunk = raw.lock().push_returning(AudioSource::Microphone, $data, sr, ch);
                let mut tb = tx.lock();
                if tb.len() < 500 {
                    tb.push_back(chunk);
                }
            }};
        }

        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &cpal_config,
                move |data: &[f32], _| mic_callback!(data),
                move |err| tracing::error!("mic capture error: {}", err),
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &cpal_config,
                move |data: &[i16], _| {
                    let f32s: Vec<f32> =
                        data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    mic_callback!(&f32s)
                },
                move |err| tracing::error!("mic capture error: {}", err),
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &cpal_config,
                move |data: &[u16], _| {
                    let f32s: Vec<f32> =
                        data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect();
                    mic_callback!(&f32s)
                },
                move |err| tracing::error!("mic capture error: {}", err),
                None,
            ),
            fmt => {
                return Err(AudioError::DeviceError(format!(
                    "Unsupported sample format: {:?}",
                    fmt
                )))
            }
        }
        .map_err(|e| AudioError::DeviceError(e.to_string()))?;

        stream
            .play()
            .map_err(|e| AudioError::DeviceError(e.to_string()))?;

        tracing::info!(
            "Microphone capture started: {} @ {} Hz / {} ch",
            device.name().unwrap_or_default(),
            sr,
            ch
        );
        Ok(stream)
    }

    fn find_system_output_device(host: &cpal::Host) -> Option<cpal::Device> {
        let devices = match host.input_devices() {
            Ok(d) => d,
            Err(_) => return None,
        };

        // Known loopback device name prefixes, in priority order.
        const LOOPBACK_PREFIXES: &[&str] = &[
            // Linux — PulseAudio / PipeWire monitor devices
            ".monitor",
            // macOS — common virtual loopback devices
            "BlackHole",
            "Blackhole",
            "blackhole",
            "SoundFlower",
            "Soundflower",
            "Loopback",
            "loopback",
            "VB-Cable",
        ];

        let mut candidates: Vec<(usize, cpal::Device)> = devices
            .filter_map(|d| {
                let name = d.name().ok()?;
                let priority = LOOPBACK_PREFIXES
                    .iter()
                    .position(|&p| name.contains(p))?;
                Some((priority, d))
            })
            .collect();

        candidates.sort_by_key(|(p, _)| *p);
        candidates.into_iter().map(|(_, d)| d).next()
    }

    fn start_sys_stream(
        _config: &AudioConfig,
        raw: Arc<Mutex<CaptureState>>,
        tx: Arc<Mutex<VecDeque<AudioChunk>>>,
    ) -> Result<cpal::Stream, AudioError> {
        let host = cpal::default_host();

        let device = Self::find_system_output_device(&host).ok_or_else(|| {
            #[cfg(target_os = "macos")]
            tracing::warn!(
                "System audio: no loopback device found. \
                 Install Blackhole (https://github.com/ExistentialAudio/BlackHole) \
                 and set it as the output device."
            );
            #[cfg(target_os = "linux")]
            tracing::warn!(
                "System audio: no PulseAudio/PipeWire monitor device found. \
                 Ensure PipeWire or PulseAudio is running."
            );
            AudioError::Unavailable
        })?;

        let device_name = device.name().unwrap_or_default();

        let supported = device
            .default_input_config()
            .map_err(|e| AudioError::DeviceError(e.to_string()))?;

        let sr = supported.sample_rate().0;
        let ch = supported.channels();
        let cpal_config = supported.config();

        macro_rules! sys_callback {
            ($data:expr) => {{
                let chunk =
                    raw.lock().push_returning(AudioSource::SystemOutput, $data, sr, ch);
                let mut tb = tx.lock();
                if tb.len() < 500 {
                    tb.push_back(chunk);
                }
            }};
        }

        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &cpal_config,
                move |data: &[f32], _| sys_callback!(data),
                move |err| tracing::error!("system audio capture error: {}", err),
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &cpal_config,
                move |data: &[i16], _| {
                    let f32s: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    sys_callback!(&f32s)
                },
                move |err| tracing::error!("system audio capture error: {}", err),
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &cpal_config,
                move |data: &[u16], _| {
                    let f32s: Vec<f32> =
                        data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect();
                    sys_callback!(&f32s)
                },
                move |err| tracing::error!("system audio capture error: {}", err),
                None,
            ),
            fmt => {
                return Err(AudioError::DeviceError(format!(
                    "Unsupported sample format: {:?}",
                    fmt
                )))
            }
        }
        .map_err(|e| AudioError::DeviceError(e.to_string()))?;

        stream
            .play()
            .map_err(|e| AudioError::DeviceError(e.to_string()))?;

        tracing::info!(
            "System audio capture started via loopback device: {} @ {} Hz / {} ch",
            device_name,
            sr,
            ch
        );
        Ok(stream)
    }
}

impl Default for CpalCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioCapture for CpalCapture {
    fn start(&mut self, config: AudioConfig) -> Result<(), AudioError> {
        if self.started {
            return Err(AudioError::AlreadyCapturing);
        }

        // Re-initialize buffers
        self.mic_raw = Arc::new(Mutex::new(CaptureState::new(&config)));
        self.sys_raw = Arc::new(Mutex::new(CaptureState::new(&config)));
        self.mic_tx = Arc::new(Mutex::new(VecDeque::new()));
        self.sys_tx = Arc::new(Mutex::new(VecDeque::new()));
        self.transcript_out = Arc::new(Mutex::new(VecDeque::new()));

        let mut mic_stream: Option<cpal::Stream> = None;
        let mut sys_stream: Option<cpal::Stream> = None;

        if matches!(config.source, AudioSource::Microphone | AudioSource::Both) {
            match Self::start_mic_stream(
                &config,
                Arc::clone(&self.mic_raw),
                Arc::clone(&self.mic_tx),
            ) {
                Ok(s) => mic_stream = Some(s),
                Err(e) => {
                    tracing::error!("Microphone capture failed: {}", e);
                    if config.source == AudioSource::Microphone {
                        return Err(e);
                    }
                }
            }
        }

        if matches!(config.source, AudioSource::SystemOutput | AudioSource::Both) {
            match Self::start_sys_stream(
                &config,
                Arc::clone(&self.sys_raw),
                Arc::clone(&self.sys_tx),
            ) {
                Ok(s) => sys_stream = Some(s),
                Err(e) => {
                    tracing::warn!("System audio capture unavailable: {}", e);
                    if config.source == AudioSource::SystemOutput {
                        return Err(e);
                    }
                }
            }
        }

        // Spawn transcription thread if a backend was provided
        if let Some(ref t) = self.transcriber {
            self.transcription = Some(TranscriptionHandle::spawn(
                Arc::clone(t),
                Arc::clone(&self.mic_tx),
                Arc::clone(&self.sys_tx),
                Arc::clone(&self.transcript_out),
                config.source,
            ));
        }

        self.streams = Some(StreamHandles {
            _mic: mic_stream,
            _sys: sys_stream,
        });
        self.config = Some(config);
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        if !self.started {
            return Err(AudioError::NotCapturing);
        }
        if let Some(handle) = self.transcription.take() {
            handle.stop();
        }
        self.streams = None;
        self.started = false;
        tracing::info!("Audio capture stopped");
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.started
    }

    fn drain_transcripts(&mut self) -> Vec<TranscriptChunk> {
        self.transcript_out.lock().drain(..).collect()
    }

    fn drain_raw_chunks(&mut self) -> Vec<AudioChunk> {
        let mut chunks = self.mic_raw.lock().drain_chunks();
        chunks.extend(self.sys_raw.lock().drain_chunks());
        chunks
    }
}
