//! CEL event types for the watchdog system.

use serde::{Deserialize, Serialize};

/// Events emitted by the ContextWatchdog when screen state changes.
/// Includes both polling-based detections and push-based AXObserver notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CelEvent {
    /// Accessibility tree changed (elements added or removed).
    TreeChanged {
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// Network became idle (no new connections recently).
    NetworkIdle,
    /// Keyboard/mouse focus moved to a different element.
    FocusChanged {
        old: Option<String>,
        new: Option<String>,
    },
    /// An element's value changed (from AXObserver push notification).
    ValueChanged {
        element_id: String,
        new_value: Option<String>,
    },
    /// A new window was created (from AXObserver).
    WindowCreated { title: Option<String> },
    /// A menu was opened (from AXObserver).
    MenuOpened,
    /// A menu was closed (from AXObserver).
    MenuClosed,
    /// A sheet/dialog appeared (from AXObserver).
    SheetCreated,
    /// UI layout changed (from AXObserver).
    LayoutChanged,
    /// An element's title changed (from AXObserver).
    TitleChanged { new_title: Option<String> },
    /// An application was activated (brought to foreground).
    AppActivated { app_name: Option<String> },
    /// An application was deactivated (sent to background).
    AppDeactivated { app_name: Option<String> },
    /// A window was moved.
    WindowMoved,
    /// A window was resized.
    WindowResized,
    /// A window was minimized.
    WindowMinimized,
    /// A window was restored from minimized state.
    WindowRestored,
    /// Selection changed (text, list, etc.).
    SelectionChanged,
    /// The number of rows in a table/outline changed.
    RowCountChanged,
}
