//! Linux accessibility via AT-SPI2 over D-Bus using zbus.
//!
//! Uses zbus blocking API to query the accessibility tree via the
//! org.a11y.atspi.Accessible interface. Falls back to StubAccessibility
//! if the AT-SPI2 registry is not running.

use crate::tree::{
    AccessibilityElement, AccessibilityError, AccessibilityTree, Bounds, ElementRole, ElementState,
};
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

/// Maximum depth to recurse into the accessibility tree.
/// Prevents runaway recursion on deeply nested or circular trees.
const MAX_TREE_DEPTH: usize = 15;

/// Maximum total elements to collect before stopping.
/// Keeps tree snapshots bounded in size.
const MAX_ELEMENTS: usize = 500;

/// Default timeout for D-Bus method calls (prevents agent hangs if AT-SPI2 is slow).
const DBUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Linux accessibility provider using AT-SPI2 D-Bus interface via zbus.
pub struct LinuxAccessibility {
    conn: Connection,
    /// Per-app adaptive walk throttle.
    budget: std::sync::Mutex<crate::budget::AppWalkBudget>,
    /// Fuzzy-dedup cache — skips near-identical snapshots.
    snap_cache: std::sync::Mutex<crate::cache::SnapshotCache>,
}

impl LinuxAccessibility {
    /// Create a new Linux accessibility provider.
    /// Connects to the AT-SPI2 accessibility bus (not the session bus).
    /// The AT-SPI2 bus address is obtained via org.a11y.Bus on the session bus.
    pub fn new() -> Result<Self, AccessibilityError> {
        // First connect to session bus to find AT-SPI2 bus address
        let session_conn = Connection::session()
            .map_err(|e| AccessibilityError::QueryFailed(format!("D-Bus session: {}", e)))?;

        // Connect to the AT-SPI2 accessibility bus
        let conn = Self::connect_atspi_bus(&session_conn)?;

        // Verify AT-SPI2 registry is reachable on this bus
        let proxy = zbus::blocking::fdo::DBusProxy::new(&conn)
            .map_err(|e| AccessibilityError::QueryFailed(format!("D-Bus proxy: {}", e)))?;
        let names = proxy
            .list_names()
            .map_err(|e| AccessibilityError::QueryFailed(format!("List names: {}", e)))?;

        let has_atspi = names
            .iter()
            .any(|n| n.as_str() == "org.a11y.atspi.Registry");
        if !has_atspi {
            return Err(AccessibilityError::Unavailable);
        }

        Ok(Self {
            conn,
            budget: std::sync::Mutex::new(crate::budget::AppWalkBudget::new()),
            snap_cache: std::sync::Mutex::new(crate::cache::SnapshotCache::new()),
        })
    }

    /// Get the AT-SPI2 bus address from the session bus via org.a11y.Bus,
    /// then connect via UnixStream to avoid async runtime requirements.
    fn connect_atspi_bus(session_conn: &Connection) -> Result<Connection, AccessibilityError> {
        let proxy = zbus::blocking::Proxy::new(
            session_conn,
            "org.a11y.Bus",
            "/org/a11y/bus",
            "org.a11y.Bus",
        )
        .map_err(|e| AccessibilityError::QueryFailed(format!("a11y bus proxy: {}", e)))?;

        let address: String = proxy
            .call("GetAddress", &())
            .map_err(|e| AccessibilityError::QueryFailed(format!("GetAddress: {}", e)))?;

        if address.is_empty() {
            return Err(AccessibilityError::Unavailable);
        }

        // Parse unix:path=/some/path from the address string
        // Format: "unix:path=/path/to/socket,guid=..."
        let socket_path = address
            .split(',')
            .next()
            .and_then(|s| s.strip_prefix("unix:path="))
            .ok_or_else(|| {
                AccessibilityError::QueryFailed(format!(
                    "Cannot parse AT-SPI2 bus address: {}",
                    address
                ))
            })?;

        // tokio::net::UnixStream::from_std panics if called outside a Tokio
        // runtime (e.g. plain `cargo test` without #[tokio::test]). Treat that
        // as Unavailable so the caller falls back to StubAccessibility.
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(AccessibilityError::Unavailable);
        }

        let std_stream = UnixStream::connect(socket_path).map_err(|e| {
            AccessibilityError::QueryFailed(format!("Connect AT-SPI2 socket: {}", e))
        })?;
        std_stream
            .set_nonblocking(true)
            .map_err(|e| AccessibilityError::QueryFailed(format!("Set non-blocking: {}", e)))?;
        let stream = tokio::net::UnixStream::from_std(std_stream)
            .map_err(|e| AccessibilityError::QueryFailed(format!("Tokio UnixStream: {}", e)))?;

        zbus::blocking::connection::Builder::unix_stream(stream)
            .method_timeout(DBUS_TIMEOUT)
            .build()
            .map_err(|e| AccessibilityError::QueryFailed(format!("AT-SPI2 bus auth: {}", e)))
    }

    /// Query the Name property of an accessible object.
    fn get_name(&self, dest: &str, path: &str) -> Option<String> {
        let proxy =
            zbus::blocking::Proxy::new(&self.conn, dest, path, "org.a11y.atspi.Accessible").ok()?;
        let value: OwnedValue = proxy.get_property("Name").ok()?;
        let name: &str = value.downcast_ref().ok()?;
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }

    /// Query the Role of an accessible object via GetRoleName.
    fn get_role(&self, dest: &str, path: &str) -> ElementRole {
        let proxy =
            match zbus::blocking::Proxy::new(&self.conn, dest, path, "org.a11y.atspi.Accessible") {
                Ok(p) => p,
                Err(_) => return ElementRole::Custom("unknown".into()),
            };

        let result: Result<String, _> = proxy.call("GetRoleName", &());
        match result {
            Ok(role_name) => atspi_role_to_element_role(&role_name),
            Err(_) => ElementRole::Custom("unknown".into()),
        }
    }

    /// Query the bounding box via the Component interface's GetExtents method.
    fn get_bounds(&self, dest: &str, path: &str) -> Option<Bounds> {
        let proxy =
            zbus::blocking::Proxy::new(&self.conn, dest, path, "org.a11y.atspi.Component").ok()?;

        // GetExtents(coord_type: u32) -> (x, y, width, height)
        // coord_type 0 = screen coordinates
        let result: Result<(i32, i32, i32, i32), _> = proxy.call("GetExtents", &(0u32,));
        match result {
            Ok((x, y, w, h)) if w > 0 && h > 0 => Some(Bounds {
                x,
                y,
                width: w as u32,
                height: h as u32,
            }),
            _ => None,
        }
    }

    /// Get the Value property (for inputs, sliders, etc.).
    fn get_value(&self, dest: &str, path: &str) -> Option<String> {
        let proxy =
            zbus::blocking::Proxy::new(&self.conn, dest, path, "org.a11y.atspi.Value").ok()?;
        let value: OwnedValue = proxy.get_property("CurrentValue").ok()?;
        if let Ok(v) = value.downcast_ref::<f64>() {
            return Some(v.to_string());
        }
        None
    }

    /// Get the Description property (secondary label / tooltip).
    fn get_description(&self, dest: &str, path: &str) -> Option<String> {
        let proxy =
            zbus::blocking::Proxy::new(&self.conn, dest, path, "org.a11y.atspi.Accessible").ok()?;
        let value: OwnedValue = proxy.get_property("Description").ok()?;
        let desc: &str = value.downcast_ref().ok()?;
        if desc.is_empty() {
            None
        } else {
            Some(desc.to_string())
        }
    }

    /// Query the AT-SPI2 StateSet for an element.
    /// Returns an ElementState with real values from the accessibility API.
    ///
    /// AT-SPI2 GetState returns two u32 values that form a 64-bit state bitfield.
    /// State bits per AtspiStateType enum (atspi-constants.h):
    ///   0  = invalid,     1  = active,      2  = armed,       3  = busy
    ///   4  = checked,     5  = collapsed,   6  = defunct,     7  = editable
    ///   8  = enabled,     9  = expandable,  10 = expanded,    11 = focusable
    ///   12 = focused,     13 = has_tooltip,  ...
    ///   23 = selected,    24 = sensitive,   25 = showing,     ...
    ///   30 = visible,     31 = manages_descendants
    ///   41 = checkable    (in second u32, bit 9)
    fn get_state(&self, dest: &str, path: &str) -> ElementState {
        let proxy =
            match zbus::blocking::Proxy::new(&self.conn, dest, path, "org.a11y.atspi.Accessible") {
                Ok(p) => p,
                Err(_) => return ElementState::default_visible(),
            };

        // GetState returns (u32, u32) — two halves of a 64-bit bitfield
        let result: Result<Vec<u32>, _> = proxy.call("GetState", &());
        match result {
            Ok(state_vec) if state_vec.len() >= 2 => {
                let bits: u64 = (state_vec[0] as u64) | ((state_vec[1] as u64) << 32);
                ElementState {
                    focused: bits & (1 << 12) != 0,  // ATSPI_STATE_FOCUSED = 12
                    enabled: bits & (1 << 8) != 0,   // ATSPI_STATE_ENABLED = 8
                    visible: bits & (1 << 30) != 0,  // ATSPI_STATE_VISIBLE = 30
                    selected: bits & (1 << 23) != 0, // ATSPI_STATE_SELECTED = 23
                    expanded: if bits & (1 << 9) != 0 {
                        // ATSPI_STATE_EXPANDABLE = 9
                        Some(bits & (1 << 10) != 0) // ATSPI_STATE_EXPANDED = 10
                    } else {
                        None
                    },
                    checked: if bits & (1u64 << 41) != 0 {
                        // ATSPI_STATE_CHECKABLE = 41
                        Some(bits & (1 << 4) != 0) // ATSPI_STATE_CHECKED = 4
                    } else {
                        None
                    },
                }
            }
            Ok(state_vec) if state_vec.len() == 1 => {
                let bits: u64 = state_vec[0] as u64;
                ElementState {
                    focused: bits & (1 << 12) != 0,
                    enabled: bits & (1 << 8) != 0,
                    visible: bits & (1 << 30) != 0,
                    selected: bits & (1 << 23) != 0,
                    expanded: None,
                    checked: None,
                }
            }
            _ => {
                // Fallback: infer visible from bounds
                ElementState::default_visible()
            }
        }
    }

    /// Get text content from the Text interface.
    fn get_text(&self, dest: &str, path: &str) -> Option<String> {
        let proxy =
            zbus::blocking::Proxy::new(&self.conn, dest, path, "org.a11y.atspi.Text").ok()?;

        // GetText(start_offset, end_offset) — use 0, -1 for full text
        let result: Result<String, _> = proxy.call("GetText", &(0i32, -1i32));
        match result {
            Ok(text) if !text.is_empty() => Some(text),
            _ => None,
        }
    }

    /// Get the count of selected children via the Selection interface.
    /// Returns the number of selected children, or 0 if not a selection container.
    fn get_selected_count(&self, dest: &str, path: &str) -> usize {
        let proxy =
            match zbus::blocking::Proxy::new(&self.conn, dest, path, "org.a11y.atspi.Selection") {
                Ok(p) => p,
                Err(_) => return 0,
            };

        let result: Result<i32, _> = proxy.get_property("NSelectedChildren");
        match result {
            Ok(n) if n > 0 => n as usize,
            _ => 0,
        }
    }

    /// Get available actions from the Action interface.
    /// Returns action names like "click", "press", "activate", "expand or contract".
    fn get_actions(&self, dest: &str, path: &str) -> Vec<String> {
        let proxy =
            match zbus::blocking::Proxy::new(&self.conn, dest, path, "org.a11y.atspi.Action") {
                Ok(p) => p,
                Err(_) => return vec![],
            };

        // Get the number of actions
        let n_actions: Result<i32, _> = proxy.get_property("NActions");
        let count = match n_actions {
            Ok(n) if n > 0 => n as usize,
            _ => return vec![],
        };

        let mut actions = Vec::with_capacity(count.min(10));
        for i in 0..count.min(10) {
            let result: Result<String, _> = proxy.call("GetName", &(i as i32,));
            if let Ok(name) = result {
                if !name.is_empty() {
                    actions.push(name);
                }
            }
        }
        actions
    }

    /// Recursively search a subtree for the deepest element with STATE_FOCUSED (bit 12).
    /// Depth-limited to 4 levels and 50 elements to keep the search fast.
    fn find_focused_in_tree(
        &self,
        dest: &str,
        path: &str,
        depth: usize,
    ) -> Option<AccessibilityElement> {
        const MAX_FOCUSED_DEPTH: usize = 4;
        const MAX_FOCUSED_ELEMENTS: usize = 50;

        if depth > MAX_FOCUSED_DEPTH {
            return None;
        }

        // Check this element's state for STATE_FOCUSED
        let state = self.get_state(dest, path);
        let mut found_here = if state.focused {
            let role = self.get_role(dest, path);
            let label = self
                .get_name(dest, path)
                .or_else(|| self.get_description(dest, path));
            Some(AccessibilityElement {
                id: format!("focused-{}", depth),
                role,
                label,
                description: self.get_description(dest, path),
                value: self
                    .get_text(dest, path)
                    .or_else(|| self.get_value(dest, path)),
                bounds: self.get_bounds(dest, path),
                state: state.clone(),
                parent_id: None,
                actions: self.get_actions(dest, path),
                properties: HashMap::new(),
                children: vec![],
                ..Default::default()
            })
        } else {
            None
        };

        // Search children for a deeper focused element
        let child_refs = self.get_children(dest, path);
        for (i, (child_bus, child_path)) in child_refs.iter().enumerate() {
            if i >= MAX_FOCUSED_ELEMENTS {
                break;
            }
            let child_dest = if child_bus.is_empty() {
                dest
            } else {
                child_bus.as_str()
            };
            if let Some(deeper) = self.find_focused_in_tree(child_dest, child_path, depth + 1) {
                // Prefer the deepest focused element (most specific)
                found_here = Some(deeper);
            }
        }

        found_here
    }

    /// Get children of an accessible object as (bus_name, object_path) pairs.
    fn get_children(&self, dest: &str, path: &str) -> Vec<(String, String)> {
        let proxy =
            match zbus::blocking::Proxy::new(&self.conn, dest, path, "org.a11y.atspi.Accessible") {
                Ok(p) => p,
                Err(e) => {
                    tracing::trace!("get_children proxy failed for {} {}: {}", dest, path, e);
                    return vec![];
                }
            };

        // GetChildren returns array of (bus_name, object_path) D-Bus structs: a(so)
        let result: Result<Vec<(String, OwnedObjectPath)>, _> = proxy.call("GetChildren", &());
        match result {
            Ok(children) => children
                .into_iter()
                .map(|(bus, path)| (bus, path.as_str().to_string()))
                .collect(),
            Err(e) => {
                tracing::trace!("GetChildren failed for {} {}: {}", dest, path, e);
                vec![]
            }
        }
    }

    /// Recursively build an AccessibilityElement tree from a D-Bus accessible.
    #[allow(clippy::too_many_arguments)]
    fn build_element(
        &self,
        dest: &str,
        path: &str,
        id_prefix: &str,
        idx: usize,
        depth: usize,
        element_count: &mut usize,
        parent_id: Option<&str>,
        max_elements: usize,
    ) -> AccessibilityElement {
        let id = format!("{}-{}", id_prefix, idx);
        let role = self.get_role(dest, path);
        let bounds = self.get_bounds(dest, path);

        // Get label from Name, fall back to Description
        let name = self.get_name(dest, path);
        let description = self.get_description(dest, path);
        let label = name.or_else(|| description.clone());

        // Try to get value — first from Text interface, then Value interface
        let value = self
            .get_text(dest, path)
            .or_else(|| self.get_value(dest, path));

        // Get available actions (click, press, activate, etc.)
        let actions = self.get_actions(dest, path);

        // Query real AT-SPI2 states
        let mut state = self.get_state(dest, path);
        // If AT-SPI2 says not visible but we have bounds, trust bounds
        if !state.visible && bounds.is_some() {
            state.visible = true;
        }

        // For list/tree containers, check if this element has selected children
        // (enriches the selected state beyond just the state bitfield)
        if matches!(
            role,
            ElementRole::List | ElementRole::TreeView | ElementRole::Table
        ) {
            let n_selected = self.get_selected_count(dest, path);
            if n_selected > 0 {
                state.selected = true;
            }
        }

        *element_count += 1;

        // Recurse into children if within limits
        let children = if depth < MAX_TREE_DEPTH && *element_count < max_elements {
            let child_refs = self.get_children(dest, path);
            let mut children = Vec::new();
            for (ci, (child_bus, child_path)) in child_refs.iter().enumerate() {
                if *element_count >= max_elements {
                    break;
                }
                let child_dest = if child_bus.is_empty() {
                    dest
                } else {
                    child_bus.as_str()
                };
                children.push(self.build_element(
                    child_dest,
                    child_path,
                    &id,
                    ci,
                    depth + 1,
                    element_count,
                    Some(&id),
                    max_elements,
                ));
            }
            children
        } else {
            vec![]
        };

        AccessibilityElement {
            id,
            role,
            label,
            description,
            value,
            bounds,
            state,
            parent_id: parent_id.map(|s| s.to_string()),
            actions,
            properties: HashMap::new(),
            children,
            ..Default::default()
        }
    }
}

/// Collect all label/value text from a tree for SimHash computation.
fn collect_element_text(elem: &AccessibilityElement) -> String {
    let mut buf = String::new();
    fn recurse(e: &AccessibilityElement, b: &mut String) {
        if let Some(ref l) = e.label {
            b.push_str(l);
            b.push(' ');
        }
        if let Some(ref v) = e.value {
            b.push_str(v);
            b.push(' ');
        }
        for child in &e.children {
            recurse(child, b);
        }
    }
    recurse(elem, &mut buf);
    buf
}

impl AccessibilityTree for LinuxAccessibility {
    fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError> {
        let children_refs =
            self.get_children("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root");

        tracing::debug!("AT-SPI2 registry children: {}", children_refs.len());

        let walk_start = std::time::Instant::now();
        let mut element_count: usize = 0;
        let mut children = Vec::new();
        let app_root_path = "/org/a11y/atspi/accessible/root";

        for (idx, (bus_name, _obj_path)) in children_refs.iter().enumerate() {
            // Derive app name for budget/cache keying
            let app_name = self
                .get_name(bus_name, app_root_path)
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| bus_name.clone());

            // Incognito check: skip private browser windows
            if let Some(title) = self.get_name(bus_name, app_root_path) {
                if crate::incognito::is_title_private(&title) {
                    tracing::debug!("Skipping private window: {:?}", title);
                    continue;
                }
            }

            // Per-app budget check
            let decision = self
                .budget
                .lock()
                .map(|mut b| b.should_walk(&app_name))
                .unwrap_or(crate::budget::WalkDecision {
                    walk: true,
                    max_nodes: MAX_ELEMENTS,
                    timeout: std::time::Duration::from_secs(5),
                    tier: crate::budget::WalkTier::Light,
                });

            if element_count >= decision.max_nodes {
                break;
            }

            let mut app_element = self.build_element(
                bus_name,
                app_root_path,
                "app",
                idx,
                1,
                &mut element_count,
                Some("root"),
                decision.max_nodes,
            );

            // Override role to Window for top-level apps
            app_element.role = ElementRole::Window;

            children.push(app_element);
        }

        let walk_duration = walk_start.elapsed();
        let truncated = element_count >= MAX_ELEMENTS;

        // Record walk duration for all apps (keyed as "linux-atspi2")
        if let Ok(mut b) = self.budget.lock() {
            b.record_walk("linux-atspi2", walk_duration, truncated);
        }

        let root = make_root_element(children);

        // SimHash dedup — log near-identical snapshots
        let text = collect_element_text(&root);
        let hash = crate::simhash::simhash(&text);
        let window_title = root.label.clone().unwrap_or_default();
        if let Ok(mut sc) = self.snap_cache.lock() {
            if sc.should_store("linux-atspi2", &window_title, hash) {
                sc.record("linux-atspi2", &window_title, hash);
            } else {
                tracing::debug!("AT-SPI2 snapshot dedup: near-identical content");
            }
        }

        Ok(root)
    }

    fn find_elements(
        &self,
        role: Option<&ElementRole>,
        label: Option<&str>,
    ) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
        let tree = self.get_tree()?;
        let mut results = Vec::new();
        collect_matching(&tree, role, label, &mut results);
        Ok(results)
    }

    fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError> {
        // Recursively search the accessibility tree for the element with STATE_FOCUSED.
        // Strategy:
        // 1. Iterate registry children (apps)
        // 2. For each app, do a shallow depth-limited search for STATE_FOCUSED (bit 12)
        // 3. Return the deepest focused element found (most specific)
        // 4. Fall back to first app with a non-empty name if no STATE_FOCUSED found
        let children_refs =
            self.get_children("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root");

        let mut fallback_app: Option<(String, String)> = None; // (bus_name, app_name)

        for (bus_name, _obj_path) in &children_refs {
            let app_path = "/org/a11y/atspi/accessible/root";

            // Remember first app with a name as fallback
            if fallback_app.is_none() {
                if let Some(name) = self.get_name(bus_name, app_path) {
                    if !name.is_empty() {
                        fallback_app = Some((bus_name.clone(), name));
                    }
                }
            }

            // Recursively search this app's tree for STATE_FOCUSED (depth-limited)
            if let Some(focused) = self.find_focused_in_tree(bus_name, app_path, 0) {
                return Ok(Some(focused));
            }
        }

        // Fallback: return first app with a name (old behavior)
        if let Some((bus_name, app_name)) = fallback_app {
            return Ok(Some(AccessibilityElement {
                id: "focused".into(),
                role: ElementRole::Window,
                label: Some(app_name),
                description: None,
                value: None,
                bounds: self.get_bounds(&bus_name, "/org/a11y/atspi/accessible/root"),
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
            }));
        }

        Ok(None)
    }
}

/// Map AT-SPI2 role name strings to our ElementRole enum.
fn atspi_role_to_element_role(role_name: &str) -> ElementRole {
    match role_name {
        "push button" | "toggle button" => ElementRole::Button,
        "text" | "paragraph" | "heading" | "label" | "caption" | "static" => ElementRole::Text,
        "entry" | "password text" | "spin button" | "date editor" => ElementRole::Input,
        "frame" | "window" | "dialog" => ElementRole::Window,
        "list" => ElementRole::List,
        "list item" => ElementRole::ListItem,
        "menu" | "menu bar" | "popup menu" => ElementRole::Menu,
        "menu item" | "check menu item" | "radio menu item" | "tearoff menu item" => {
            ElementRole::MenuItem
        }
        "page tab list" => ElementRole::Tab,
        "page tab" => ElementRole::TabItem,
        "table" | "tree table" => ElementRole::Table,
        "table row" | "table row header" => ElementRole::TableRow,
        "table cell" | "table column header" => ElementRole::TableCell,
        "check box" => ElementRole::Checkbox,
        "radio button" => ElementRole::RadioButton,
        "combo box" => ElementRole::ComboBox,
        "slider" => ElementRole::Slider,
        "scroll bar" => ElementRole::ScrollBar,
        "tree" => ElementRole::TreeView,
        "tree item" => ElementRole::TreeItem,
        "tool bar" => ElementRole::Toolbar,
        "status bar" => ElementRole::StatusBar,
        "alert" | "file chooser" | "color chooser" | "font chooser" => ElementRole::Dialog,
        "panel" | "filler" | "section" | "form" | "block quote" | "redundant object"
        | "application" | "autocomplete" | "embedded" | "grouping" => ElementRole::Group,
        "image" | "icon" | "animation" | "canvas" => ElementRole::Image,
        "link" => ElementRole::Link,
        other => ElementRole::Custom(other.to_string()),
    }
}

/// Build the root element with children.
fn make_root_element(children: Vec<AccessibilityElement>) -> AccessibilityElement {
    AccessibilityElement {
        id: "root".into(),
        role: ElementRole::Window,
        label: Some("Desktop".into()),
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
        children,
        ..Default::default()
    }
}

/// Recursively collect elements matching role and/or label.
fn collect_matching(
    node: &AccessibilityElement,
    role: Option<&ElementRole>,
    label: Option<&str>,
    out: &mut Vec<AccessibilityElement>,
) {
    let role_matches =
        role.is_none_or(|r| std::mem::discriminant(&node.role) == std::mem::discriminant(r));
    let label_matches =
        label.is_none_or(|l| node.label.as_deref().is_some_and(|nl| nl.contains(l)));

    if role_matches && label_matches {
        out.push(node.clone());
    }

    for child in &node.children {
        collect_matching(child, role, label, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_root_element() {
        let root = make_root_element(vec![]);
        assert_eq!(root.id, "root");
        assert!(root.children.is_empty());
    }

    #[test]
    fn test_make_root_with_children() {
        let child = AccessibilityElement {
            id: "app-0".into(),
            role: ElementRole::Window,
            label: Some("Firefox".into()),
            description: None,
            value: None,
            bounds: None,
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
        };
        let root = make_root_element(vec![child]);
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].label.as_deref(), Some("Firefox"));
    }

    #[test]
    fn test_collect_matching_by_role() {
        let tree = AccessibilityElement {
            id: "root".into(),
            role: ElementRole::Window,
            label: Some("Root".into()),
            description: None,
            value: None,
            bounds: None,
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
            children: vec![
                AccessibilityElement {
                    id: "btn-1".into(),
                    role: ElementRole::Button,
                    label: Some("OK".into()),
                    description: None,
                    value: None,
                    bounds: None,
                    state: ElementState {
                        focused: false,
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
                },
                AccessibilityElement {
                    id: "txt-1".into(),
                    role: ElementRole::Text,
                    label: Some("Hello".into()),
                    description: None,
                    value: None,
                    bounds: None,
                    state: ElementState {
                        focused: false,
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
                },
            ],
            ..Default::default()
        };

        let mut results = Vec::new();
        collect_matching(&tree, Some(&ElementRole::Button), None, &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "btn-1");
    }

    #[test]
    fn test_atspi_role_mapping() {
        assert!(matches!(
            atspi_role_to_element_role("push button"),
            ElementRole::Button
        ));
        assert!(matches!(
            atspi_role_to_element_role("entry"),
            ElementRole::Input
        ));
        assert!(matches!(
            atspi_role_to_element_role("check box"),
            ElementRole::Checkbox
        ));
        assert!(matches!(
            atspi_role_to_element_role("combo box"),
            ElementRole::ComboBox
        ));
        assert!(matches!(
            atspi_role_to_element_role("menu bar"),
            ElementRole::Menu
        ));
        assert!(matches!(
            atspi_role_to_element_role("page tab"),
            ElementRole::TabItem
        ));
        assert!(matches!(
            atspi_role_to_element_role("table"),
            ElementRole::Table
        ));
        assert!(matches!(
            atspi_role_to_element_role("tree"),
            ElementRole::TreeView
        ));
        assert!(matches!(
            atspi_role_to_element_role("link"),
            ElementRole::Link
        ));
        assert!(matches!(
            atspi_role_to_element_role("slider"),
            ElementRole::Slider
        ));
        assert!(matches!(
            atspi_role_to_element_role("panel"),
            ElementRole::Group
        ));
        assert!(matches!(
            atspi_role_to_element_role("image"),
            ElementRole::Image
        ));
        // Unknown roles become Custom
        assert!(matches!(
            atspi_role_to_element_role("weird-widget"),
            ElementRole::Custom(_)
        ));
    }

    #[test]
    fn test_collect_matching_by_label() {
        let tree = make_root_element(vec![AccessibilityElement {
            id: "btn-1".into(),
            role: ElementRole::Button,
            label: Some("Submit Form".into()),
            description: None,
            value: None,
            bounds: None,
            state: ElementState {
                focused: false,
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
        }]);

        let mut results = Vec::new();
        collect_matching(&tree, None, Some("Submit"), &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "btn-1");
    }
}
