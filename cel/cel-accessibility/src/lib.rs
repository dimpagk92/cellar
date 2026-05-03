//! CEL Accessibility Layer
//!
//! Bridges platform accessibility APIs into a unified element tree.
//! - Windows: UI Automation (requires `uiautomation` crate — added when targeting Windows)
//! - macOS: AXUIElement (requires `objc2` + `core-foundation` — added when targeting macOS)
//! - Linux: AT-SPI2 via D-Bus
//!
//! The tree types and trait are platform-agnostic. Platform implementations
//! are selected at compile time based on the target OS.
//!
//! Supporting modules (all platform-agnostic):
//! - [`budget`] — adaptive per-app walk throttling
//! - [`simhash`] — locality-sensitive hashing for dedup
//! - [`cache`] — fuzzy-dedup snapshot cache
//! - [`incognito`] — private-browsing detection

pub mod budget;
pub mod cache;
pub mod incognito;
pub mod simhash;

mod tree;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

pub use tree::{
    AccessibilityElement, AccessibilityError, AccessibilityEvent, AccessibilityTree, Bounds,
    ElementRole, ElementState, MenuBarItem, NormalizedBounds, SkipReason, StubAccessibility,
    TruncationReason,
};

/// Check whether the host process has macOS Accessibility permission granted.
///
/// On macOS this calls `AXIsProcessTrusted()` (cheap, no UI prompt). On every
/// other platform it returns `true` — Accessibility permission is a macOS-only
/// concept; AT-SPI2 on Linux and UIA on Windows have different permission models.
#[cfg(target_os = "macos")]
pub fn ax_is_process_trusted() -> bool {
    macos::is_process_trusted()
}

#[cfg(not(target_os = "macos"))]
pub fn ax_is_process_trusted() -> bool {
    true
}

/// Trigger the macOS Accessibility permission prompt for the host process.
///
/// Returns the trust state. The side effect is a system notification for
/// processes not yet in the Privacy & Security list — clicking it opens
/// System Settings with the host process pre-selected. After the user
/// toggles the permission on, the host process must restart for macOS to
/// pick up the change.
///
/// On non-macOS platforms this is a no-op that returns true.
#[cfg(target_os = "macos")]
pub fn ax_request_process_trust() -> bool {
    macos::request_process_trust()
}

#[cfg(not(target_os = "macos"))]
pub fn ax_request_process_trust() -> bool {
    true
}

/// Create a platform-appropriate accessibility tree provider.
pub fn create_tree() -> Box<dyn AccessibilityTree> {
    #[cfg(target_os = "linux")]
    {
        match linux::LinuxAccessibility::new() {
            Ok(provider) => return Box::new(provider),
            Err(e) => {
                tracing::warn!("AT-SPI2 not available, falling back to stub: {}", e);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        match macos::MacAccessibility::new() {
            Ok(provider) => return Box::new(provider),
            Err(e) => {
                tracing::warn!("AXUIElement not available, falling back to stub: {}", e);
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        match windows::WindowsAccessibility::new() {
            Ok(provider) => return Box::new(provider),
            Err(e) => {
                tracing::warn!("UIA not available, falling back to stub: {}", e);
            }
        }
    }
    // Fallback for all platforms where native a11y isn't available
    Box::new(StubAccessibility)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stub_get_tree() {
        let stub = StubAccessibility;
        let tree = stub.get_tree().unwrap();
        assert_eq!(tree.id, "root");
        assert!(matches!(tree.role, ElementRole::Window));
        assert_eq!(tree.label.as_deref(), Some("Stub Window"));
        assert!(tree.state.focused);
        assert!(tree.state.enabled);
        assert!(tree.state.visible);
        assert!(tree.children.is_empty());
    }

    #[test]
    fn test_stub_find_elements_returns_empty() {
        let stub = StubAccessibility;
        let results = stub
            .find_elements(Some(&ElementRole::Button), None)
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_stub_focused_element_returns_none() {
        let stub = StubAccessibility;
        let focused = stub.focused_element().unwrap();
        assert!(focused.is_none());
    }

    #[test]
    fn test_create_tree_returns_working_instance() {
        let tree = create_tree();
        let root = tree.get_tree().unwrap();
        // On Linux with AT-SPI2 or stub, root should exist
        assert!(!root.id.is_empty());
    }

    #[test]
    fn test_element_role_all_variants() {
        let roles = vec![
            ElementRole::Window,
            ElementRole::Button,
            ElementRole::Input,
            ElementRole::Text,
            ElementRole::List,
            ElementRole::ListItem,
            ElementRole::Menu,
            ElementRole::MenuItem,
            ElementRole::Tab,
            ElementRole::TabItem,
            ElementRole::Table,
            ElementRole::TableRow,
            ElementRole::TableCell,
            ElementRole::Checkbox,
            ElementRole::RadioButton,
            ElementRole::ComboBox,
            ElementRole::Slider,
            ElementRole::ScrollBar,
            ElementRole::TreeView,
            ElementRole::TreeItem,
            ElementRole::Toolbar,
            ElementRole::StatusBar,
            ElementRole::Dialog,
            ElementRole::Group,
            ElementRole::Image,
            ElementRole::Link,
            ElementRole::Custom("custom".into()),
        ];
        assert_eq!(roles.len(), 27);
        for role in &roles {
            let json = serde_json::to_string(role).unwrap();
            let _back: ElementRole = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn test_element_state_defaults() {
        let state = ElementState {
            focused: false,
            enabled: true,
            visible: true,
            selected: false,
            expanded: None,
            checked: None,
        };
        assert!(!state.focused);
        assert!(state.enabled);
        assert!(state.expanded.is_none());
    }

    #[test]
    fn test_element_state_serialization() {
        let state = ElementState {
            focused: true,
            enabled: true,
            visible: true,
            selected: false,
            expanded: Some(true),
            checked: Some(false),
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: ElementState = serde_json::from_str(&json).unwrap();
        assert!(back.focused);
        assert_eq!(back.expanded, Some(true));
        assert_eq!(back.checked, Some(false));
    }

    #[test]
    fn test_bounds_serialization() {
        let bounds = Bounds {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        };
        let json = serde_json::to_string(&bounds).unwrap();
        let back: Bounds = serde_json::from_str(&json).unwrap();
        assert_eq!(back.x, 10);
        assert_eq!(back.y, 20);
        assert_eq!(back.width, 100);
        assert_eq!(back.height, 50);
    }

    #[test]
    fn test_accessibility_element_with_children() {
        let child = AccessibilityElement {
            id: "child-1".into(),
            role: ElementRole::Button,
            label: Some("OK".into()),
            value: None,
            bounds: Some(Bounds {
                x: 100,
                y: 200,
                width: 80,
                height: 30,
            }),
            state: ElementState {
                focused: false,
                enabled: true,
                visible: true,
                selected: false,
                expanded: None,
                checked: None,
            },
            description: None,
            parent_id: None,
            actions: vec![],
            properties: std::collections::HashMap::new(),
            children: vec![],
            ..Default::default()
        };
        let parent = AccessibilityElement {
            id: "parent".into(),
            role: ElementRole::Dialog,
            label: Some("Confirm".into()),
            value: None,
            bounds: Some(Bounds {
                x: 50,
                y: 50,
                width: 300,
                height: 200,
            }),
            state: ElementState {
                focused: true,
                enabled: true,
                visible: true,
                selected: false,
                expanded: None,
                checked: None,
            },
            description: None,
            parent_id: None,
            actions: vec![],
            properties: std::collections::HashMap::new(),
            children: vec![child],
            ..Default::default()
        };
        assert_eq!(parent.children.len(), 1);
        assert_eq!(parent.children[0].id, "child-1");
        assert_eq!(parent.children[0].label.as_deref(), Some("OK"));
    }

    #[test]
    fn test_accessibility_element_serialization_roundtrip() {
        let elem = AccessibilityElement {
            id: "test-elem".into(),
            role: ElementRole::Input,
            label: Some("Username".into()),
            value: Some("admin".into()),
            bounds: Some(Bounds {
                x: 0,
                y: 0,
                width: 200,
                height: 30,
            }),
            state: ElementState {
                focused: true,
                enabled: true,
                visible: true,
                selected: false,
                expanded: None,
                checked: None,
            },
            description: None,
            parent_id: None,
            actions: vec![],
            properties: std::collections::HashMap::new(),
            children: vec![],
            ..Default::default()
        };
        let json = serde_json::to_string(&elem).unwrap();
        let back: AccessibilityElement = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "test-elem");
        assert_eq!(back.value.as_deref(), Some("admin"));
    }

    #[test]
    fn test_accessibility_error_display() {
        assert_eq!(
            AccessibilityError::Unavailable.to_string(),
            "Accessibility API not available on this platform"
        );
        assert_eq!(
            AccessibilityError::QueryFailed("timeout".into()).to_string(),
            "Failed to query accessibility tree: timeout"
        );
        assert_eq!(
            AccessibilityError::NotFound("btn-1".into()).to_string(),
            "Element not found: btn-1"
        );
    }
}
