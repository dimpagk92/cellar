use crate::element::{ContentRole, ContextElement, ContextSnapshot, ContextSource, FocusedContext};

/// High-level status for streams that contributed to a context snapshot.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StreamStatus {
    pub accessibility: bool,
    pub display: bool,
    pub network: bool,
    pub signals: bool,
    pub vision: bool,
    pub audio_capture: bool,
}

/// Generic source contribution accepted by [`ContextMerger`].
#[derive(Debug, Clone, Default)]
pub struct ContextContribution {
    pub source_name: String,
    pub elements: Vec<ContextElement>,
    pub app: Option<String>,
    pub window: Option<String>,
    pub stream_status: StreamStatus,
}

impl ContextContribution {
    pub fn new(source_name: impl Into<String>, elements: Vec<ContextElement>) -> Self {
        Self {
            source_name: source_name.into(),
            elements,
            app: None,
            window: None,
            stream_status: StreamStatus::default(),
        }
    }

    pub fn with_app(mut self, app: impl Into<String>) -> Self {
        self.app = Some(app.into());
        self
    }

    pub fn with_window(mut self, window: impl Into<String>) -> Self {
        self.window = Some(window.into());
        self
    }

    pub fn with_stream_status(mut self, status: StreamStatus) -> Self {
        self.stream_status = status;
        self
    }
}

/// Generic merger for already-normalized context contributions.
///
/// `cel-context` deliberately does not know how to read accessibility, CDP,
/// OCR, network, logs, traces, metrics, tickets, or other streams. Those
/// sources should emit [`ContextElement`]s / [`ContextSnapshot`]s and feed them
/// here.
#[derive(Debug, Clone)]
pub struct ContextMerger {
    contributions: Vec<ContextContribution>,
    default_app: String,
    default_window: String,
}

impl Default for ContextMerger {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextMerger {
    pub fn new() -> Self {
        Self {
            contributions: Vec::new(),
            default_app: String::new(),
            default_window: String::new(),
        }
    }

    /// Compatibility constructor for runtimes that still wire concrete source
    /// crates externally. `cel-context` ignores the source object and only
    /// merges already-normalized contributions.
    pub fn with_all<A, D, N>(_accessibility: A, _display: D, _network: N) -> Self {
        Self::new()
    }

    /// Compatibility constructor for a runtime-provided display source.
    pub fn with_display<A, D>(_accessibility: A, _display: D) -> Self {
        Self::new()
    }

    /// Compatibility hook for a runtime-provided signal source.
    pub fn with_signals<S>(self, _signals: S) -> Self {
        self
    }

    /// Compatibility hook for a runtime-provided vision source.
    pub fn with_vision<V>(self, _vision: V) -> Self {
        self
    }

    /// Compatibility hook for a runtime-provided async runtime handle.
    pub fn with_runtime<R>(self, _runtime: R) -> Self {
        self
    }

    /// Compatibility hook for a runtime-provided screenshot fallback.
    pub fn with_cdp_screenshot_fallback<F, T>(self, _fallback: F) -> Self
    where
        F: Fn() -> Option<T> + Send + Sync + 'static,
    {
        self
    }

    /// Compatibility hook retained until the live runtime owns OCR fallback.
    pub fn run_ocr_fallback(&mut self, _existing: &[ContextElement]) -> Vec<ContextElement> {
        Vec::new()
    }

    /// Network events are carried on [`ContextSnapshot`], not owned by the merger.
    pub fn recent_network_events(&self) -> &[crate::ConnectionEvent] {
        &[]
    }

    pub fn with_defaults(mut self, app: impl Into<String>, window: impl Into<String>) -> Self {
        self.default_app = app.into();
        self.default_window = window.into();
        self
    }

    pub fn push(&mut self, contribution: ContextContribution) {
        self.contributions.push(contribution);
    }

    pub fn extend(&mut self, contributions: impl IntoIterator<Item = ContextContribution>) {
        self.contributions.extend(contributions);
    }

    pub fn stream_status(&self) -> StreamStatus {
        self.contributions
            .iter()
            .fold(StreamStatus::default(), |mut acc, c| {
                acc.accessibility |= c.stream_status.accessibility;
                acc.display |= c.stream_status.display;
                acc.network |= c.stream_status.network;
                acc.signals |= c.stream_status.signals;
                acc.vision |= c.stream_status.vision;
                acc.audio_capture |= c.stream_status.audio_capture;
                acc
            })
    }

    pub fn build(&self) -> ContextSnapshot {
        let app = self
            .contributions
            .iter()
            .find_map(|c| c.app.clone())
            .unwrap_or_else(|| self.default_app.clone());
        let window = self
            .contributions
            .iter()
            .find_map(|c| c.window.clone())
            .unwrap_or_else(|| self.default_window.clone());

        let elements = self
            .contributions
            .iter()
            .flat_map(|c| c.elements.clone())
            .collect();

        build_from_external(elements, Vec::new(), app, window)
    }

    /// Backwards-compatible alias for callers that think of merging as a read.
    pub fn get_context(&mut self) -> ContextSnapshot {
        self.build()
    }

    pub fn get_context_focused(&mut self, element_id: &str) -> Option<FocusedContext> {
        let context = self.build();
        let element = context
            .elements
            .iter()
            .find(|element| element.id == element_id)?
            .clone();
        let subtree = context
            .elements
            .iter()
            .filter(|candidate| candidate.parent_id.as_deref() == Some(element_id))
            .cloned()
            .collect();
        let ancestor_path = element.build_ancestor_path(&context.elements);

        Some(FocusedContext {
            element,
            subtree,
            ancestor_path,
        })
    }
}

/// Convert a WAI-ARIA / DOM role into CEL's normalized element type strings.
pub fn aria_role_to_cel_type(role: &str) -> &'static str {
    match role {
        "button" => "button",
        "link" => "link",
        "textbox" | "searchbox" => "input",
        "checkbox" => "checkbox",
        "radio" => "radio_button",
        "combobox" | "listbox" => "combobox",
        "option" => "list_item",
        "menuitem" => "menu_item",
        "tab" => "tab_item",
        "slider" => "slider",
        "heading" => "heading",
        "img" | "image" => "image",
        "table" => "table",
        "row" => "table_row",
        "cell" | "gridcell" => "table_cell",
        "dialog" => "dialog",
        "navigation" | "banner" | "contentinfo" => "group",
        _ => "unknown",
    }
}

pub fn assign_default_actions(element_type: &str) -> Vec<String> {
    match element_type {
        "button" | "link" | "checkbox" | "radio_button" | "menu_item" | "tab_item"
        | "list_item" | "tree_item" => vec!["click".into()],
        "input" | "textarea" | "textfield" | "searchfield" => {
            vec!["focus".into(), "set_value".into()]
        }
        "combobox" | "select" => vec!["click".into(), "select".into()],
        "slider" => vec!["set_value".into()],
        _ => Vec::new(),
    }
}

pub fn build_from_external(
    mut elements: Vec<ContextElement>,
    http_events: Vec<crate::HttpEvent>,
    app: String,
    window: String,
) -> ContextSnapshot {
    for e in &mut elements {
        e.element_type = aria_role_to_cel_type(&e.element_type).to_string();
        if e.actions.is_empty() {
            e.actions = assign_default_actions(&e.element_type);
        }
        e.content_role = crate::classify_content_role(&e.element_type, &e.actions, &e.state);
        e.confidence = score_element_confidence(e);
    }

    elements.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    ContextSnapshot {
        app,
        window,
        elements,
        network_events: Vec::new(),
        http_events,
        timestamp_ms: now_ms(),
        screen_width: None,
        screen_height: None,
        clipboard: None,
        window_list: Vec::new(),
        audio: None,
        power: None,
        running_apps: Vec::new(),
        recent_files: Vec::new(),
        transcripts: Vec::new(),
    }
}

pub fn score_element_confidence(element: &ContextElement) -> f64 {
    let mut score: f64 = match element.source {
        ContextSource::NativeApi | ContextSource::Cdp => 0.95,
        ContextSource::AccessibilityTree => 0.85,
        ContextSource::External => 0.8,
        ContextSource::Ocr => 0.75,
        ContextSource::Vision => 0.65,
        ContextSource::Merged => 0.9,
    };

    if element
        .label
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        score += 0.03;
    }
    if element.bounds.is_some() {
        score += 0.02;
    }
    if is_actionable_type(&element.element_type) && !element.actions.is_empty() {
        score += 0.03;
    }
    if matches!(element.content_role, ContentRole::Content) {
        score -= 0.02;
    }

    score.clamp(0.0, 0.99)
}

pub fn is_actionable_type(element_type: &str) -> bool {
    matches!(
        element_type,
        "button"
            | "input"
            | "textfield"
            | "textarea"
            | "searchfield"
            | "link"
            | "checkbox"
            | "radio_button"
            | "combobox"
            | "select"
            | "menu_item"
            | "tab_item"
            | "slider"
            | "list_item"
            | "tree_item"
    )
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bounds, ElementState};
    use std::collections::HashMap;

    fn element(id: &str, source: ContextSource) -> ContextElement {
        ContextElement {
            id: id.into(),
            label: Some("Submit".into()),
            description: None,
            element_type: "button".into(),
            value: None,
            bounds: Some(Bounds {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            }),
            state: ElementState::default(),
            parent_id: None,
            actions: Vec::new(),
            confidence: 0.0,
            source,
            content_role: ContentRole::Interactive,
            properties: HashMap::new(),
        }
    }

    #[test]
    fn external_elements_are_normalized() {
        let ctx = build_from_external(
            vec![element("a", ContextSource::NativeApi)],
            Vec::new(),
            "App".into(),
            "Window".into(),
        );

        assert_eq!(ctx.app, "App");
        assert_eq!(ctx.window, "Window");
        assert_eq!(ctx.elements[0].actions, vec!["click"]);
        assert!(ctx.elements[0].confidence > 0.9);
    }

    #[test]
    fn merger_combines_contributions() {
        let mut merger = ContextMerger::new().with_defaults("App", "Window");
        merger.push(ContextContribution::new(
            "native",
            vec![element("a", ContextSource::NativeApi)],
        ));
        merger.push(ContextContribution::new(
            "ocr",
            vec![element("b", ContextSource::Ocr)],
        ));

        let ctx = merger.build();
        assert_eq!(ctx.elements.len(), 2);
    }
}
