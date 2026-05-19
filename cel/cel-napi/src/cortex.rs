//! NAPI bindings for the Rust Cortex perception engine.
//!
//! Exposes the Cortex lifecycle and model reading to TypeScript.
//! The Cortex runs its 200ms tick loop on the shared Tokio runtime.
//! Reading the model is instant — it accesses the `Arc<RwLock<MentalModel>>` directly.

use crate::rt_handle;
use napi_derive::napi;
use std::sync::{Arc, Mutex};

fn dirs_or_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
}

fn default_audio_capture() -> Option<(Box<dyn cel_audio::AudioCapture>, cel_audio::AudioConfig)> {
    if std::env::var("CEL_DISABLE_AUDIO")
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
    {
        return None;
    }

    let mut capture = cel_audio::CpalCapture::new();
    if let Some(config) = whisper_config_from_env() {
        capture.set_transcriber(Arc::new(cel_audio::WhisperApiTranscriber::new(config)));
    }

    let mut audio_config = cel_audio::AudioConfig::default();
    if let Ok(source) = std::env::var("CEL_AUDIO_SOURCE") {
        audio_config.source = match source.to_ascii_lowercase().as_str() {
            "microphone" | "mic" => cel_audio::AudioSource::Microphone,
            "system_output" | "system" | "speaker" => cel_audio::AudioSource::SystemOutput,
            _ => cel_audio::AudioSource::Both,
        };
    }

    Some((Box::new(capture), audio_config))
}

fn whisper_config_from_env() -> Option<cel_audio::WhisperApiConfig> {
    let endpoint = std::env::var("CEL_WHISPER_ENDPOINT").ok();
    let api_key = std::env::var("CEL_WHISPER_API_KEY")
        .ok()
        .or_else(|| std::env::var("OPENAI_API_KEY").ok());
    let model = std::env::var("CEL_WHISPER_MODEL").ok();
    let language = std::env::var("CEL_WHISPER_LANGUAGE").ok();

    if endpoint.is_none() && api_key.is_none() && model.is_none() && language.is_none() {
        return None;
    }

    let mut config = cel_audio::WhisperApiConfig::default();
    if let Some(endpoint) = endpoint.filter(|value| !value.is_empty()) {
        config.endpoint = endpoint;
    }
    if let Some(api_key) = api_key {
        config.api_key = api_key;
    }
    if let Some(model) = model.filter(|value| !value.is_empty()) {
        config.model = model;
    }
    if let Some(language) = language.filter(|value| !value.is_empty()) {
        config.language = Some(language);
    }
    Some(config)
}

/// All cortex state behind a single Mutex, lazily initialized.
struct CortexState {
    cortex: Option<Arc<cel_cortex::Cortex>>,
    model_handle: Option<Arc<tokio::sync::RwLock<cel_cortex::MentalModel>>>,
}

static STATE: std::sync::OnceLock<Mutex<CortexState>> = std::sync::OnceLock::new();

fn get_state() -> &'static Mutex<CortexState> {
    STATE.get_or_init(|| {
        Mutex::new(CortexState {
            cortex: None,
            model_handle: None,
        })
    })
}

/// Get the Cortex model handle for direct Rust access (used by goal_runner).
pub(crate) fn get_model_handle() -> Option<Arc<tokio::sync::RwLock<cel_cortex::MentalModel>>> {
    let state = get_state().lock().ok()?;
    state.model_handle.clone()
}

pub(crate) fn get_cortex_handle() -> Option<Arc<cel_cortex::Cortex>> {
    let state = get_state().lock().ok()?;
    state.cortex.clone()
}

/// Boot the Rust Cortex — starts the always-on 200ms perception tick loop.
#[napi]
pub fn boot_cortex() -> napi::Result<()> {
    // Ensure tracing is on before any `tracing::*` macro fires below.
    // Without this, the AX-permission warning (and any other boot-time
    // diagnostics) get silently dropped because the global subscriber
    // hasn't been installed yet.
    crate::ensure_tracing_init();

    let handle = rt_handle()?;

    let mut state = get_state()
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("State lock poisoned: {}", e)))?;

    if state.cortex.is_some() {
        return Err(napi::Error::from_reason("Cortex already running"));
    }

    // One-shot AX-trust check. Without this permission, every cortex tick
    // emits the same `Accessibility tree unavailable` WARN — easy to drown
    // in. Surfacing it once here, with the fix path, makes the failure
    // diagnosable without grepping logs.
    #[cfg(target_os = "macos")]
    if !cel_accessibility::ax_is_process_trusted() {
        tracing::warn!(
            target: "cel_napi::cortex",
            "macOS Accessibility permission missing for the host process. \
             AX-element observations will be empty until granted. \
             Fix: System Settings → Privacy & Security → Accessibility, \
             then enable the host (Terminal / Claude Desktop / Claude Code / \
             Cursor / etc.) and restart it. \
             CDP browser perception and adapter-driven app truth (Numbers, \
             Excel, …) work without AX trust."
        );
    }

    let a11y = cel_accessibility::create_tree();
    let display = cel_display::create_capture();
    let network = cel_network::create_monitor();
    let signals = cel_signals::create_signal_bus();
    let mut merger =
        cel_context::ContextMerger::with_all(a11y, display, network).with_signals(signals);
    if let Ok(vision) = cel_vision::create_provider_from_env() {
        merger = merger.with_vision(vision).with_runtime(handle.clone());
    }
    let mut stream_status = merger.stream_status();
    let observer = cel_accessibility::create_tree();

    // MCP server runs on the user's machine when they explicitly invoke it
    // — opt into native input. Browser actions still route through CDP
    // automatically when a CDP target is bound; this is the fallback for
    // desktop apps.
    let mut cortex = cel_cortex::Cortex::new("mcp-default".into()).with_native_input_unsafe();
    #[cfg(target_os = "macos")]
    cortex.register_adapter(Box::new(adapter_numbers::NumbersAdapter::new()));
    #[cfg(target_os = "macos")]
    cortex.register_adapter(Box::new(adapter_notes::NotesAdapter::new()));
    // Mail / Calendar / Reminders / Messages are NOT registered in-process
    // here. They ship as ProcessDriver adapters: each has an `adapter.json`
    // under `adapters/<name>/` (runtime: process) and a small binary built
    // by `cargo build --release -p adapter-<name>`. The
    // `cel_cortex::discover_adapters` loop below picks them up automatically
    // — to add a new productivity adapter, drop a folder under `adapters/`
    // or `~/.cellar/adapters/`, no edits to this file.
    // Register the native browser adapter so MCP clients automatically get
    // DOM perception when a CDP target is reachable. Registered without a
    // CDP client up front — `BrowserAdapter::probe()` returns false until
    // one is bound, so the cortex tick loop simply leaves the adapter
    // inactive in non-browser sessions. When the user's MCP host opens a
    // browser and `cel_cdp::connect_to_focused_app()` succeeds, the
    // adapter activates without any extra wiring.
    cortex.register_adapter(Box::new(adapter_browser::BrowserAdapter::new()));
    if let Some((capture, config)) = default_audio_capture() {
        cortex = cortex.with_audio(capture, config);
        stream_status.audio_capture = true;
    }

    // Discover and register adapters from known locations
    let adapter_dirs = [
        // Project-local adapters
        std::path::PathBuf::from("adapters"),
        // User-installed adapters
        dirs_or_home().join(".cellar").join("adapters"),
    ];
    for dir in &adapter_dirs {
        for (adapter_path, manifest) in cel_cortex::discover_adapters(dir) {
            if manifest.runtime == "process" {
                let driver = cel_cortex::ProcessDriver::new(manifest, adapter_path);
                cortex.register_adapter(Box::new(driver));
            }
        }
    }

    handle
        .block_on(async { cortex.boot(merger, observer).await })
        .map_err(|e| napi::Error::from_reason(format!("Cortex boot failed: {}", e)))?;

    let cortex = Arc::new(cortex);
    handle.block_on(async {
        let model_handle = cortex.model();
        let mut model = model_handle.write().await;
        model.stream_status = stream_status;
    });
    state.model_handle = Some(cortex.model());
    state.cortex = Some(cortex);
    Ok(())
}

/// Read the current mental model as JSON. Instant — reads shared memory.
#[napi]
pub fn read_cortex_model() -> napi::Result<String> {
    let handle = rt_handle()?;
    let state = get_state()
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("State lock poisoned: {}", e)))?;

    let model_lock = state
        .model_handle
        .as_ref()
        .ok_or_else(|| napi::Error::from_reason("Cortex not running. Call boot_cortex() first."))?;

    let mut model = handle.block_on(async { model_lock.read().await.clone() });
    let last_event_ms = model
        .freshness
        .as_ref()
        .and_then(|freshness| freshness.last_event_ms);
    let last_significant_event_ms = model
        .freshness
        .as_ref()
        .and_then(|freshness| freshness.last_significant_event_ms);
    model.refresh_derived(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        last_event_ms,
        last_significant_event_ms,
    );
    serde_json::to_string(&model)
        .map_err(|e| napi::Error::from_reason(format!("Model serialize error: {}", e)))
}

/// Notify the Cortex that an action was taken. Resets idle tracking.
#[napi]
pub fn notify_cortex_action(action: String) -> napi::Result<()> {
    let handle = rt_handle()?;
    let state = get_state()
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("State lock poisoned: {}", e)))?;

    let cortex = state
        .cortex
        .as_ref()
        .ok_or_else(|| napi::Error::from_reason("Cortex not running"))?;

    handle.block_on(cortex.notify_action(&action));
    Ok(())
}

/// Report a consecutive action failure to the Cortex.
#[napi]
pub fn report_cortex_action_failure() -> napi::Result<()> {
    let handle = rt_handle()?;
    let state = get_state()
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("State lock poisoned: {}", e)))?;

    let cortex = state
        .cortex
        .as_ref()
        .ok_or_else(|| napi::Error::from_reason("Cortex not running"))?;

    handle.block_on(cortex.report_action_failure());
    Ok(())
}

/// Report a successful action to the Cortex.
#[napi]
pub fn report_cortex_action_success() -> napi::Result<()> {
    let handle = rt_handle()?;
    let state = get_state()
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("State lock poisoned: {}", e)))?;

    let cortex = state
        .cortex
        .as_ref()
        .ok_or_else(|| napi::Error::from_reason("Cortex not running"))?;

    handle.block_on(cortex.report_action_success());
    Ok(())
}

/// Consume anomalies from the Cortex's anomaly queue. Returns JSON array.
#[napi]
pub fn consume_cortex_anomalies() -> napi::Result<String> {
    let handle = rt_handle()?;
    let state = get_state()
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("State lock poisoned: {}", e)))?;

    let cortex = state
        .cortex
        .as_ref()
        .ok_or_else(|| napi::Error::from_reason("Cortex not running"))?;

    let anomalies = handle.block_on(cortex.consume_anomalies());
    serde_json::to_string(&anomalies)
        .map_err(|e| napi::Error::from_reason(format!("Serialize error: {}", e)))
}

/// Check if the Cortex is currently running.
#[napi]
pub fn is_cortex_running() -> bool {
    let state = match get_state().lock() {
        Ok(s) => s,
        Err(_) => return false,
    };
    state.cortex.as_ref().is_some_and(|c| c.is_running())
}

// ─── Liveness API (Phase 1) ───────────────────────────────────────────────
// These read lock-free atomics on the Cortex handle — no tokio runtime
// needed, safe to call at any rate. Return 0 / None when no Cortex exists
// so callers can treat "not booted" as "no activity" without special-casing.

/// Total successful ticks since boot. 0 if Cortex not running.
#[napi]
pub fn cortex_tick_count() -> u32 {
    let state = match get_state().lock() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    state
        .cortex
        .as_ref()
        .map_or(0, |c| c.tick_count().min(u32::MAX as u64) as u32)
}

/// Count of `refresh_now` calls that timed out waiting for a tick.
#[napi]
pub fn cortex_stalled_ticks() -> u32 {
    let state = match get_state().lock() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    state
        .cortex
        .as_ref()
        .map_or(0, |c| c.stalled_ticks().min(u32::MAX as u64) as u32)
}

/// Milliseconds since the last successful tick. `None` if no tick has fired
/// (cortex not booted, or boot is in progress). A steadily-growing number
/// means the tick loop is stalled.
#[napi]
pub fn cortex_last_tick_age_ms() -> Option<u32> {
    let state = match get_state().lock() {
        Ok(s) => s,
        Err(_) => return None,
    };
    state
        .cortex
        .as_ref()
        .and_then(|c| c.last_tick_age_ms())
        .map(|v| v.min(u32::MAX as u64) as u32)
}

/// Force an out-of-band tick and return when it completes. Returns the
/// tick_count after the triggered tick. Times out after `timeout_ms`
/// (default 500); on timeout, `stalled_ticks` increments and this returns
/// an Err.
#[napi]
pub async fn cortex_refresh_now(timeout_ms: Option<u32>) -> napi::Result<u32> {
    // Snapshot the cortex Arc under the sync mutex, then drop the guard
    // before awaiting — tokio futures can't cross a std::sync::Mutex hold.
    let cortex = {
        let state = get_state()
            .lock()
            .map_err(|e| napi::Error::from_reason(format!("State lock poisoned: {}", e)))?;
        state.cortex.as_ref().cloned().ok_or_else(|| {
            napi::Error::from_reason("Cortex not running. Call boot_cortex() first.")
        })?
    };

    let timeout = timeout_ms.map(|v| v as u64);
    cortex
        .refresh_now(timeout)
        .await
        .map(|count| count.min(u32::MAX as u64) as u32)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Stop the Cortex — stops the background perception loop.
#[napi]
pub fn stop_cortex() -> napi::Result<()> {
    let mut state = get_state()
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("State lock poisoned: {}", e)))?;

    if let Some(cortex) = state.cortex.as_ref() {
        cortex.shutdown();
    }
    state.cortex = None;
    state.model_handle = None;
    Ok(())
}
