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
pub mod window;

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
pub use window::{DockOp, DockResult, MenuExtraOp, MenuExtraResult, WindowGeom, WindowOp};

/// Apply a window-management operation (WS2). On macOS this drives the AX
/// window attributes (position/size/minimized/zoom/raise) and reads geometry
/// back; other platforms return an error.
#[cfg(target_os = "macos")]
pub use macos::{get_window_geom, perform_dock_op, perform_menu_extra_op, perform_window_op};

#[cfg(not(target_os = "macos"))]
pub fn perform_window_op(
    _app: Option<&str>,
    _window_index: usize,
    _op: &window::WindowOp,
) -> Result<window::WindowGeom, AccessibilityError> {
    Err(AccessibilityError::OperationFailed(
        "window management is macOS-only".into(),
    ))
}

/// Read-only window geometry query (WS4). macOS-only; other platforms error.
#[cfg(not(target_os = "macos"))]
pub fn get_window_geom(
    _app: Option<&str>,
    _window_index: usize,
) -> Result<window::WindowGeom, AccessibilityError> {
    Err(AccessibilityError::OperationFailed(
        "window management is macOS-only".into(),
    ))
}

/// Dock control (WS6). macOS-only; other platforms error.
#[cfg(not(target_os = "macos"))]
pub fn perform_dock_op(_op: &window::DockOp) -> Result<window::DockResult, AccessibilityError> {
    Err(AccessibilityError::OperationFailed(
        "dock control is macOS-only".into(),
    ))
}

/// Menu-bar extras (WS7). macOS-only; other platforms error.
#[cfg(not(target_os = "macos"))]
pub fn perform_menu_extra_op(
    _op: &window::MenuExtraOp,
) -> Result<window::MenuExtraResult, AccessibilityError> {
    Err(AccessibilityError::OperationFailed(
        "menu-bar extras are macOS-only".into(),
    ))
}

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

/// Process-startup pre-flight for the macOS Accessibility permission.
///
/// Call this from a binary's `main()` (or equivalent boot path) so the
/// first AX-requiring tool call doesn't surprise the user with a stream
/// of `Accessibility tree unavailable` warnings + silent degradation.
///
/// Returns the trust state. **Never aborts the process** — accessibility
/// is recoverable per-call (browser-only / CDP goals work without it),
/// and forcing a startup abort would punish workloads that don't need AX.
///
/// `interactive`:
/// - `true`  — When denied, also call [`ax_request_process_trust`], which
///   triggers the macOS system notification. The user can click it to
///   jump straight into Settings with the binary pre-selected. Right for
///   user-facing binaries (CLI, GUI app).
/// - `false` — When denied, only log a clear WARN with grant instructions.
///   Right for daemons / CI runners / headless services where a system
///   notification would be unanswered noise.
///
/// On non-macOS platforms this is a no-op that returns `true`.
///
/// macOS Accessibility quirks worth knowing:
/// - The grant is per-binary-identity. Signed releases use the
///   code-signing fingerprint; unsigned dev builds use the path +
///   checksum — so a `cargo build` rebuild typically requires
///   re-granting. The notification rate-limits itself, so calling this
///   on every cold start is safe.
/// - macOS does NOT pick up a grant mid-process. The process must
///   restart after the user toggles the permission on.
/// - Once the binary appears in the Accessibility list (even toggled
///   off), the prompt notification will not re-fire — the user has to
///   open Settings themselves.
pub fn ensure_trust_or_log(interactive: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        if macos::is_process_trusted() {
            tracing::debug!("Accessibility permission granted");
            return true;
        }
        if interactive {
            // Side effect: posts the macOS system notification. Return
            // value is the current trust state (still false the first
            // time — the user hasn't clicked through yet).
            macos::request_process_trust();
            tracing::warn!(
                "Accessibility permission not granted. A macOS notification \
                 should now be visible — click it to open System Settings → \
                 Privacy & Security → Accessibility, add this binary, and \
                 toggle it on. macOS does not pick up the grant mid-process; \
                 restart after enabling. Goals that only need CDP / browser \
                 perception will continue to work without it."
            );
        } else {
            tracing::warn!(
                "Accessibility permission not granted. To enable AX-dependent \
                 features, open System Settings → Privacy & Security → \
                 Accessibility, add this binary, and toggle it on; then \
                 restart the process. Browser-only / CDP goals continue to \
                 work without it."
            );
        }
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = interactive;
        true
    }
}

/// Create a platform-appropriate accessibility tree provider.
pub fn create_tree() -> Box<dyn AccessibilityTree> {
    #[cfg(target_os = "linux")]
    {
        // `LinuxAccessibility::new` calls `zbus::blocking::Connection::session()`
        // which internally spawns a tokio runtime and `block_on`s it. When
        // cel-eval (or any other tokio-main caller) constructs the AX tree
        // from inside an async context, that internal block_on panics with
        // "Cannot start a runtime from within a runtime". Run the
        // initialization on a fresh OS thread that has no ambient runtime
        // so zbus can spin up its own without conflict.
        //
        // Also `catch_unwind`-style: if the thread itself panics (network
        // issue, missing D-Bus socket, etc.) we fall back to the stub the
        // same way as a regular Err return — headless servers without
        // AT-SPI just get an empty AX tree, which is fine for browser-only
        // CDP goals.
        let init = std::thread::spawn(linux::LinuxAccessibility::new).join();
        match init {
            Ok(Ok(provider)) => return Box::new(provider),
            Ok(Err(e)) => {
                tracing::warn!("AT-SPI2 not available, falling back to stub: {}", e);
            }
            Err(panic_payload) => {
                let msg = panic_payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| {
                        panic_payload
                            .downcast_ref::<&str>()
                            .map(|s| (*s).to_string())
                    })
                    .unwrap_or_else(|| "(non-string panic payload)".into());
                tracing::warn!("AT-SPI2 init panicked, falling back to stub: {}", msg);
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
    fn ensure_trust_or_log_returns_actual_trust_state_without_panicking() {
        // Contract: the helper must NEVER abort the process, regardless of
        // platform or trust state. It's called from binary main()s where a
        // panic on first call would block users with browser-only goals
        // who don't need AX at all.
        //
        // On non-macOS the return must be true (no-op).
        // On macOS the return must match `ax_is_process_trusted()` — we
        // can't assert a specific value because that depends on whether
        // the test harness binary was granted permission in the dev
        // environment, but the helper must not lie.
        let interactive = ensure_trust_or_log(true);
        let headless = ensure_trust_or_log(false);
        // Both modes must observe the same trust state.
        assert_eq!(
            interactive, headless,
            "ensure_trust_or_log must report the same trust state regardless of `interactive` — \
             the flag only changes the side effect (prompt vs log), not the truth."
        );
        // And that trust state must match the underlying ax_is_process_trusted.
        assert_eq!(
            interactive,
            ax_is_process_trusted(),
            "ensure_trust_or_log must agree with ax_is_process_trusted; \
             one returning true while the other returns false would mean a binary thinks it's \
             granted when it isn't (or vice versa)."
        );
    }

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
        // WK5: this test was flaky on non-interactive environments
        // (CI runners, headless test sandboxes, no frontmost app).
        // `create_tree()` succeeds on macOS by returning a
        // `MacAccessibility` provider, but the subsequent `get_tree()`
        // call queries the focused-window AX subtree — which legitimately
        // fails with `QueryFailed("Failed to build tree from focused
        // window")` when the OS has no focused window for the test
        // process to introspect.
        //
        // The test contract is "create_tree returns a working trait
        // object that can be dispatched against without panicking" —
        // not "the OS has an accessible focused window right now."
        // Platform-specific provider tests (under `macos::tests`,
        // `linux::tests`, etc.) cover behaviour-with-real-window
        // separately. Here we only require that get_tree() returns a
        // proper `Result` (success or a recognisable error), not that
        // the host environment guarantees focus.
        let tree = create_tree();
        match tree.get_tree() {
            Ok(root) => {
                // Interactive env: assert the root looks well-formed.
                assert!(!root.id.is_empty(), "expected non-empty root id");
            }
            Err(AccessibilityError::QueryFailed(_)) => {
                // Non-interactive env (no focused window). Acceptable;
                // the dispatch contract held — we got a typed Err,
                // not a panic.
            }
            Err(other) => {
                panic!(
                    "create_tree dispatched but get_tree returned an \
                     unexpected error class: {other:?}"
                );
            }
        }
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
