use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum AccessibilityError {
    #[error("Accessibility API not available on this platform")]
    Unavailable,
    #[error("Failed to query accessibility tree: {0}")]
    QueryFailed(String),
    #[error("Element not found: {0}")]
    NotFound(String),
    #[error("Action failed: {0}")]
    OperationFailed(String),
}

/// Bounding rectangle in screen pixel coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bounds {
    #[serde(deserialize_with = "flexible_i32")]
    pub x: i32,
    #[serde(deserialize_with = "flexible_i32")]
    pub y: i32,
    #[serde(deserialize_with = "flexible_u32")]
    pub width: u32,
    #[serde(deserialize_with = "flexible_u32")]
    pub height: u32,
}

/// Bounds normalized to 0–1 relative to the containing monitor.
///
/// Aligns directly with full-monitor screenshots so vision and a11y
/// references share a coordinate space. Populated when the walker knows
/// the monitor geometry; `None` otherwise (callers fall back to `bounds`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NormalizedBounds {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

impl NormalizedBounds {
    /// Compute normalized bounds from pixel bounds + monitor geometry.
    /// Returns `None` if the monitor has zero area.
    pub fn from_pixels(
        b: &Bounds,
        monitor_x: f64,
        monitor_y: f64,
        monitor_w: f64,
        monitor_h: f64,
    ) -> Option<Self> {
        if monitor_w <= 0.0 || monitor_h <= 0.0 {
            return None;
        }
        Some(NormalizedBounds {
            left: ((b.x as f64 - monitor_x) / monitor_w) as f32,
            top: ((b.y as f64 - monitor_y) / monitor_h) as f32,
            width: (b.width as f64 / monitor_w) as f32,
            height: (b.height as f64 / monitor_h) as f32,
        })
    }
}

/// Why a tree walk stopped early (if it did). Feeds back into
/// [`crate::budget::AppWalkBudget`] for adaptive throttling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TruncationReason {
    /// Walk completed naturally — visited all reachable nodes.
    #[default]
    None,
    /// Hit the wall-clock timeout.
    Timeout,
    /// Hit the maximum node count.
    MaxNodes,
    /// Hit the maximum recursion depth.
    MaxDepth,
}

/// Why a window was skipped during a tree walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Incognito,
    ExcludedApp,
    UserIgnored,
    NotInIncludeList,
}

/// Deserialize a value as u32, accepting floats and rounding.
fn flexible_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_u64() {
                Ok(i as u32)
            } else if let Some(f) = n.as_f64() {
                Ok(f.round().max(0.0) as u32)
            } else {
                Err(serde::de::Error::custom("expected numeric value"))
            }
        }
        _ => Err(serde::de::Error::custom("expected number")),
    }
}

/// Deserialize a value as i32, accepting floats and rounding.
fn flexible_i32<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i as i32)
            } else if let Some(f) = n.as_f64() {
                Ok(f.round() as i32)
            } else {
                Err(serde::de::Error::custom("expected numeric value"))
            }
        }
        _ => Err(serde::de::Error::custom("expected number")),
    }
}

/// UI element role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum ElementRole {
    /// Used as a neutral default when the role is not yet known.
    #[default]
    #[serde(rename = "unknown")]
    Unknown,
    Window,
    Button,
    Input,
    Text,
    List,
    ListItem,
    Menu,
    MenuItem,
    Tab,
    TabItem,
    Table,
    TableRow,
    TableCell,
    Checkbox,
    RadioButton,
    ComboBox,
    Slider,
    ScrollBar,
    TreeView,
    TreeItem,
    Toolbar,
    StatusBar,
    Dialog,
    Group,
    Image,
    Link,
    Custom(String),
}

/// UI element state flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementState {
    pub focused: bool,
    pub enabled: bool,
    pub visible: bool,
    pub selected: bool,
    pub expanded: Option<bool>,
    pub checked: Option<bool>,
}

impl Default for ElementState {
    /// Default state: all false, expanded/checked = None.
    /// Used for elements from sources that don't provide state (e.g., vision).
    fn default() -> Self {
        Self {
            focused: false,
            enabled: false,
            visible: false,
            selected: false,
            expanded: None,
            checked: None,
        }
    }
}

impl ElementState {
    /// Default state when AT-SPI2 state query fails — assume visible and enabled.
    pub fn default_visible() -> Self {
        Self {
            focused: false,
            enabled: true,
            visible: true,
            selected: false,
            expanded: None,
            checked: None,
        }
    }
}

/// A single element in the accessibility tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccessibilityElement {
    /// Unique identifier within this tree snapshot.
    pub id: String,
    /// Element role (button, input, etc.).
    pub role: ElementRole,
    /// Human-readable label.
    pub label: Option<String>,
    /// Accessibility description (tooltip / secondary label).
    pub description: Option<String>,
    /// Current value (for inputs, sliders, etc.).
    pub value: Option<String>,
    /// Screen-space bounding rectangle in pixel coordinates.
    pub bounds: Option<Bounds>,
    /// Bounds normalized to 0–1 relative to the containing monitor.
    /// Aligns with full-monitor screenshots; populated when monitor geometry is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_bounds: Option<NormalizedBounds>,
    /// Current state flags.
    pub state: ElementState,
    /// ID of the parent element (None for root).
    pub parent_id: Option<String>,
    /// Available actions (from AT-SPI2 Action interface): "click", "press", "activate", etc.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,

    // --- Automation-grade properties, all Optional, filled per-platform ---
    /// Stable unique identifier for targeting. Windows: UIA AutomationId.
    /// macOS: AXIdentifier. Linux: AT-SPI object path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    /// Class/type info. Windows: Win32 ClassName. macOS: AXSubrole.
    /// Linux: AT-SPI attribute "class".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    /// Fine-grained role classification. macOS: AXSubrole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subrole: Option<String>,
    /// Human-readable role description. macOS: AXRoleDescription.
    /// Windows: LocalizedControlType.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_description: Option<String>,
    /// Placeholder text for input fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Tooltip or help text. Windows: UIA HelpText. macOS: AXHelp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help_text: Option<String>,
    /// Associated URL. macOS: AXURL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// True if this element is a password field — consumers should redact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_password: Option<bool>,
    /// Whether the element can receive keyboard focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_keyboard_focusable: Option<bool>,
    /// Keyboard shortcut. Windows: AcceleratorKey.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accelerator_key: Option<String>,
    /// Access-key mnemonic. Windows: AccessKey.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_key: Option<String>,

    /// Extended free-form properties (kept for backward compatibility).
    /// New typed fields above are preferred when the property is well-known.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub properties: HashMap<String, String>,
    /// Child elements.
    pub children: Vec<AccessibilityElement>,
}

/// An accessibility event pushed from the OS via AXObserver (macOS) or AT-SPI2 signals (Linux).
/// These replace polling-based change detection with real-time push notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AccessibilityEvent {
    /// Focus moved to a different element.
    FocusChanged { element_id: Option<String> },
    /// An element's value changed (input text, checkbox state, slider position).
    ValueChanged {
        element_id: String,
        new_value: Option<String>,
    },
    /// UI layout changed (elements added, removed, or repositioned).
    LayoutChanged,
    /// A new window was created.
    WindowCreated { title: Option<String> },
    /// A menu was opened.
    MenuOpened,
    /// A menu was closed.
    MenuClosed,
    /// A sheet/dialog appeared.
    SheetCreated,
    /// An element's title changed.
    TitleChanged {
        element_id: Option<String>,
        new_title: Option<String>,
    },
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
    /// Selection changed (text selection, list selection, etc.).
    SelectionChanged,
    /// The number of rows in a table/outline changed.
    RowCountChanged,
    /// An element was destroyed/removed from the tree.
    ElementDestroyed,
    /// The main window changed (different from focus change).
    MainWindowChanged,
    /// The application was hidden (⌘H).
    AppHidden { app_name: Option<String> },
    /// The application was shown (unhidden).
    AppShown { app_name: Option<String> },
    /// A VoiceOver/screen reader announcement was requested.
    AnnouncementRequested { text: Option<String> },
    /// A tooltip/help tag was shown.
    HelpTagShown,
}

/// Platform-agnostic accessibility tree provider.
pub trait AccessibilityTree: Send + Sync {
    /// Get the full accessibility tree for the focused window.
    fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError>;

    /// Find elements matching a query (by role, label, value).
    fn find_elements(
        &self,
        role: Option<&ElementRole>,
        label: Option<&str>,
    ) -> Result<Vec<AccessibilityElement>, AccessibilityError>;

    /// Get the currently focused element.
    fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError>;

    /// Start observing accessibility events (push-based via AXObserver / AT-SPI2 signals).
    /// Default: no-op (platforms without observer support).
    fn start_observing(&mut self) -> Result<(), AccessibilityError> {
        Ok(())
    }

    /// Drain accumulated accessibility events since last call.
    /// Default: empty (platforms without observer support).
    fn drain_events(&mut self) -> Vec<AccessibilityEvent> {
        vec![]
    }

    /// Stop observing accessibility events.
    /// Default: no-op.
    fn stop_observing(&mut self) {}

    /// Execute an action on an element by its ID (e.g., "click" on a button).
    /// Uses the native accessibility API (AXPerformAction on macOS) instead of mouse/keyboard.
    /// More reliable than coordinate-based input for buttons, checkboxes, menu items.
    /// Default: not supported.
    fn perform_action(&self, _element_id: &str, _action: &str) -> Result<bool, AccessibilityError> {
        Err(AccessibilityError::Unavailable)
    }

    /// Set a value directly on an element by ID (e.g., set text field content, checkbox state).
    /// Bypasses mouse/keyboard entirely — most reliable for form filling.
    /// Default: not supported.
    fn set_value(&self, _element_id: &str, _value: &str) -> Result<bool, AccessibilityError> {
        Err(AccessibilityError::Unavailable)
    }

    /// Check if an element's value can be set directly.
    /// Default: false.
    fn is_settable(&self, _element_id: &str) -> bool {
        false
    }

    /// Get the accessibility element at a screen coordinate (hit testing).
    /// Returns the element under the given (x, y) point, or None.
    /// Default: not supported.
    fn element_at_position(
        &self,
        _x: f32,
        _y: f32,
    ) -> Result<Option<AccessibilityElement>, AccessibilityError> {
        Ok(None)
    }

    /// Get the menu bar structure of the focused application.
    /// Returns a flat list of menu items with their hierarchy path.
    /// e.g., [{ label: "Save", path: "File > Save", shortcut: "⌘S" }, ...]
    /// This is the AI's "command palette" — all available app commands.
    /// Default: empty (platforms without menu bar access).
    fn get_menu_bar(&self) -> Result<Vec<MenuBarItem>, AccessibilityError> {
        Ok(vec![])
    }

    /// Get ALL windows of the focused application (not just the focused one).
    /// Returns each window as a tree root with minimal children.
    /// Default: returns only the focused window via get_tree().
    fn get_all_windows(&self) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
        self.get_tree().map(|t| vec![t])
    }
}

/// A menu bar item — represents a command discoverable from the app's menu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MenuBarItem {
    /// Menu path: "File > Save As..."
    pub path: String,
    /// The leaf menu item label: "Save As..."
    pub label: String,
    /// Keyboard shortcut if available: "⇧⌘S"
    pub shortcut: Option<String>,
    /// Whether the menu item is currently enabled.
    pub enabled: bool,
}

/// Stub implementation for unsupported platforms.
pub struct StubAccessibility;

impl AccessibilityTree for StubAccessibility {
    fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError> {
        tracing::warn!("Stub accessibility: returning empty tree");
        Ok(AccessibilityElement {
            id: "root".into(),
            role: ElementRole::Window,
            label: Some("Stub Window".into()),
            description: None,
            value: None,
            bounds: Some(Bounds {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }),
            state: ElementState {
                focused: true,
                enabled: true,
                visible: true,
                selected: false,
                expanded: None,
                checked: None,
            },
            parent_id: None,
            actions: vec![],
            properties: HashMap::new(),
            children: vec![],
            ..Default::default()
        })
    }

    fn find_elements(
        &self,
        _role: Option<&ElementRole>,
        _label: Option<&str>,
    ) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
        Ok(vec![])
    }

    fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError> {
        Ok(None)
    }
}
