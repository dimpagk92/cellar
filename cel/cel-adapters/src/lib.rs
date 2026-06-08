//! Single modular registry for CEL's default adapter set.
//!
//! Every host that boots a production [`Cortex`] — the MCP server
//! (`cel-napi`), the eval harness (`cel-eval`), and the desktop app
//! (`cellar-cellar`) — wires the SAME adapter set through
//! [`register_default_adapters`] instead of duplicating
//! `cortex.register_adapter(...)` lines. Adding a new in-process adapter is a
//! one-line change here that every host inherits automatically.
//!
//! Two layers are registered:
//! 1. **In-process Rust adapters** linked into the binary: Numbers, Notes
//!    (macOS only) and the native Browser adapter.
//! 2. **Process-driver adapters** discovered from `adapters/` and
//!    `~/.cellar/adapters/` via [`cel_cortex::discover_adapters`] — Mail,
//!    Calendar, Reminders, Messages, and any user-installed adapter. Drop a
//!    folder with an `adapter.json` (`runtime: "process"`) into either
//!    location and every host picks it up with no code change.

use std::path::PathBuf;
use std::sync::Arc;

use cel_cdp::CdpClient;
use cel_cortex::Cortex;

/// Register CEL's default adapter set into `cortex`.
///
/// `browser_cdp` controls how the native browser adapter is constructed:
/// - `Some(client)` binds the adapter to an explicit CDP client up front —
///   used by the eval harness, which needs deterministic targeting of a
///   headless browser that is never the OS-focused app.
/// - `None` registers the adapter in ambient-discovery mode: it self-activates
///   via `cel_cdp::connect_to_focused_app()` when a browser is focused. This is
///   what the MCP server and desktop app use.
///
/// Must be called BEFORE [`Cortex::boot`], like all `register_adapter` calls.
pub fn register_default_adapters(cortex: &mut Cortex, browser_cdp: Option<Arc<CdpClient>>) {
    // ── In-process Rust adapters ────────────────────────────────────────
    // Numbers / Notes are macOS-only (AppleScript document model). The
    // `register_adapter` platform gate would skip them on other OSes anyway,
    // but cfg-gating keeps the deps from being linked where they can't run.
    #[cfg(target_os = "macos")]
    {
        cortex.register_adapter(Box::new(adapter_numbers::NumbersAdapter::new()));
        cortex.register_adapter(Box::new(adapter_notes::NotesAdapter::new()));
    }

    // Native browser adapter: DOM perception via cel-cdp. With `None` the
    // adapter stays inactive until `connect_to_focused_app()` finds a browser;
    // with `Some` it targets the supplied client deterministically.
    let browser = match browser_cdp {
        Some(client) => adapter_browser::BrowserAdapter::with_cdp_client(client),
        None => adapter_browser::BrowserAdapter::new(),
    };
    cortex.register_adapter(Box::new(browser));

    // ── Process-driver adapters (Mail/Calendar/Reminders/Messages/…) ────
    register_discovered_adapters(cortex);
}

/// Scan the standard adapter directories and register every process-runtime
/// adapter found. Split out so hosts that want only the discovered set can call
/// it directly; [`register_default_adapters`] calls it after the in-process set.
pub fn register_discovered_adapters(cortex: &mut Cortex) {
    for dir in default_adapter_dirs() {
        for (adapter_path, manifest) in cel_cortex::discover_adapters(&dir) {
            if manifest.runtime == "process" {
                let driver = cel_cortex::ProcessDriver::new(manifest, adapter_path);
                cortex.register_adapter(Box::new(driver));
            }
        }
    }
}

/// The standard locations scanned for process-driver adapters: the
/// project-local `adapters/` directory and the user-installed
/// `~/.cellar/adapters/` directory.
fn default_adapter_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    vec![
        PathBuf::from("adapters"),
        home.join(".cellar").join("adapters"),
    ]
}
