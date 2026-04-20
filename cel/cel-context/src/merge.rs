#[allow(unused_imports)]
use crate::element::{Bounds, ContentRole, ContextElement, ContextSource, ScreenContext};
use cel_accessibility::{AccessibilityElement, AccessibilityTree, ElementRole};
use cel_display::ScreenCapture;
use cel_network::{NetworkEvent, NetworkMonitor};
use cel_signals::SignalBus;
use cel_vision::VisionProvider;
use std::time::{Duration, Instant};

/// Minimum number of actionable elements (buttons, inputs, links, etc.)
/// below which the vision fallback is triggered.
/// Set to 5 to avoid unnecessary vision calls on simple dialogs (OK/Cancel = 2 buttons).
const VISION_FALLBACK_THRESHOLD: usize = 5;
const GENERIC_ACTION_LABELS: &[&str] = &[
    "open", "close", "cancel", "ok", "more", "menu", "next", "back",
    "learn more", "details", "view", "edit", "delete", "remove", "select",
    "continue", "submit", "save", "apply", "retry", "dismiss",
];
const CHROME_NOISE_HINTS: &[&str] = &[
    "header", "nav", "navbar", "toolbar", "menu", "sidebar", "breadcrumb",
    "footer", "legal", "cookie", "consent", "account", "profile", "help",
    "support", "social", "share", "newsletter", "chat", "intercom",
];

fn frontmost_app_from_signals(
    snapshot: &cel_signals::SignalSnapshot,
) -> Option<String> {
    snapshot
        .running_apps
        .iter()
        .find(|app| app.is_frontmost && !app.name.is_empty())
        .map(|app| app.name.clone())
        .or_else(|| {
            snapshot
                .window_list
                .iter()
                .find(|window| window.is_on_screen && !window.app_name.is_empty())
                .map(|window| window.app_name.clone())
        })
}

fn frontmost_window_from_signals(
    snapshot: &cel_signals::SignalSnapshot,
    app_name: Option<&str>,
) -> Option<String> {
    if let Some(app_name) = app_name {
        snapshot
            .window_list
            .iter()
            .find(|window| {
                window.is_on_screen
                    && window.app_name == app_name
                    && !window.title.is_empty()
            })
            .map(|window| window.title.clone())
            .or_else(|| {
                snapshot
                    .window_list
                    .iter()
                    .find(|window| window.is_on_screen && window.app_name == app_name)
                    .map(|window| window.title.clone())
            })
    } else {
        snapshot
            .window_list
            .iter()
            .find(|window| window.is_on_screen && !window.title.is_empty())
            .map(|window| window.title.clone())
    }
}

/// High-level status for the streams currently wired into a ContextMerger.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StreamStatus {
    pub accessibility: bool,
    pub display: bool,
    pub network: bool,
    pub signals: bool,
    pub vision: bool,
    pub audio_capture: bool,
}

/// Merges context from all available streams into a unified ScreenContext.
pub struct ContextMerger {
    accessibility: Box<dyn AccessibilityTree>,
    display: Option<Box<dyn ScreenCapture>>,
    network: Option<Box<dyn NetworkMonitor>>,
    vision: Option<Box<dyn VisionProvider>>,
    signals: Option<Box<dyn SignalBus>>,
    /// Recent network events, carried across calls for context.
    recent_network: Vec<NetworkEvent>,
    /// Tokio runtime handle for running async vision calls from sync context.
    runtime: Option<tokio::runtime::Handle>,
    /// Cached context from the last `get_context()` call, with its creation timestamp.
    /// Only the expensive accessibility tree extraction is cached; signal snapshots
    /// are always refreshed.
    last_context: Option<(Instant, ScreenContext)>,
    /// Time-to-live for the context cache. Defaults to 500ms.
    context_cache_ttl: Duration,
    /// Minimum actionable elements before triggering vision fallback. Defaults to 5.
    vision_threshold: usize,
    /// Whether the network monitor successfully started.
    network_started: bool,
}

impl ContextMerger {
    pub fn new(accessibility: Box<dyn AccessibilityTree>) -> Self {
        Self {
            accessibility,
            display: None,
            network: None,
            vision: None,
            signals: None,
            recent_network: Vec::new(),
            runtime: tokio::runtime::Handle::try_current().ok(),
            last_context: None,
            context_cache_ttl: Duration::from_millis(500),
            vision_threshold: VISION_FALLBACK_THRESHOLD,
            network_started: false,
        }
    }

    /// Create a merger with display layer for foreground app detection.
    pub fn with_display(
        accessibility: Box<dyn AccessibilityTree>,
        display: Box<dyn ScreenCapture>,
    ) -> Self {
        Self {
            accessibility,
            display: Some(display),
            network: None,
            vision: None,
            signals: None,
            recent_network: Vec::new(),
            runtime: tokio::runtime::Handle::try_current().ok(),
            last_context: None,
            context_cache_ttl: Duration::from_millis(500),
            vision_threshold: VISION_FALLBACK_THRESHOLD,
            network_started: false,
        }
    }

    /// Create a merger with all available streams.
    pub fn with_all(
        accessibility: Box<dyn AccessibilityTree>,
        display: Box<dyn ScreenCapture>,
        mut network: Box<dyn NetworkMonitor>,
    ) -> Self {
        let network_started = match network.start() {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Network monitor unavailable: {}", e);
                false
            }
        };

        Self {
            accessibility,
            display: Some(display),
            network: Some(network),
            vision: None,
            signals: None,
            recent_network: Vec::new(),
            runtime: tokio::runtime::Handle::try_current().ok(),
            last_context: None,
            context_cache_ttl: Duration::from_millis(500),
            vision_threshold: VISION_FALLBACK_THRESHOLD,
            network_started,
        }
    }

    /// Attach a vision provider for automatic fallback when accessibility is insufficient.
    pub fn with_vision(mut self, vision: Box<dyn VisionProvider>) -> Self {
        self.vision = Some(vision);
        self
    }

    /// Attach a signal bus for supplementary OS signals (clipboard, window list, etc.).
    pub fn with_signals(mut self, signals: Box<dyn SignalBus>) -> Self {
        self.signals = Some(signals);
        self
    }

    /// Set the tokio runtime handle for async vision calls.
    pub fn with_runtime(mut self, handle: tokio::runtime::Handle) -> Self {
        self.runtime = Some(handle);
        self
    }

    /// Set the context cache TTL. The cache avoids re-extracting the expensive
    /// accessibility tree on every `get_context()` call when nothing has changed.
    /// Default is 500ms. Set to `Duration::ZERO` to disable caching.
    pub fn with_cache_ttl(mut self, ttl: Duration) -> Self {
        self.context_cache_ttl = ttl;
        self
    }

    /// Set the minimum actionable element count before triggering vision fallback.
    /// Default: 5. Set higher to reduce vision API calls on simple UIs.
    pub fn with_vision_threshold(mut self, threshold: usize) -> Self {
        self.vision_threshold = threshold;
        self
    }

    /// Describe which streams are currently wired into this merger.
    pub fn stream_status(&self) -> StreamStatus {
        StreamStatus {
            accessibility: true,
            display: self.display.is_some(),
            network: self.network_started,
            signals: self.signals.is_some(),
            vision: self.vision.is_some(),
            audio_capture: false,
        }
    }

    /// Invalidate the context cache, forcing the next `get_context()` call to
    /// re-extract the full accessibility tree.
    pub fn invalidate_cache(&mut self) {
        self.last_context = None;
    }

    /// Build a unified context by querying all available streams.
    ///
    /// Priority order:
    /// 1. Native API (highest confidence, when adapter available)
    /// 2. Accessibility tree (structured, reliable on modern apps)
    /// 3. Vision (automatic fallback when a11y yields few actionable elements)
    /// 4. Network (supplementary — connection state signals)
    pub fn get_context(&mut self) -> ScreenContext {
        // Check if we can reuse the cached accessibility tree extraction
        let use_cache = self
            .last_context
            .as_ref()
            .map(|(t, _)| t.elapsed() < self.context_cache_ttl)
            .unwrap_or(false);

        if use_cache {
            // Safe: use_cache is only true when last_context is Some (checked above)
            let cached = match self.last_context.as_ref() {
                Some((_, ctx)) => ctx.clone(),
                None => unreachable!("use_cache guard ensures last_context is Some"),
            };
            // Refresh signals if available (they're cheap). If signals aren't wired,
            // keep the cached signal values instead of zeroing them.
            if let Some(ref signals) = self.signals {
                let snap = signals.snapshot();
                return ScreenContext {
                    clipboard: snap.clipboard,
                    window_list: snap.window_list,
                    audio: snap.audio,
                    power: snap.power,
                    running_apps: snap.running_apps,
                    recent_files: snap.recent_files,
                    transcripts: cached.transcripts.clone(),
                    ..cached
                };
            }
            return cached;
        }

        let mut elements = Vec::new();

        // Query accessibility tree
        match self.accessibility.get_tree() {
            Ok(tree) => {
                self.flatten_a11y_tree(&tree, &mut elements);
            }
            Err(e) => {
                tracing::warn!("Accessibility tree unavailable: {}", e);
            }
        }

        // Vision fallback: if too few actionable elements, capture screen and run vision
        let actionable_count = elements
            .iter()
            .filter(|e| is_actionable_type(&e.element_type) && e.state.enabled && e.state.visible)
            .count();

        if actionable_count < self.vision_threshold {
            if let Some(vision_elements) = self.run_vision_fallback() {
                for ve in vision_elements {
                    // Check if an a11y element overlaps this vision element.
                    // If so, SUPPLEMENT the a11y element with vision bounds (vision often
                    // sees the actual clickable region more accurately than a11y container bounds).
                    let overlap_idx = elements.iter().position(|e| {
                        if let (Some(eb), Some(vb)) = (&e.bounds, &ve.bounds) {
                            bounds_overlap(eb, vb) > 0.5
                        } else {
                            false
                        }
                    });
                    match overlap_idx {
                        Some(idx) => {
                            // Vision has better bounds for the clickable region —
                            // keep a11y element but upgrade its bounds if vision bounds are smaller
                            // (more precise). Also boost confidence slightly for cross-source confirmation.
                            if let (Some(eb), Some(vb)) = (&elements[idx].bounds, &ve.bounds) {
                                let a11y_area = eb.width as u64 * eb.height as u64;
                                let vision_area = vb.width as u64 * vb.height as u64;
                                if vision_area > 0 && vision_area < a11y_area {
                                    elements[idx].bounds = ve.bounds.clone();
                                }
                            }
                            // Cross-source confirmation: small confidence boost (capped at 0.95)
                            elements[idx].confidence = (elements[idx].confidence + 0.05).min(0.95);
                        }
                        None => {
                            // No overlap — vision found something a11y missed entirely
                            elements.push(ve);
                        }
                    }
                }
            }
        }

        // Drain network events (supplementary context)
        if let Some(ref mut net) = self.network {
            let events = net.drain_events();
            self.recent_network.extend(events);
            // Keep only last 50 events
            if self.recent_network.len() > 50 {
                let drain_count = self.recent_network.len() - 50;
                self.recent_network.drain(..drain_count);
            }
        }

        elements = suppress_obvious_noise(elements);

        // Sort by confidence (highest first)
        elements.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Detect foreground app/window — prefer accessibility data over display layer
        let (app, window) = self.detect_foreground_from_a11y().unwrap_or_else(|| self.detect_foreground());

        // Capture screen dimensions for spatial normalization in reference resolution
        let (screen_width, screen_height) = self
            .display
            .as_ref()
            .map(|d| d.resolution())
            .filter(|&(w, h)| w > 0 && h > 0)
            .map(|(w, h)| (Some(w), Some(h)))
            .unwrap_or((None, None));

        // Snapshot supplementary signals
        let signal_snapshot = self.signals.as_ref().map(|s| s.snapshot());

        let context = ScreenContext {
            app,
            window,
            elements,
            network_events: self.recent_network.clone(),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            screen_width,
            screen_height,
            clipboard: signal_snapshot.as_ref().and_then(|s| s.clipboard.clone()),
            window_list: signal_snapshot.as_ref().map(|s| s.window_list.clone()).unwrap_or_default(),
            audio: signal_snapshot.as_ref().and_then(|s| s.audio.clone()),
            power: signal_snapshot.as_ref().and_then(|s| s.power.clone()),
            running_apps: signal_snapshot.as_ref().map(|s| s.running_apps.clone()).unwrap_or_default(),
            recent_files: signal_snapshot.map(|s| s.recent_files).unwrap_or_default(),
            http_events: vec![],
            transcripts: vec![],
        };

        // Cache the context for subsequent calls within the TTL window
        self.last_context = Some((Instant::now(), context.clone()));

        context
    }

    /// Get focused context for a single element by ID.
    /// Falls back to matching by type + label if the exact ID isn't found
    /// (AX element IDs can change between queries on macOS).
    ///
    /// Always performs a fresh extraction (invalidates context cache) because
    /// focused queries need the most up-to-date element state.
    pub fn get_context_focused(
        &mut self,
        element_id: &str,
    ) -> Option<crate::element::FocusedContext> {
        // Invalidate cache — focused queries must always be fresh
        self.last_context = None;
        let context = self.get_context();

        // Try exact ID match first
        let target = context
            .elements
            .iter()
            .find(|e| e.id == element_id)
            .or_else(|| {
                // Fallback: parse the element_id to extract type+label hints.
                // If the caller provides "button:Submit", match by type and label.
                // Otherwise, try to find by reference resolution.
                // For now, just try to find by the element's old type+label combo
                // using the resolve_reference system.
                None
            })?;

        // Build ancestor path by following parent_id chain
        let mut ancestor_path = Vec::new();
        let mut current_parent = target.parent_id.as_deref();
        let mut depth = 0;
        while let Some(pid) = current_parent {
            if depth > 20 {
                break; // safety limit
            }
            if let Some(parent) = context.elements.iter().find(|e| e.id == pid) {
                let label = parent.label.as_deref().unwrap_or("?");
                ancestor_path.push(format!("{}:{}", parent.element_type, label));
                current_parent = parent.parent_id.as_deref();
            } else {
                break;
            }
            depth += 1;
        }
        ancestor_path.reverse(); // root-first order

        // Collect subtree: elements whose parent_id matches target
        let subtree: Vec<crate::element::ContextElement> = context
            .elements
            .iter()
            .filter(|e| e.parent_id.as_deref() == Some(element_id))
            .cloned()
            .collect();

        Some(crate::element::FocusedContext {
            element: target.clone(),
            subtree,
            ancestor_path,
        })
    }

    /// Get recent network events (for supplementary context).
    pub fn recent_network_events(&self) -> &[NetworkEvent] {
        &self.recent_network
    }

    /// Run vision analysis on the current screen, returning ContextElements.
    /// Returns None if vision is not configured or capture/analysis fails.
    fn run_vision_fallback(&mut self) -> Option<Vec<ContextElement>> {
        let vision = self.vision.as_ref()?;
        let display = self.display.as_mut()?;
        let runtime = self.runtime.as_ref()?;

        let frame = match display.capture_frame() {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("Vision fallback: capture failed: {}", e);
                return None;
            }
        };

        tracing::info!("Running vision fallback ({} provider)", vision.name());

        let vision_prompt = "Focus on interactive elements: buttons, inputs, links, dropdowns, checkboxes, tabs, and menu items. Include text labels and visible headings. Ignore decorative images, backgrounds, and layout containers.";
        let vision_elements = match runtime.block_on(vision.analyze(&frame, vision_prompt, None)) {
            Ok(ve) => ve,
            Err(e) => {
                tracing::warn!("Vision fallback: analysis failed: {}", e);
                return None;
            }
        };

        let context_elements: Vec<ContextElement> = vision_elements
            .into_iter()
            .enumerate()
            .map(|(i, ve)| {
                let role = crate::classify_content_role(&ve.element_type, &[], &cel_accessibility::ElementState::default_visible());
                ContextElement {
                    id: format!("vision:{}", i),
                    label: Some(ve.label),
                    description: None,
                    element_type: ve.element_type,
                    value: None,
                    bounds: ve.bounds.map(|b| Bounds {
                        x: b.x,
                        y: b.y,
                        width: b.width,
                        height: b.height,
                    }),
                    state: cel_accessibility::ElementState::default_visible(),
                    parent_id: None,
                    actions: vec![],
                    confidence: ve.confidence,
                    source: ContextSource::Vision,
                    content_role: role,
                    properties: std::collections::HashMap::new(),
                }
            })
            .collect();

        if context_elements.is_empty() {
            None
        } else {
            Some(context_elements)
        }
    }

    /// Detect the foreground application and window title.
    ///
    /// Strategy:
    /// 1. Try the accessibility tree's focused element (most reliable).
    /// 2. Fall back to the display layer's window list (first non-minimized).
    /// Detect foreground app/window from the accessibility tree.
    /// Returns (app_name, window_title) if available.
    fn detect_foreground_from_a11y(&self) -> Option<(String, String)> {
        let tree = self.accessibility.get_tree().ok()?;
        // The root is typically the window; its label is the window title
        let window_title = tree.label.clone().unwrap_or_default();
        if window_title.is_empty() {
            return None;
        }
        // The app name might differ from the window title — get it from signals or display
        let app_name = self
            .signals
            .as_ref()
            .and_then(|s| {
                let snap = s.snapshot();
                frontmost_app_from_signals(&snap).or_else(|| {
                    snap.window_list
                        .iter()
                        .find(|w| w.is_on_screen && w.title == window_title)
                        .map(|w| w.app_name.clone())
                })
            })
            .or_else(|| {
                // Fallback to display layer when signals not wired
                self.display.as_ref().and_then(|d| {
                    d.list_windows()
                        .ok()
                        .and_then(|w| w.iter().find(|w| !w.is_minimized).map(|w| w.app_name.clone()))
                })
            })
            .unwrap_or_else(|| window_title.clone());
        Some((app_name, window_title))
    }

    fn detect_foreground(&self) -> (String, String) {
        // Strategy 1: signal bus (frontmost app + best matching visible window)
        if let Some(signals) = &self.signals {
            let snap = signals.snapshot();
            if let Some(app_name) = frontmost_app_from_signals(&snap) {
                let window_title = frontmost_window_from_signals(&snap, Some(&app_name))
                    .unwrap_or_else(|| app_name.clone());
                return (app_name, window_title);
            }
            if let Some(window_title) = frontmost_window_from_signals(&snap, None) {
                return (window_title.clone(), window_title);
            }
        }

        // Strategy 2: accessibility focused element (minimal fallback when signals unavailable)
        match self.accessibility.focused_element() {
            Ok(Some(focused)) => {
                let label = focused.label.unwrap_or_default();
                if !label.is_empty() {
                    return (label.clone(), label);
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!("Could not get focused element: {}", e);
            }
        }

        // Strategy 3: display layer window list (fallback when signals not wired)
        if let Some(display) = &self.display {
            if let Ok(windows) = display.list_windows() {
                if let Some(fg) = windows.iter().find(|w| !w.is_minimized) {
                    return (fg.app_name.clone(), fg.title.clone());
                }
            }
        }

        (String::new(), String::new())
    }

    /// Merge additional elements from a native adapter (highest confidence).
    pub fn merge_native_elements(&self, base: &mut ScreenContext, native: Vec<ContextElement>) {
        for elem in native {
            // Check if this element already exists (by ID match)
            if let Some(existing) = base.elements.iter_mut().find(|e| e.id == elem.id) {
                // Native API overrides — higher confidence
                if elem.confidence > existing.confidence {
                    *existing = elem;
                }
            } else {
                base.elements.push(elem);
            }
        }
        // Re-sort
        base.elements.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Merge vision-detected elements, supplementing existing elements where they overlap.
    ///
    /// When a vision element overlaps an existing element (IoU > 0.5):
    /// - Upgrades bounds if vision bounds are more precise (smaller area)
    /// - Boosts confidence by 0.05 for cross-source confirmation
    /// When no overlap exists, adds the vision element as a new discovery.
    pub fn merge_vision_elements(&self, base: &mut ScreenContext, vision: Vec<ContextElement>) {
        for elem in vision {
            let overlap_idx = base.elements.iter().position(|e| {
                if let (Some(eb), Some(vb)) = (&e.bounds, &elem.bounds) {
                    bounds_overlap(eb, vb) > 0.5
                } else {
                    false
                }
            });
            match overlap_idx {
                Some(idx) => {
                    // Supplement: upgrade bounds if vision is more precise
                    if let (Some(eb), Some(vb)) = (&base.elements[idx].bounds, &elem.bounds) {
                        let existing_area = eb.width as u64 * eb.height as u64;
                        let vision_area = vb.width as u64 * vb.height as u64;
                        if vision_area > 0 && vision_area < existing_area {
                            base.elements[idx].bounds = elem.bounds.clone();
                        }
                    }
                    // Cross-source confirmation boost
                    base.elements[idx].confidence =
                        (base.elements[idx].confidence + 0.05).min(0.95);
                }
                None => {
                    base.elements.push(elem);
                }
            }
        }
    }

    /// Flatten the accessibility tree into a list of ContextElements.
    ///
    /// Confidence scoring is based on data quality rather than a hardcoded value:
    /// - Base: 0.60 (element exists in the a11y tree)
    /// - +0.10 if has a non-empty label or value
    /// - +0.10 if has valid bounds (non-zero area)
    /// - +0.05 if state indicates visible and enabled
    /// - +0.05 if element is an actionable type (button, input, etc.)
    /// Maximum: ~0.90 for a fully-qualified element
    fn flatten_a11y_tree(&self, node: &AccessibilityElement, out: &mut Vec<ContextElement>) {
        let element_type = role_to_string(&node.role);

        let bounds = node.bounds.as_ref().map(|b| Bounds {
            x: b.x,
            y: b.y,
            width: b.width,
            height: b.height,
        });

        // Filter out noise: skip elements that are invisible AND have no useful data.
        // Keep the element if it has children (they might be useful), or if it has a label/value.
        let has_label = node.label.as_ref().map_or(false, |l| !l.is_empty());
        let has_value = node.value.as_ref().map_or(false, |v| !v.is_empty());
        if !node.state.visible && !has_label && !has_value && node.children.is_empty() {
            return; // Skip invisible leaf elements with no data
        }

        // Filter offscreen elements: skip elements whose bounds are entirely outside
        // a reasonable screen area (negative coords or beyond 8K resolution).
        if let Some(ref b) = bounds {
            let right = b.x.saturating_add(b.width as i32);
            let bottom = b.y.saturating_add(b.height as i32);
            // Element is entirely offscreen if its right edge is <= 0 or bottom edge is <= 0
            // or its left edge is beyond 8K (7680px) or top edge is beyond 4K (4320px)
            if right <= 0 || bottom <= 0 || b.x >= 7680 || b.y >= 4320 {
                // Still recurse into children — they might have different bounds
                for child in &node.children {
                    self.flatten_a11y_tree(child, out);
                }
                return;
            }
        }

        // Build element, then score via the canonical scoring function
        let mut elem = ContextElement {
            id: node.id.clone(),
            label: node.label.clone(),
            description: node.description.clone(),
            element_type: element_type.to_string(),
            value: node.value.clone(),
            bounds,
            state: node.state.clone(),
            parent_id: node.parent_id.clone(),
            actions: node.actions.clone(),
            confidence: 0.0, // Will be scored below
            source: ContextSource::AccessibilityTree,
            content_role: crate::classify_content_role(
                &element_type, &node.actions, &node.state,
            ),
            properties: node.properties.clone(),
        };
        elem.confidence = score_element_confidence(&elem);

        out.push(elem);

        for child in &node.children {
            self.flatten_a11y_tree(child, out);
        }
    }
}

fn role_to_string(role: &ElementRole) -> &str {
    match role {
        ElementRole::Button => "button",
        ElementRole::Input => "input",
        ElementRole::Text => "text",
        ElementRole::Window => "window",
        ElementRole::List => "list",
        ElementRole::ListItem => "list_item",
        ElementRole::Menu => "menu",
        ElementRole::MenuItem => "menu_item",
        ElementRole::Checkbox => "checkbox",
        ElementRole::ComboBox => "combobox",
        ElementRole::Table => "table",
        ElementRole::TableRow => "table_row",
        ElementRole::TableCell => "table_cell",
        ElementRole::Dialog => "dialog",
        ElementRole::Tab => "tab",
        ElementRole::TabItem => "tab_item",
        ElementRole::RadioButton => "radio_button",
        ElementRole::Slider => "slider",
        ElementRole::ScrollBar => "scrollbar",
        ElementRole::TreeView => "tree_view",
        ElementRole::TreeItem => "tree_item",
        ElementRole::Toolbar => "toolbar",
        ElementRole::StatusBar => "status_bar",
        ElementRole::Group => "group",
        ElementRole::Image => "image",
        ElementRole::Link => "link",
        ElementRole::Custom(s) => s.as_str(),
        ElementRole::Unknown => "unknown",
    }
}

// ─── Public helpers (single source of truth for scoring, mapping, actions) ────

/// Score a ContextElement based on data quality.
/// This is the single source of truth for confidence scoring across ALL sources
/// (accessibility tree, browser CDP, native API, vision).
///
/// Range: 0.60 (base, element exists) to 0.90 (fully-qualified).
pub fn score_element_confidence(element: &ContextElement) -> f64 {
    let mut confidence: f64 = 0.60;

    let has_label = element.label.as_ref().map_or(false, |l| !l.is_empty());
    let has_value = element.value.as_ref().map_or(false, |v| !v.is_empty());
    if has_label || has_value {
        confidence += 0.10;
    }

    if let Some(ref b) = element.bounds {
        if b.width > 0 && b.height > 0 {
            confidence += 0.10;
        }
    }

    if element.state.visible && element.state.enabled {
        confidence += 0.05;
    }

    if is_actionable_type(&element.element_type) {
        confidence += 0.05;
    }

    if !element.actions.is_empty() {
        confidence += 0.05;
    }

    confidence.min(0.95)
}

/// Normalize an ARIA role string (from browser CDP Accessibility.getFullAXTree)
/// to a CEL element type string.
///
/// This is the comprehensive superset of `role_to_string()` (which handles
/// the 26-variant ElementRole enum). It maps all ARIA roles that Chrome's
/// accessibility tree may return.
pub fn aria_role_to_cel_type(role: &str) -> &'static str {
    match role {
        "button" => "button",
        "link" | "a" => "link",
        "textbox" | "searchbox" | "spinbutton" | "input" | "textarea" => "input",
        "checkbox" | "switch" => "checkbox",
        "radio" => "radio_button",
        "combobox" | "listbox" | "select" => "combobox",
        "menuitem" | "menuitemcheckbox" | "menuitemradio" => "menu_item",
        "tab" => "tab_item",
        "slider" | "progressbar" | "meter" => "slider",
        "treeitem" => "tree_item",
        "option" | "listitem" => "list_item",
        "gridcell" | "rowheader" | "columnheader" | "cell" => "table_cell",
        "dialog" | "alertdialog" => "dialog",
        "menu" | "menubar" => "menu",
        "navigation" | "toolbar" => "toolbar",
        "tablist" | "tabpanel" | "group" | "region" | "banner"
        | "complementary" | "contentinfo" | "form" | "main"
        | "search" | "article" | "section" | "aside"
        | "header" | "footer" | "fieldset" | "figure"
        | "details" | "summary" => "group",
        "tree" => "tree_view",
        "grid" | "table" | "treegrid" => "table",
        "row" => "table_row",
        "img" | "image" => "image",
        "status" => "status_bar",
        "list" | "directory" | "feed" => "list",
        "heading" | "separator" | "paragraph" | "blockquote"
        | "caption" | "code" | "definition" | "deletion"
        | "insertion" | "mark" | "math" | "note"
        | "subscript" | "superscript" | "term" | "time" => "text",
        "scrollbar" => "scrollbar",
        "window" | "application" | "document" => "window",
        _ => "text",
    }
}

/// Assign default actions for a CEL element type.
/// Used when the source doesn't provide explicit actions (e.g., browser CDP).
pub fn assign_default_actions(element_type: &str) -> Vec<String> {
    match element_type {
        "button" => vec!["click".into(), "press".into()],
        "input" => vec!["activate".into(), "set".into()],
        "link" => vec!["click".into(), "jump".into()],
        "checkbox" | "radio_button" => vec!["toggle".into()],
        "combobox" => vec!["select".into(), "activate".into()],
        "slider" => vec!["set".into()],
        "menu_item" | "tab_item" | "tree_item" | "list_item" => {
            vec!["click".into(), "activate".into()]
        }
        _ => vec![],
    }
}

/// Build a ScreenContext from externally-provided elements (e.g., browser CDP).
///
/// Applies the unified Rust pipeline:
/// 1. Filter invisible/offscreen noise
/// 2. Normalize element_type from ARIA role strings
/// 3. Score confidence (single source of truth)
/// 4. Assign default actions when empty
/// 5. Filter unlabeled leaf groups
/// 6. Sort by confidence descending
///
/// This is the browser adapter's entry point into the Rust pipeline.
/// Elements should have `element_type` set to the raw ARIA role string —
/// this function normalizes it to CEL types.
pub fn build_from_external(
    elements: Vec<ContextElement>,
    http_events: Vec<cel_network::HttpEvent>,
    app: String,
    window: String,
) -> ScreenContext {
    let mut processed: Vec<ContextElement> = elements
        .into_iter()
        .filter(|e| {
            // Filter invisible leaf elements with no data
            let has_label = e.label.as_ref().map_or(false, |l| !l.is_empty());
            let has_value = e.value.as_ref().map_or(false, |v| !v.is_empty());
            if !e.state.visible && !has_label && !has_value {
                return false;
            }
            // Filter offscreen elements
            if let Some(ref b) = e.bounds {
                let right = b.x.saturating_add(b.width as i32);
                let bottom = b.y.saturating_add(b.height as i32);
                if right <= 0 || bottom <= 0 || b.x >= 7680 || b.y >= 4320 {
                    return false;
                }
            }
            true
        })
        .map(|mut e| {
            // Normalize element type from ARIA role to CEL type
            e.element_type = aria_role_to_cel_type(&e.element_type).to_string();
            // Apply unified confidence scoring
            e.confidence = score_element_confidence(&e);
            // Assign default actions if empty
            if e.actions.is_empty() {
                e.actions = assign_default_actions(&e.element_type);
            }
            e
        })
        .collect();

    processed = suppress_obvious_noise(processed);

    // Sort by confidence (highest first)
    processed.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Keep only last 50 HTTP events
    let http = if http_events.len() > 50 {
        http_events[http_events.len() - 50..].to_vec()
    } else {
        http_events
    };

    ScreenContext {
        app,
        window,
        elements: processed,
        network_events: vec![],
        timestamp_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        screen_width: None,
        screen_height: None,
        clipboard: None,
        window_list: vec![],
        audio: None,
        power: None,
        running_apps: vec![],
        recent_files: vec![],
        http_events: http,
        transcripts: vec![],
    }
}

/// Whether an element type is "actionable" — interactive elements that an agent can click/type into.
pub fn is_actionable_type(element_type: &str) -> bool {
    matches!(
        element_type,
        "button"
            | "input"
            | "link"
            | "checkbox"
            | "radio_button"
            | "combobox"
            | "menu_item"
            | "tab_item"
            | "slider"
            | "list_item"
            | "tree_item"
    )
}

fn normalized_text(text: Option<&str>) -> String {
    text.unwrap_or("")
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_generic_action_label(label: &str) -> bool {
    GENERIC_ACTION_LABELS.contains(&label)
}

fn has_noise_hint(el: &ContextElement) -> bool {
    let selector = el.properties.get("css_selector").map(String::as_str).unwrap_or("");
    let dom_id = el.properties.get("dom_id").map(String::as_str).unwrap_or("");
    let combined = format!("{} {}", selector.to_lowercase(), dom_id.to_lowercase());
    CHROME_NOISE_HINTS.iter().any(|hint| combined.contains(hint))
}

fn suppress_obvious_noise(mut elements: Vec<ContextElement>) -> Vec<ContextElement> {
    use std::collections::HashMap;

    // Pre-compute normalized labels once (was N² calls, now N).
    let normalized: Vec<String> = elements
        .iter()
        .map(|e| normalized_text(e.label.as_deref()))
        .collect();

    // (element_type, normalized_label) → occurrence count, for O(1) repeated_label_count.
    let mut label_counts: HashMap<(String, String), usize> = HashMap::new();
    for (el, n) in elements.iter().zip(normalized.iter()) {
        if !n.is_empty() {
            *label_counts.entry((el.element_type.clone(), n.clone())).or_insert(0) += 1;
        }
    }

    // parent_id → child indices, for O(1) sibling lookup in has_structural_identity.
    let mut by_parent: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, el) in elements.iter().enumerate() {
        if let Some(pid) = el.parent_id.as_deref() {
            by_parent.entry(pid.to_string()).or_default().push(idx);
        }
    }

    // id → index, for O(1) parent lookup.
    let id_to_idx: HashMap<String, usize> = elements
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.clone(), i))
        .collect();

    // Phase 1: noise penalty (was O(N²) due to per-element O(N) scans inside).
    let snapshot_for_phase1 = elements.clone();
    for (i, el) in elements.iter_mut().enumerate() {
        apply_noise_penalty_cached(
            el, &normalized[i], &snapshot_for_phase1, &normalized,
            &label_counts, &by_parent, &id_to_idx,
        );
    }

    // Phase 2: filter out leaf groups + obvious noise (was O(N²)).
    let remove_leaf_groups = elements.len() > 15;
    let parent_ids: std::collections::HashSet<&str> = elements
        .iter()
        .filter_map(|e| e.parent_id.as_deref())
        .collect();
    let snapshot_for_phase2 = snapshot_for_phase1;
    let mut keep = vec![true; elements.len()];
    for (i, el) in elements.iter().enumerate() {
        if remove_leaf_groups
            && el.element_type == "group"
            && normalized[i].is_empty()
            && !parent_ids.contains(el.id.as_str())
        {
            keep[i] = false;
            continue;
        }
        if is_actionable_type(&el.element_type)
            && !normalized[i].is_empty()
            && is_generic_action_label(&normalized[i])
            && !has_structural_identity_cached(el, &snapshot_for_phase2, &normalized, &by_parent, &id_to_idx)
            && has_noise_hint(el)
        {
            keep[i] = false;
        }
    }
    let mut filtered: Vec<ContextElement> = Vec::with_capacity(elements.len());
    let mut filtered_norm: Vec<String> = Vec::with_capacity(elements.len());
    for (i, (el, n)) in elements.into_iter().zip(normalized.into_iter()).enumerate() {
        if keep[i] {
            filtered_norm.push(n);
            filtered.push(el);
        }
    }

    // Phase 3: dedup (was O(N²) with normalized_text in the inner loop).
    let mut deduped: Vec<ContextElement> = Vec::with_capacity(filtered.len());
    let mut deduped_norm: Vec<String> = Vec::with_capacity(filtered.len());
    let snapshot_for_dedup = filtered.clone();
    let dedup_normalized = filtered_norm.clone();

    'outer: for (i, el) in filtered.into_iter().enumerate() {
        for (j, existing) in deduped.iter_mut().enumerate() {
            if are_overlapping_duplicates_cached(existing, &deduped_norm[j], &el, &filtered_norm[i]) {
                if !should_keep_over_cached(
                    existing, &el, &snapshot_for_dedup, &dedup_normalized, &by_parent, &id_to_idx,
                ) {
                    deduped_norm[j] = filtered_norm[i].clone();
                    *existing = el;
                }
                continue 'outer;
            }
        }
        deduped_norm.push(filtered_norm[i].clone());
        deduped.push(el);
    }

    deduped
}

// ─── Cached versions of the helpers above ────────────────────────────────────

fn apply_noise_penalty_cached(
    el: &mut ContextElement,
    label: &str,
    elements: &[ContextElement],
    normalized: &[String],
    label_counts: &std::collections::HashMap<(String, String), usize>,
    by_parent: &std::collections::HashMap<String, Vec<usize>>,
    id_to_idx: &std::collections::HashMap<String, usize>,
) {
    let mut penalty = 0.0_f64;
    let mut reasons: Vec<&str> = Vec::new();

    if el.content_role == ContentRole::System {
        penalty += 0.12;
        reasons.push("system");
    }

    if is_actionable_type(&el.element_type) && !label.is_empty() && is_generic_action_label(label) {
        penalty += 0.12;
        reasons.push("generic_label");

        // Pre-computed: count includes self, so >= 2 means at least one other.
        let count = label_counts
            .get(&(el.element_type.clone(), label.to_string()))
            .copied()
            .unwrap_or(0);
        if count >= 3 {
            // 1 (self) + 2 others = 3
            penalty += 0.08;
            reasons.push("repeated");
        }

        if !has_structural_identity_cached(el, elements, normalized, by_parent, id_to_idx) {
            penalty += 0.08;
            reasons.push("no_identity");
        }
    }

    if has_noise_hint(el) {
        penalty += 0.12;
        reasons.push("chrome_hint");
    }

    if !reasons.is_empty() {
        el.confidence = (el.confidence - penalty).max(0.15);
        el.properties.insert("noise_penalty".into(), format!("{:.2}", penalty));
        el.properties.insert("noise_reasons".into(), reasons.join(","));
    }
}

fn has_structural_identity_cached(
    el: &ContextElement,
    elements: &[ContextElement],
    normalized: &[String],
    by_parent: &std::collections::HashMap<String, Vec<usize>>,
    id_to_idx: &std::collections::HashMap<String, usize>,
) -> bool {
    let parent_id = match el.parent_id.as_deref() {
        Some(pid) => pid,
        None => return false,
    };

    if let Some(&parent_idx) = id_to_idx.get(parent_id) {
        if !normalized[parent_idx].is_empty() {
            return true;
        }
    }

    if let Some(siblings) = by_parent.get(parent_id) {
        let sibling_labels = siblings
            .iter()
            .filter(|&&i| elements[i].id != el.id)
            .filter(|&&i| {
                !normalized[i].is_empty()
                    || elements[i].value.as_ref().map_or(false, |v| !normalized_text(Some(v)).is_empty())
            })
            .count();
        if sibling_labels >= 2 {
            return true;
        }
    }

    false
}

fn are_overlapping_duplicates_cached(
    a: &ContextElement,
    label_a: &str,
    b: &ContextElement,
    label_b: &str,
) -> bool {
    if a.element_type != b.element_type || label_a.is_empty() || label_a != label_b {
        return false;
    }
    match (&a.bounds, &b.bounds) {
        (Some(bounds_a), Some(bounds_b)) => bounds_overlap(bounds_a, bounds_b) >= 0.92,
        _ => false,
    }
}

fn should_keep_over_cached(
    existing: &ContextElement,
    candidate: &ContextElement,
    elements: &[ContextElement],
    normalized: &[String],
    by_parent: &std::collections::HashMap<String, Vec<usize>>,
    id_to_idx: &std::collections::HashMap<String, usize>,
) -> bool {
    let existing_richness = context_richness_cached(existing, elements, normalized, by_parent, id_to_idx);
    let candidate_richness = context_richness_cached(candidate, elements, normalized, by_parent, id_to_idx);
    if existing_richness != candidate_richness {
        return existing_richness > candidate_richness;
    }
    existing.confidence >= candidate.confidence
}

fn context_richness_cached(
    el: &ContextElement,
    elements: &[ContextElement],
    normalized: &[String],
    by_parent: &std::collections::HashMap<String, Vec<usize>>,
    id_to_idx: &std::collections::HashMap<String, usize>,
) -> usize {
    let label_len = el.label.as_ref().map_or(0, |label| label.len().min(32));
    let value_len = el.value.as_ref().map_or(0, |value| value.len().min(16));
    let structural = usize::from(has_structural_identity_cached(el, elements, normalized, by_parent, id_to_idx)) * 4;
    el.actions.len() * 3 + el.properties.len() * 2 + label_len + value_len + structural
}

/// Compute intersection-over-union of two bounding boxes.
fn bounds_overlap(a: &Bounds, b: &Bounds) -> f64 {
    let ax2 = a.x.saturating_add(a.width as i32);
    let ay2 = a.y.saturating_add(a.height as i32);
    let bx2 = b.x.saturating_add(b.width as i32);
    let by2 = b.y.saturating_add(b.height as i32);

    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);

    if ix1 >= ix2 || iy1 >= iy2 {
        return 0.0;
    }

    let intersection = (ix2 - ix1) as f64 * (iy2 - iy1) as f64;
    let area_a = a.width as f64 * a.height as f64;
    let area_b = b.width as f64 * b.height as f64;
    let union = area_a + area_b - intersection;

    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bounds_overlap_full() {
        let a = Bounds { x: 0, y: 0, width: 100, height: 100 };
        let b = Bounds { x: 0, y: 0, width: 100, height: 100 };
        assert!((bounds_overlap(&a, &b) - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_bounds_overlap_none() {
        let a = Bounds { x: 0, y: 0, width: 50, height: 50 };
        let b = Bounds { x: 100, y: 100, width: 50, height: 50 };
        assert_eq!(bounds_overlap(&a, &b), 0.0);
    }

    #[test]
    fn test_bounds_overlap_partial() {
        let a = Bounds { x: 0, y: 0, width: 100, height: 100 };
        let b = Bounds { x: 50, y: 50, width: 100, height: 100 };
        let iou = bounds_overlap(&a, &b);
        assert!(iou > 0.0 && iou < 1.0);
    }

    #[test]
    fn test_bounds_overlap_adjacent() {
        let a = Bounds { x: 0, y: 0, width: 50, height: 50 };
        let b = Bounds { x: 50, y: 0, width: 50, height: 50 };
        assert_eq!(bounds_overlap(&a, &b), 0.0);
    }

    #[test]
    fn test_bounds_overlap_contained() {
        let a = Bounds { x: 0, y: 0, width: 200, height: 200 };
        let b = Bounds { x: 50, y: 50, width: 50, height: 50 };
        let iou = bounds_overlap(&a, &b);
        assert!(iou > 0.0 && iou < 0.1);
    }

    #[test]
    fn test_get_context_with_stub() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let mut merger = ContextMerger::new(stub);
        let ctx = merger.get_context();
        assert!(!ctx.elements.is_empty());
        assert_eq!(ctx.elements[0].element_type, "window");
        // Stub has label "Stub Window", bounds 1920x1080, visible+enabled → 0.60+0.10+0.10+0.05 = 0.85
        assert!(ctx.elements[0].confidence >= 0.60, "Confidence should be at least base 0.60");
        assert!(ctx.elements[0].confidence <= 0.95, "Confidence should be at most 0.95");
        assert_eq!(ctx.elements[0].source, ContextSource::AccessibilityTree);
        assert!(ctx.timestamp_ms > 0);
    }

    #[test]
    fn test_merge_native_elements_overrides_by_id() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let merger = ContextMerger::new(stub);
        let mut ctx = ScreenContext {
            app: "".into(), window: "".into(), network_events: vec![], http_events: vec![], timestamp_ms: 0, screen_width: None, screen_height: None,
            clipboard: None, window_list: vec![],
            audio: None, power: None, running_apps: vec![], recent_files: vec![], transcripts: vec![],
            elements: vec![ContextElement {
                id: "root".into(),
                label: Some("Stub Window".into()),
                description: None,
                element_type: "window".into(),
                value: None,
                bounds: None,
                state: cel_accessibility::ElementState::default(),
                parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.85,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::default(),
            }],
        };

        let native = vec![ContextElement {
            id: "root".into(),
            label: Some("Excel".into()),
            description: None,
            element_type: "window".into(),
            value: None,
            bounds: None,
            state: cel_accessibility::ElementState::default(),
            parent_id: None,
            actions: vec![],
            confidence: 0.98,
            source: ContextSource::NativeApi,
            properties: std::collections::HashMap::new(),
            content_role: ContentRole::default(),
        }];

        merger.merge_native_elements(&mut ctx, native);
        let root = ctx.elements.iter().find(|e| e.id == "root").unwrap();
        assert_eq!(root.confidence, 0.98);
        assert_eq!(root.source, ContextSource::NativeApi);
        assert_eq!(root.label.as_deref(), Some("Excel"));
    }

    #[test]
    fn test_merge_native_elements_adds_new() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let merger = ContextMerger::new(stub);
        let mut ctx = ScreenContext {
            app: "".into(), window: "".into(), network_events: vec![], http_events: vec![], timestamp_ms: 0, screen_width: None, screen_height: None,
            clipboard: None, window_list: vec![],
            audio: None, power: None, running_apps: vec![], recent_files: vec![], transcripts: vec![],
            elements: vec![ContextElement {
                id: "root".into(),
                label: None,
                description: None,
                element_type: "window".into(),
                value: None,
                bounds: None,
                state: cel_accessibility::ElementState::default(),
                parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.85,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::default(),
            }],
        };
        let initial_count = ctx.elements.len();

        let native = vec![ContextElement {
            id: "excel:A1".into(),
            label: Some("Cell A1".into()),
            description: None,
            element_type: "table_cell".into(),
            value: Some("Revenue".into()),
            bounds: Some(Bounds { x: 120, y: 200, width: 80, height: 20 }),
            state: cel_accessibility::ElementState::default(),
            parent_id: None,
            actions: vec![],
            confidence: 0.98,
            source: ContextSource::NativeApi,
            properties: std::collections::HashMap::new(),
            content_role: ContentRole::default(),
        }];

        merger.merge_native_elements(&mut ctx, native);
        assert_eq!(ctx.elements.len(), initial_count + 1);
        assert_eq!(ctx.elements[0].confidence, 0.98);
    }

    #[test]
    fn test_merge_vision_elements_no_overlap() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let merger = ContextMerger::new(stub);
        let mut ctx = ScreenContext {
            app: "test".into(),
            window: "test".into(),
            network_events: vec![],
            http_events: vec![],
            elements: vec![],
            timestamp_ms: 0,
            screen_width: None,
            screen_height: None,
            clipboard: None,
            window_list: vec![],
            audio: None,
            power: None,
            running_apps: vec![],
            recent_files: vec![],
            transcripts: vec![],
        };

        let vision = vec![ContextElement {
            id: "vision:btn:1".into(),
            label: Some("Submit".into()),
            description: None,
            element_type: "button".into(),
            value: None,
            bounds: Some(Bounds { x: 500, y: 500, width: 100, height: 40 }),
            state: cel_accessibility::ElementState::default(),
            parent_id: None,
            actions: vec![],
            confidence: 0.75,
            source: ContextSource::Vision,
            properties: std::collections::HashMap::new(),
            content_role: ContentRole::default(),
        }];

        merger.merge_vision_elements(&mut ctx, vision);
        assert_eq!(ctx.elements.len(), 1);
    }

    #[test]
    fn test_merge_vision_supplements_overlapping_element() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let merger = ContextMerger::new(stub);
        let mut ctx = ScreenContext {
            app: "test".into(),
            window: "test".into(),
            network_events: vec![],
            http_events: vec![],
            elements: vec![ContextElement {
                id: "a11y:btn:1".into(),
                label: Some("OK".into()),
                description: None,
                element_type: "button".into(),
                value: None,
                bounds: Some(Bounds { x: 100, y: 100, width: 80, height: 30 }),
                state: cel_accessibility::ElementState::default(),
                parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.85,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::default(),
            }],
            timestamp_ms: 0,
            screen_width: None,
            screen_height: None,
            clipboard: None,
            window_list: vec![],
            audio: None,
            power: None,
            running_apps: vec![],
            recent_files: vec![],
            transcripts: vec![],
        };

        // Vision element with same bounds — should supplement, not add
        let vision = vec![ContextElement {
            id: "vision:btn:1".into(),
            label: Some("OK".into()),
            description: None,
            element_type: "button".into(),
            value: None,
            bounds: Some(Bounds { x: 100, y: 100, width: 80, height: 30 }),
            state: cel_accessibility::ElementState::default(),
            parent_id: None,
            actions: vec![],
            confidence: 0.70,
            source: ContextSource::Vision,
            properties: std::collections::HashMap::new(),
            content_role: ContentRole::default(),
        }];

        merger.merge_vision_elements(&mut ctx, vision);
        // Still 1 element (merged, not added)
        assert_eq!(ctx.elements.len(), 1);
        // Source stays a11y, but confidence boosted
        assert_eq!(ctx.elements[0].source, ContextSource::AccessibilityTree);
        assert_eq!(ctx.elements[0].confidence, 0.90, "Should be boosted from 0.85 by 0.05");
    }

    #[test]
    fn test_merge_vision_upgrades_to_more_precise_bounds() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let merger = ContextMerger::new(stub);
        let mut ctx = ScreenContext {
            app: "test".into(),
            window: "test".into(),
            network_events: vec![],
            http_events: vec![],
            elements: vec![ContextElement {
                id: "a11y:btn:1".into(),
                label: Some("OK".into()),
                description: None,
                element_type: "button".into(),
                value: None,
                bounds: Some(Bounds { x: 100, y: 100, width: 100, height: 50 }), // A11y bounds (slightly larger)
                state: cel_accessibility::ElementState::default(),
                parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.85,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::default(),
            }],
            timestamp_ms: 0,
            screen_width: None,
            screen_height: None,
            clipboard: None,
            window_list: vec![],
            audio: None,
            power: None,
            running_apps: vec![],
            recent_files: vec![],
            transcripts: vec![],
        };

        // Vision sees smaller, more precise clickable region
        let vision = vec![ContextElement {
            id: "vision:btn:1".into(),
            label: Some("OK".into()),
            description: None,
            element_type: "button".into(),
            value: None,
            bounds: Some(Bounds { x: 105, y: 105, width: 80, height: 40 }),
            state: cel_accessibility::ElementState::default(),
            parent_id: None,
            actions: vec![],
            confidence: 0.70,
            source: ContextSource::Vision,
            properties: std::collections::HashMap::new(),
            content_role: ContentRole::default(),
        }];

        merger.merge_vision_elements(&mut ctx, vision);
        assert_eq!(ctx.elements.len(), 1);
        // Bounds should be upgraded to the smaller vision bounds
        let b = ctx.elements[0].bounds.as_ref().unwrap();
        assert_eq!(b.width, 80);
        assert_eq!(b.height, 40);
    }

    #[test]
    fn test_elements_sorted_by_confidence() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let merger = ContextMerger::new(stub);
        let mut ctx = ScreenContext {
            app: "".into(), window: "".into(), network_events: vec![], http_events: vec![], timestamp_ms: 0, screen_width: None, screen_height: None,
            clipboard: None, window_list: vec![],
            audio: None, power: None, running_apps: vec![], recent_files: vec![], transcripts: vec![],
            elements: vec![ContextElement {
                id: "root".into(), label: None, description: None,
                element_type: "window".into(),
                value: None, bounds: None, state: cel_accessibility::ElementState::default(), parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.85, source: ContextSource::AccessibilityTree,
                content_role: ContentRole::default(),
            }],
        };

        let native = vec![
            ContextElement {
                id: "low".into(), label: None, description: None,
                element_type: "text".into(),
                value: None, bounds: None, state: cel_accessibility::ElementState::default(), parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.50, source: ContextSource::NativeApi,
                content_role: ContentRole::default(),
            },
            ContextElement {
                id: "high".into(), label: None, description: None,
                element_type: "button".into(),
                value: None, bounds: None, state: cel_accessibility::ElementState::default(), parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.99, source: ContextSource::NativeApi,
                content_role: ContentRole::default(),
            },
        ];

        merger.merge_native_elements(&mut ctx, native);
        for i in 0..ctx.elements.len() - 1 {
            assert!(ctx.elements[i].confidence >= ctx.elements[i + 1].confidence);
        }
    }

    #[test]
    fn test_role_to_string_all_variants() {
        let mappings = vec![
            (ElementRole::Button, "button"),
            (ElementRole::Input, "input"),
            (ElementRole::Text, "text"),
            (ElementRole::Window, "window"),
            (ElementRole::List, "list"),
            (ElementRole::ListItem, "list_item"),
            (ElementRole::Menu, "menu"),
            (ElementRole::MenuItem, "menu_item"),
            (ElementRole::Checkbox, "checkbox"),
            (ElementRole::ComboBox, "combobox"),
            (ElementRole::Table, "table"),
            (ElementRole::TableRow, "table_row"),
            (ElementRole::TableCell, "table_cell"),
            (ElementRole::Dialog, "dialog"),
            (ElementRole::Tab, "tab"),
            (ElementRole::TabItem, "tab_item"),
            (ElementRole::RadioButton, "radio_button"),
            (ElementRole::Slider, "slider"),
            (ElementRole::ScrollBar, "scrollbar"),
            (ElementRole::TreeView, "tree_view"),
            (ElementRole::TreeItem, "tree_item"),
            (ElementRole::Toolbar, "toolbar"),
            (ElementRole::StatusBar, "status_bar"),
            (ElementRole::Group, "group"),
            (ElementRole::Image, "image"),
            (ElementRole::Link, "link"),
            (ElementRole::Custom("widget".into()), "widget"),
        ];
        for (role, expected) in mappings {
            assert_eq!(role_to_string(&role), expected);
        }
    }

    #[test]
    fn test_recent_network_events_empty() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let merger = ContextMerger::new(stub);
        assert!(merger.recent_network_events().is_empty());
    }

    #[test]
    fn test_with_all_constructor() {
        let a11y = Box::new(cel_accessibility::StubAccessibility);
        let net = Box::new(cel_network::StubNetworkMonitor);

        // We can't easily construct a stub ScreenCapture, so test network path manually
        let merger = ContextMerger {
            accessibility: a11y,
            display: None,
            network: Some(net),
            vision: None,
            signals: None,
            recent_network: Vec::new(),
            runtime: None,
            last_context: None,
            context_cache_ttl: Duration::from_millis(500),
            vision_threshold: VISION_FALLBACK_THRESHOLD,
            network_started: false,
        };
        assert!(merger.recent_network_events().is_empty());
    }

    struct MockSignals {
        snapshot: cel_signals::SignalSnapshot,
    }

    impl cel_signals::SignalBus for MockSignals {
        fn snapshot(&self) -> cel_signals::SignalSnapshot {
            self.snapshot.clone()
        }
    }

    #[test]
    fn test_signals_frontmost_app_overrides_window_title_as_app_name() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let signals = Box::new(MockSignals {
            snapshot: cel_signals::SignalSnapshot {
                clipboard: None,
                window_list: vec![cel_signals::WindowState {
                    app_name: "Google Chrome".into(),
                    title: "CrossFlow Dashboard".into(),
                    x: 0,
                    y: 0,
                    width: 1440,
                    height: 900,
                    layer: 0,
                    is_on_screen: true,
                    pid: 42,
                }],
                audio: None,
                power: None,
                running_apps: vec![cel_signals::RunningApp {
                    name: "Google Chrome".into(),
                    is_frontmost: true,
                }],
                recent_files: vec![],
            },
        });
        let mut merger = ContextMerger::new(stub).with_signals(signals);

        let ctx = merger.get_context();

        assert_eq!(ctx.app, "Google Chrome");
        assert_eq!(ctx.window, "Stub Window");
    }

    #[test]
    fn test_is_actionable_type() {
        assert!(is_actionable_type("button"));
        assert!(is_actionable_type("input"));
        assert!(is_actionable_type("link"));
        assert!(is_actionable_type("checkbox"));
        assert!(is_actionable_type("radio_button"));
        assert!(is_actionable_type("combobox"));
        assert!(is_actionable_type("menu_item"));
        assert!(is_actionable_type("tab_item"));
        assert!(is_actionable_type("slider"));
        assert!(is_actionable_type("list_item"));
        assert!(is_actionable_type("tree_item"));
        assert!(!is_actionable_type("window"));
        assert!(!is_actionable_type("text"));
        assert!(!is_actionable_type("group"));
        assert!(!is_actionable_type("table"));
        assert!(!is_actionable_type("dialog"));
        assert!(!is_actionable_type("toolbar"));
        assert!(!is_actionable_type("status_bar"));
        assert!(!is_actionable_type("image"));
        assert!(!is_actionable_type(""));
        assert!(!is_actionable_type("unknown_type"));
    }

    #[test]
    fn test_bounds_overlap_iou_value() {
        // 50% overlap: two 100x100 boxes offset by 50px
        let a = Bounds { x: 0, y: 0, width: 100, height: 100 };
        let b = Bounds { x: 50, y: 0, width: 100, height: 100 };
        let iou = bounds_overlap(&a, &b);
        // Intersection: 50x100 = 5000, Union: 10000 + 10000 - 5000 = 15000
        let expected = 5000.0 / 15000.0;
        assert!((iou - expected).abs() < 0.01, "Expected IoU ~{:.3}, got {:.3}", expected, iou);
    }

    #[test]
    fn test_bounds_overlap_zero_area() {
        let a = Bounds { x: 10, y: 10, width: 0, height: 0 };
        let b = Bounds { x: 10, y: 10, width: 0, height: 0 };
        assert_eq!(bounds_overlap(&a, &b), 0.0, "Zero-area bounds should have 0 IoU");
    }

    #[test]
    fn test_merge_vision_preserves_source_and_confidence() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let merger = ContextMerger::new(stub);
        let mut ctx = ScreenContext {
            app: "test".into(), window: "test".into(),
            network_events: vec![], http_events: vec![], elements: vec![], timestamp_ms: 0,
            screen_width: None, screen_height: None,
            clipboard: None, window_list: vec![],
            audio: None, power: None, running_apps: vec![], recent_files: vec![], transcripts: vec![],
        };

        let vision = vec![
            ContextElement {
                id: "vision:0".into(),
                label: Some("Submit".into()),
                description: None,
                element_type: "button".into(),
                value: None,
                bounds: Some(Bounds { x: 100, y: 200, width: 80, height: 30 }),
                state: cel_accessibility::ElementState::default(),
                parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.72,
                source: ContextSource::Vision,
                content_role: ContentRole::default(),
            },
            ContextElement {
                id: "vision:1".into(),
                label: Some("Cancel".into()),
                description: None,
                element_type: "button".into(),
                value: None,
                bounds: Some(Bounds { x: 200, y: 200, width: 80, height: 30 }),
                state: cel_accessibility::ElementState::default(),
                parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.68,
                source: ContextSource::Vision,
                content_role: ContentRole::default(),
            },
        ];

        merger.merge_vision_elements(&mut ctx, vision);

        assert_eq!(ctx.elements.len(), 2);
        for e in &ctx.elements {
            assert_eq!(e.source, ContextSource::Vision);
            assert!(e.confidence < 0.85, "Vision elements should have lower confidence");
            assert!(e.bounds.is_some(), "Vision elements should have bounds");
            assert!(e.label.is_some(), "Vision elements should have labels");
        }
    }

    #[test]
    fn test_merge_native_does_not_lower_confidence() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let merger = ContextMerger::new(stub);
        let mut ctx = ScreenContext {
            app: "".into(), window: "".into(), network_events: vec![], http_events: vec![], timestamp_ms: 0, screen_width: None, screen_height: None,
            clipboard: None, window_list: vec![],
            audio: None, power: None, running_apps: vec![], recent_files: vec![], transcripts: vec![],
            elements: vec![ContextElement {
                id: "btn1".into(),
                label: Some("OK".into()),
                description: None,
                element_type: "button".into(),
                value: None, bounds: None,
                state: cel_accessibility::ElementState::default(), parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.95,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::default(),
            }],
        };

        // Native element with LOWER confidence should NOT override
        let native = vec![ContextElement {
            id: "btn1".into(),
            label: Some("OK (native)".into()),
            description: None,
            element_type: "button".into(),
            value: None, bounds: None,
            state: cel_accessibility::ElementState::default(), parent_id: None,
            actions: vec![],
            confidence: 0.80,
            source: ContextSource::NativeApi,
            properties: std::collections::HashMap::new(),
            content_role: ContentRole::default(),
        }];

        merger.merge_native_elements(&mut ctx, native);
        let btn = ctx.elements.iter().find(|e| e.id == "btn1").unwrap();
        // Original 0.95 should be preserved since it's higher
        assert_eq!(btn.confidence, 0.95);
        assert_eq!(btn.source, ContextSource::AccessibilityTree);
    }

    #[test]
    fn test_build_from_external_deduplicates_overlapping_duplicates() {
        let button_a = ContextElement {
            id: "dom:save-1".into(),
            label: Some("Save".into()),
            description: None,
            element_type: "button".into(),
            value: None,
            bounds: Some(Bounds { x: 100, y: 100, width: 120, height: 32 }),
            state: cel_accessibility::ElementState {
                visible: true,
                enabled: true,
                ..Default::default()
            },
            parent_id: None,
            actions: vec!["click".into()],
            confidence: 0.0,
            source: ContextSource::AccessibilityTree,
            content_role: ContentRole::Interactive,
            properties: std::collections::HashMap::from([
                ("css_selector".into(), "#save-primary".into()),
            ]),
        };
        let button_b = ContextElement {
            id: "dom:save-2".into(),
            label: Some("Save".into()),
            description: Some("Duplicate wrapper".into()),
            element_type: "button".into(),
            value: None,
            bounds: Some(Bounds { x: 101, y: 100, width: 120, height: 32 }),
            state: cel_accessibility::ElementState {
                visible: true,
                enabled: true,
                ..Default::default()
            },
            parent_id: None,
            actions: vec!["click".into()],
            confidence: 0.0,
            source: ContextSource::AccessibilityTree,
            content_role: ContentRole::Interactive,
            properties: std::collections::HashMap::from([
                ("css_selector".into(), "#save-primary span".into()),
                ("dom_id".into(), "save-primary".into()),
            ]),
        };

        let ctx = build_from_external(
            vec![button_a, button_b],
            vec![],
            "Browser".into(),
            "Test".into(),
        );

        let save_buttons: Vec<_> = ctx.elements.iter().filter(|el| el.label.as_deref() == Some("Save")).collect();
        assert_eq!(save_buttons.len(), 1, "Expected overlapping duplicate buttons to collapse to one");
    }

    #[test]
    fn test_build_from_external_drops_header_noise_but_keeps_row_scoped_action() {
        let row_id = "row:jamie";
        let elements = vec![
            ContextElement {
                id: "dom:header-more".into(),
                label: Some("More".into()),
                description: None,
                element_type: "button".into(),
                value: None,
                bounds: Some(Bounds { x: 1200, y: 24, width: 80, height: 28 }),
                state: cel_accessibility::ElementState {
                    visible: true,
                    enabled: true,
                    ..Default::default()
                },
                parent_id: None,
                actions: vec!["click".into()],
                confidence: 0.0,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::Interactive,
                properties: std::collections::HashMap::from([
                    ("css_selector".into(), "header .toolbar .more".into()),
                ]),
            },
            ContextElement {
                id: row_id.into(),
                label: None,
                description: None,
                element_type: "group".into(),
                value: None,
                bounds: Some(Bounds { x: 100, y: 200, width: 900, height: 48 }),
                state: cel_accessibility::ElementState {
                    visible: true,
                    enabled: true,
                    ..Default::default()
                },
                parent_id: None,
                actions: vec![],
                confidence: 0.0,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::Content,
                properties: std::collections::HashMap::new(),
            },
            ContextElement {
                id: "dom:name".into(),
                label: Some("Jamie Rodriguez".into()),
                description: None,
                element_type: "text".into(),
                value: None,
                bounds: Some(Bounds { x: 120, y: 210, width: 220, height: 24 }),
                state: cel_accessibility::ElementState {
                    visible: true,
                    enabled: true,
                    ..Default::default()
                },
                parent_id: Some(row_id.into()),
                actions: vec![],
                confidence: 0.0,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::Content,
                properties: std::collections::HashMap::new(),
            },
            ContextElement {
                id: "dom:role".into(),
                label: Some("Viewer".into()),
                description: None,
                element_type: "text".into(),
                value: None,
                bounds: Some(Bounds { x: 360, y: 210, width: 90, height: 24 }),
                state: cel_accessibility::ElementState {
                    visible: true,
                    enabled: true,
                    ..Default::default()
                },
                parent_id: Some(row_id.into()),
                actions: vec![],
                confidence: 0.0,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::Content,
                properties: std::collections::HashMap::new(),
            },
            ContextElement {
                id: "dom:remove".into(),
                label: Some("Remove".into()),
                description: None,
                element_type: "button".into(),
                value: None,
                bounds: Some(Bounds { x: 840, y: 206, width: 120, height: 28 }),
                state: cel_accessibility::ElementState {
                    visible: true,
                    enabled: true,
                    ..Default::default()
                },
                parent_id: Some(row_id.into()),
                actions: vec!["click".into()],
                confidence: 0.0,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::Interactive,
                properties: std::collections::HashMap::from([
                    ("css_selector".into(), ".user-row [data-action='remove']".into()),
                ]),
            },
        ];

        let ctx = build_from_external(elements, vec![], "Browser".into(), "Test".into());

        assert!(
            ctx.elements.iter().any(|el| el.id == "dom:remove"),
            "Expected row-scoped generic action to survive the noise filter",
        );
        assert!(
            !ctx.elements.iter().any(|el| el.id == "dom:header-more"),
            "Expected header chrome noise to be filtered out",
        );
    }

    #[test]
    fn test_context_timestamp_is_nonzero() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let mut merger = ContextMerger::new(stub);
        let ctx = merger.get_context();
        assert!(ctx.timestamp_ms > 0, "Context should have a real timestamp");
        // Should be a recent epoch-ms value (after 2020-01-01)
        assert!(ctx.timestamp_ms > 1_577_836_800_000);
    }

    #[test]
    fn test_flatten_a11y_tree_sets_dynamic_confidence() {
        let stub = Box::new(cel_accessibility::StubAccessibility);
        let mut merger = ContextMerger::new(stub);
        let ctx = merger.get_context();

        // All a11y elements should have confidence in valid range (0.60 base to ~0.90 max)
        for e in &ctx.elements {
            assert!(e.confidence >= 0.60, "Confidence {} too low for {}", e.confidence, e.id);
            assert!(e.confidence <= 0.95, "Confidence {} too high for {}", e.confidence, e.id);
            assert_eq!(e.source, ContextSource::AccessibilityTree);
        }
    }

    #[test]
    fn test_role_to_string_covers_all_roles() {
        // Ensure no role maps to empty string
        let roles = vec![
            ElementRole::Button, ElementRole::Input, ElementRole::Text,
            ElementRole::Window, ElementRole::List, ElementRole::ListItem,
            ElementRole::Menu, ElementRole::MenuItem, ElementRole::Checkbox,
            ElementRole::ComboBox, ElementRole::Table, ElementRole::TableRow,
            ElementRole::TableCell, ElementRole::Dialog, ElementRole::Tab,
            ElementRole::TabItem, ElementRole::RadioButton, ElementRole::Slider,
            ElementRole::ScrollBar, ElementRole::TreeView, ElementRole::TreeItem,
            ElementRole::Toolbar, ElementRole::StatusBar, ElementRole::Group,
            ElementRole::Image, ElementRole::Link,
            ElementRole::Custom("custom_widget".into()),
        ];

        for role in &roles {
            let s = role_to_string(role);
            assert!(!s.is_empty(), "role_to_string({:?}) returned empty string", role);
            assert!(!s.contains(' '), "role_to_string({:?}) contains spaces: '{}'", role, s);
        }
    }
}
