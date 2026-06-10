//! Shared "boot a default Cortex" helper.
//!
//! The Cortex boot sequence — build the standard perception [`ContextMerger`]
//! (AX + display + network + signals, optional vision/audio) → [`Cortex::new`]
//! → [`cel_adapters::register_default_adapters`] → `cortex.boot(...)` (which
//! starts the background perception tick loop) — was historically inlined in
//! both `cel-napi` (the MCP host) and `cel-eval` (benchmarks). This crate is the
//! single owner of that path, parameterised by [`BootOpts`], so every host
//! (MCP, eval, the daemon) boots the same way — no per-host divergence.
//!
//! [`ContextMerger`]: cel_context::ContextMerger

use std::sync::Arc;

use cel_cortex::Cortex;

/// A host-built audio capture + its config (from `cel-audio`). The host owns
/// device selection; the helper just wires it into the Cortex when present.
pub type AudioSource = (Box<dyn cel_audio::AudioCapture>, cel_audio::AudioConfig);

/// Options for [`boot_default_cortex`].
///
/// Defaults ([`BootOpts::new`]) are the safe/headless choice: no native input,
/// ambient CDP discovery, no vision/audio. Hosts opt into more:
/// - **MCP** (`cel-napi`): `native_input = true`, `vision = true` + `runtime`,
///   `audio = default_audio_capture()`, ambient CDP.
/// - **eval** (`cel-eval`): pinned `cdp_client`, `native_input` only behind the
///   foreground-leak flag, no vision/audio.
pub struct BootOpts {
    /// Cortex id (`"mcp-default"`, `"cel-eval"`, `"daemon"`, …).
    pub id: String,
    /// Enable native input (CGEvent mouse/keyboard/AX/app-activation). Off in
    /// eval/CI; on for the MCP host and the app daemon.
    pub native_input: bool,
    /// Pinned CDP client — eval targets a specific headless browser. `None` =
    /// ambient discovery (`connect_to_focused_app`), used by MCP/daemon.
    pub cdp_client: Option<Arc<cel_cdp::CdpClient>>,
    /// Attach a vision provider from env (`cel_vision::create_provider_from_env`).
    /// Requires [`BootOpts::runtime`]; a missing/erroring provider is skipped.
    pub vision: bool,
    /// Tokio runtime handle for the vision provider's background work.
    pub runtime: Option<tokio::runtime::Handle>,
    /// Host-built audio capture, when audio perception is wanted.
    pub audio: Option<AudioSource>,
}

impl BootOpts {
    /// Safe defaults: no native input, ambient CDP, no vision/audio.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            native_input: false,
            cdp_client: None,
            vision: false,
            runtime: None,
            audio: None,
        }
    }
}

/// Build the standard perception merger + default adapters and boot the Cortex
/// tick loop. Returns the booted `Arc<Cortex>`, with `model.stream_status`
/// reflecting which perception streams are live.
pub async fn boot_default_cortex(opts: BootOpts) -> Result<Arc<Cortex>, String> {
    let BootOpts {
        id,
        native_input,
        cdp_client,
        vision,
        runtime,
        audio,
    } = opts;

    // One-shot AX-trust check — surface the missing-permission failure once,
    // with the fix path, instead of one WARN per tick.
    #[cfg(target_os = "macos")]
    if !cel_accessibility::ax_is_process_trusted() {
        tracing::warn!(
            target: "cel_boot",
            "macOS Accessibility permission missing for this process. AX-element \
             observations will be empty until granted: System Settings → Privacy & \
             Security → Accessibility, then enable the host and restart it. CDP \
             browser perception and adapter-driven app truth (Numbers, Excel, …) \
             work without AX trust."
        );
    }

    // Perception sources → merger.
    let a11y = cel_accessibility::create_tree();
    let display = cel_display::create_capture();
    let network = cel_network::create_monitor();
    let signals = cel_signals::create_signal_bus();
    let mut merger =
        cel_context::ContextMerger::with_all(a11y, display, network).with_signals(signals);
    if vision {
        if let (Ok(provider), Some(rt)) = (cel_vision::create_provider_from_env(), runtime) {
            merger = merger.with_vision(provider).with_runtime(rt);
        }
    }
    let mut stream_status = merger.stream_status();
    let observer = cel_accessibility::create_tree();

    // Cortex + execution config.
    let mut cortex = Cortex::new(id);
    if native_input {
        cortex = cortex.with_native_input_unsafe();
    }
    if let Some(client) = cdp_client.clone() {
        cortex = cortex.with_cdp_client(client);
    }
    if let Some((capture, config)) = audio {
        cortex = cortex.with_audio(capture, config);
        stream_status.audio_capture = true;
    }

    // Default adapter set (in-process Numbers/Notes/Browser + process-driver
    // discovery). `cdp_client` here is the same Option: `Some` pins the browser
    // adapter to the eval browser, `None` lets it self-activate via ambient CDP.
    cel_adapters::register_default_adapters(&mut cortex, cdp_client);

    cortex
        .boot(merger, observer)
        .await
        .map_err(|e| format!("cortex boot failed: {e}"))?;

    let cortex = Arc::new(cortex);
    {
        let model_handle = cortex.model();
        let mut model = model_handle.write().await;
        model.stream_status = stream_status;
    }
    Ok(cortex)
}
