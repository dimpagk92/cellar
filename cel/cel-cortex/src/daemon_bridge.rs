//! Bridge trait for forwarding Cortex-observed events to an external daemon.
//!
//! `cel-cortex` deliberately does NOT depend on `cellar-ipc` — that would
//! couple the perception engine to one specific transport. The Tauri app (or
//! any other host) implements this trait by wrapping its IPC client and
//! injecting it via `Cortex::with_daemon_bridge`.
//!
//! The tick loop calls `forward` fire-and-forget: the implementation must
//! not block. Spawn a task internally if async work is required.

use cellar_types::event::Event;

/// Implemented by the host process to forward Cortex-observed events into the
/// daemon's event bus. The daemon's rule matcher then sees the full Cortex
/// stream — AX (app/window/element), CDP (`url_changed`, `page_loaded`),
/// network connections, audio activity, and keyboard/pointer input.
pub trait DaemonBridge: Send + Sync + 'static {
    /// Forward a single event to the daemon. Must not block the caller.
    fn forward(&self, event: Event);
}
