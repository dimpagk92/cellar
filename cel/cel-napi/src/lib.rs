//! CEL Node.js Native Bindings
//!
//! Exposes the CEL unified context API to TypeScript via napi-rs.

// All public functions in this crate are consumed via FFI (napi-rs), not Rust calls.
// The test harness incorrectly reports them as unused.
#![allow(dead_code)]
// The ctor crate (used by napi-rs for module registration) triggers cfg warnings
// on newer Rust compilers. Safe to suppress until napi-rs updates ctor.
#![allow(unexpected_cfgs)]

use napi_derive::napi;

mod adapter_registry;
mod cdp;
mod context;
mod cortex;
mod goal_runner;
mod input;
mod llm;
mod planner;
mod store;
mod watchdog;

/// Initialize tracing subscriber on first access so RUST_LOG works inside the
/// NAPI-hosted runtime. Without this, all cel-crates `tracing::*` calls are
/// silently dropped and the host can't diagnose what the agent is doing.
static TRACING_INIT: std::sync::Once = std::sync::Once::new();

fn ensure_tracing_init() {
    TRACING_INIT.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init();
    });
}

#[napi]
pub fn init_tracing() {
    ensure_tracing_init();
}

/// Shared Tokio runtime — created once, reused across all async operations.
static TOKIO_RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

pub(crate) fn rt_handle() -> napi::Result<tokio::runtime::Handle> {
    Ok(TOKIO_RT
        .get_or_init(|| tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"))
        .handle()
        .clone())
}

/// Get the CEL runtime version.
#[napi]
pub fn cel_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Check whether the current host process has been granted macOS Accessibility
/// permission. Returns true when granted, false when denied. On non-macOS
/// platforms returns true (no-op — Accessibility permission is a macOS concept).
///
/// Pure boolean check via `AXIsProcessTrusted()` — does not prompt the user.
/// Cheap (microseconds) so safe to call on every MCP tool invocation as a
/// pre-flight guard rather than failing late inside an AX traversal.
#[napi]
pub fn ax_permission_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        cel_accessibility::ax_is_process_trusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}
