//! CEL Supplementary Signal Sources
//!
//! Captures OS-level signals beyond the accessibility tree: clipboard,
//! visible windows, audio state, power, running apps, recent file changes.
//!
//! Each signal source is polled independently. The `SignalBus` aggregates
//! all sources and provides a `snapshot()` method that the ContextMerger
//! calls during `get_context()`.

mod active_app;
mod audio;
mod clipboard;
pub mod gesture;
mod power;
mod recent_files;
mod window_list;

pub use active_app::RunningApp;
pub use audio::AudioState;
pub use clipboard::ClipboardState;
pub use gesture::GestureObserver;
pub use power::PowerState;
pub use recent_files::RecentFile;
pub use window_list::WindowState;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    #[error("Signal source unavailable: {0}")]
    Unavailable(String),
    #[error("Signal query failed: {0}")]
    Failed(String),
}

/// Aggregated snapshot of all supplementary signals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SignalSnapshot {
    /// Current clipboard contents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<ClipboardState>,
    /// All visible windows on screen (not just the focused app).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub window_list: Vec<WindowState>,
    /// Audio output state (volume, muted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioState>,
    /// Battery/power state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<PowerState>,
    /// Running GUI applications.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_apps: Vec<RunningApp>,
    /// Recently created/modified files in Downloads/Desktop (last 60s).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_files: Vec<RecentFile>,
}

/// The signal bus — aggregates all supplementary signal sources.
/// Feeds into the ContextMerger as another stream.
pub trait SignalBus: Send + Sync {
    /// Take a snapshot of all current signal state. Must be cheap and non-blocking.
    fn snapshot(&self) -> SignalSnapshot;
}

/// Platform-appropriate signal bus with TTL caching.
/// Expensive signals (running_apps, audio, power) are cached for 5 seconds.
/// Cheap signals (clipboard, window_list) are always fresh.
pub struct PlatformSignalBus {
    cache: std::sync::Mutex<CachedSignals>,
}

struct CachedSignals {
    audio: Option<AudioState>,
    power: Option<PowerState>,
    running_apps: Vec<RunningApp>,
    last_cached: std::time::Instant,
}

/// Cache TTL for expensive signals (osascript calls ~100ms each).
const SIGNAL_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

impl PlatformSignalBus {
    pub fn new() -> Self {
        Self {
            cache: std::sync::Mutex::new(CachedSignals {
                audio: None,
                power: None,
                running_apps: vec![],
                last_cached: std::time::Instant::now() - SIGNAL_CACHE_TTL,
            }),
        }
    }
}

impl Default for PlatformSignalBus {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalBus for PlatformSignalBus {
    fn snapshot(&self) -> SignalSnapshot {
        // Always fresh: clipboard and window list (fast native calls)
        let clipboard = clipboard::read_clipboard();
        let window_list = window_list::list_windows();
        let recent_files = recent_files::recent_downloads(60);

        // Cached: audio, power, running_apps (expensive osascript calls)
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.last_cached.elapsed() >= SIGNAL_CACHE_TTL {
            cache.audio = audio::read_audio_state();
            cache.power = power::read_power_state();
            cache.running_apps = active_app::list_running_apps();
            cache.last_cached = std::time::Instant::now();
        }

        SignalSnapshot {
            clipboard,
            window_list,
            audio: cache.audio.clone(),
            power: cache.power.clone(),
            running_apps: cache.running_apps.clone(),
            recent_files,
        }
    }
}

/// Create a platform-appropriate signal bus.
pub fn create_signal_bus() -> Box<dyn SignalBus> {
    Box::new(PlatformSignalBus::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_bus_snapshot_does_not_panic() {
        let bus = PlatformSignalBus::new();
        let snap = bus.snapshot();
        let _ = serde_json::to_string(&snap).unwrap();
    }

    #[test]
    fn test_signal_snapshot_default() {
        let snap = SignalSnapshot::default();
        assert!(snap.clipboard.is_none());
        assert!(snap.window_list.is_empty());
        assert!(snap.audio.is_none());
        assert!(snap.power.is_none());
        assert!(snap.running_apps.is_empty());
        assert!(snap.recent_files.is_empty());
    }
}
