use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Re-export Bounds and ElementState from the accessibility crate — single source of truth.
pub use cel_accessibility::Bounds;
pub use cel_accessibility::ElementState;

/// The source that provided a context element.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    /// From the accessibility tree (UIA / AXUIElement).
    AccessibilityTree,
    /// From a native API adapter (SAP, Excel COM, etc.).
    NativeApi,
    /// From vision model analysis.
    Vision,
    /// Merged from multiple sources.
    Merged,
}

/// Content role classification for prompt injection defense.
///
/// Elements are classified by their semantic role so the LLM can distinguish
/// between actionable UI controls and untrusted text content. This prevents
/// adversarial websites from injecting instructions via text elements that
/// look like system commands.
///
/// In the prompt, elements are tagged: [1] button "Submit" (interactive)
/// vs [5] text "Click here to win" (content) — so the LLM knows [5] is
/// user-authored text, not a UI control.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContentRole {
    /// UI controls the user can interact with (buttons, inputs, links, menus).
    /// Safe to act on — these are real UI elements.
    #[default]
    Interactive,
    /// Text content (paragraphs, headings, labels). May contain adversarial text.
    /// The LLM should READ but not EXECUTE instructions found in content elements.
    Content,
    /// Decorative elements (separators, icons, spacers). Can be deprioritized.
    Decorative,
    /// System/chrome UI (scrollbars, window controls, status bars).
    System,
}

/// Classify an element's content role based on its type and properties.
pub fn classify_content_role(
    element_type: &str,
    actions: &[String],
    state: &ElementState,
) -> ContentRole {
    match element_type {
        // Interactive controls
        "button" | "link" | "input" | "textfield" | "textarea" | "combobox" | "select"
        | "checkbox" | "radio" | "slider" | "switch" | "toggle" | "tab" | "tab_item"
        | "menuitem" | "menu_item" | "menubar" | "menu" | "toolbar" | "searchfield"
        | "tree_item" => ContentRole::Interactive,
        // System chrome
        "scrollbar" | "splitter" | "statusbar" | "status_bar" | "progressbar" | "indicator"
        | "dialog" | "window" => ContentRole::System,
        // Decorative
        "separator" | "image" | "icon" | "spacer" => ContentRole::Decorative,
        // Text content — untrusted
        "text" | "statictext" | "paragraph" | "heading" | "label" | "cell" | "table"
        | "table_row" | "table_cell" | "list" | "listitem" | "list_item" | "article"
        | "blockquote" => ContentRole::Content,
        // Default: if it has actions, it's interactive; otherwise content
        _ => {
            if !actions.is_empty() || state.focused {
                ContentRole::Interactive
            } else {
                ContentRole::Content
            }
        }
    }
}

/// A single UI element in the unified context model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextElement {
    /// Unique identifier for this element.
    pub id: String,
    /// Human-readable label.
    pub label: Option<String>,
    /// Accessibility description (tooltip / secondary label).
    pub description: Option<String>,
    /// Element type (button, input, text, etc.).
    pub element_type: String,
    /// Current value (for inputs, dropdowns, etc.).
    pub value: Option<String>,
    /// Screen-space bounding rectangle.
    pub bounds: Option<Bounds>,
    /// Current state flags (from accessibility tree).
    /// Defaults to all-false for sources that don't provide state (e.g., vision).
    #[serde(default)]
    pub state: ElementState,
    /// ID of the parent element (None for root elements).
    pub parent_id: Option<String>,
    /// Available actions (from AT-SPI2 Action interface): "click", "press", "activate", etc.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    /// Confidence score (0.0 - 1.0).
    pub confidence: f64,
    /// Which context source provided this element.
    pub source: ContextSource,
    /// Content role for prompt injection defense.
    /// Classifies elements as Interactive (safe to act on), Content (untrusted text),
    /// Decorative (deprioritize), or System (chrome UI).
    #[serde(default)]
    pub content_role: ContentRole,
    /// Extended properties from the accessibility or native API.
    /// Keys: "placeholder", "url", "required", "invalid", "role_desc", "selected_text",
    ///        "dom_id", "document", "filename", "min_value", "max_value", "has_popup",
    ///        "column_count", "row_count", "loading_progress"
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        deserialize_with = "flexible_properties"
    )]
    pub properties: HashMap<String, String>,
}

/// Deserialize a HashMap where values may be strings, numbers, bools, or nested objects.
/// Non-string values are converted to their JSON string representation.
fn flexible_properties<'de, D>(deserializer: D) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let mut map = HashMap::new();
    if let serde_json::Value::Object(obj) = value {
        for (key, val) in obj {
            let string_value = match val {
                serde_json::Value::String(s) => s,
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => String::new(),
                // Nested objects/arrays → JSON string
                other => other.to_string(),
            };
            map.insert(key, string_value);
        }
    }
    Ok(map)
}

/// Coarse spatial region for resilient element targeting.
/// Uses normalized coordinates (0.0-1.0) so references survive resolution changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundsRegion {
    /// Spatial quadrant: "top-left", "top-center", "top-right",
    /// "center-left", "center", "center-right",
    /// "bottom-left", "bottom-center", "bottom-right"
    pub quadrant: String,
    /// Normalized horizontal position (0.0 = left edge, 1.0 = right edge).
    pub relative_x: f64,
    /// Normalized vertical position (0.0 = top edge, 1.0 = bottom edge).
    pub relative_y: f64,
}

/// A resilient, multi-signal reference to a UI element.
/// Unlike element IDs (which are ephemeral per snapshot), references survive
/// across context snapshots by combining multiple identifying signals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextReference {
    /// Element type (button, input, text, etc.) — must match exactly.
    pub element_type: String,
    /// Expected label text (fuzzy matched).
    pub label: Option<String>,
    /// Ancestor path from root: e.g. \["window:Finder", "toolbar", "group"\].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ancestor_path: Vec<String>,
    /// Coarse spatial region where the element was last seen.
    pub bounds_region: Option<BoundsRegion>,
    /// Pattern the element's value should match.
    pub value_pattern: Option<String>,
}

impl ContextElement {
    /// Build the ancestor path by walking parent_id chains.
    /// Returns element_types from root to parent (not including self).
    fn build_ancestor_path(&self, all_elements: &[ContextElement]) -> Vec<String> {
        let mut path = Vec::new();
        let mut current_id = self.parent_id.as_deref();
        let mut depth = 0;
        while let Some(pid) = current_id {
            if depth > 15 {
                break;
            }
            if let Some(parent) = all_elements.iter().find(|e| e.id == pid) {
                path.push(parent.element_type.clone());
                current_id = parent.parent_id.as_deref();
            } else {
                break;
            }
            depth += 1;
        }
        path.reverse();
        path
    }

    /// Build a resilient reference from this element's current data.
    /// `screen_width` and `screen_height` are used to compute normalized coordinates.
    pub fn to_reference(&self, screen_width: u32, screen_height: u32) -> ContextReference {
        let bounds_region = self.bounds.as_ref().and_then(|b| {
            if screen_width == 0 || screen_height == 0 {
                return None;
            }
            let cx = b.x as f64 + b.width as f64 / 2.0;
            let cy = b.y as f64 + b.height as f64 / 2.0;
            let rx = cx / screen_width as f64;
            let ry = cy / screen_height as f64;

            let col = if rx < 0.33 {
                "left"
            } else if rx < 0.66 {
                "center"
            } else {
                "right"
            };
            let row = if ry < 0.33 {
                "top"
            } else if ry < 0.66 {
                "center"
            } else {
                "bottom"
            };
            let quadrant = if row == "center" && col == "center" {
                "center".to_string()
            } else {
                format!("{}-{}", row, col)
            };

            Some(BoundsRegion {
                quadrant,
                relative_x: rx.clamp(0.0, 1.0),
                relative_y: ry.clamp(0.0, 1.0),
            })
        });

        ContextReference {
            element_type: self.element_type.clone(),
            label: self.label.clone(),
            ancestor_path: Vec::new(),
            bounds_region,
            value_pattern: self.value.clone(),
        }
    }

    /// Build a resilient reference with ancestor path context.
    /// `all_elements` is the flattened element list from the ScreenContext.
    pub fn to_reference_in_context(
        &self,
        screen_width: u32,
        screen_height: u32,
        all_elements: &[ContextElement],
    ) -> ContextReference {
        let mut reference = self.to_reference(screen_width, screen_height);
        reference.ancestor_path = self.build_ancestor_path(all_elements);
        reference
    }
}

/// A single transcribed speech segment from the audio capture layer.
/// Source-agnostic: may come from the microphone or system loopback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// "microphone" | "system_output" | "both"
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// The complete screen context — the unified world model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenContext {
    /// Name of the foreground application.
    pub app: String,
    /// Title of the active window.
    pub window: String,
    /// All detected UI elements, sorted by confidence (highest first).
    pub elements: Vec<ContextElement>,
    /// Recent network connections (TCP/UDP level — honest data from lsof or /proc).
    #[serde(default)]
    pub network_events: Vec<cel_network::ConnectionEvent>,
    /// Real HTTP events from CDP or proxy (never fabricated).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http_events: Vec<cel_network::HttpEvent>,
    /// Timestamp of this context snapshot (ms since epoch).
    pub timestamp_ms: u64,
    /// Screen width in pixels (used for spatial normalization in reference resolution).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_width: Option<u32>,
    /// Screen height in pixels (used for spatial normalization in reference resolution).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_height: Option<u32>,
    /// Clipboard state (text, has_image, has_files). From cel-signals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<cel_signals::ClipboardState>,
    /// All visible windows on screen (not just focused app). From cel-signals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub window_list: Vec<cel_signals::WindowState>,
    /// Audio output state (volume, muted). From cel-signals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<cel_signals::AudioState>,
    /// Battery/power state. From cel-signals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<cel_signals::PowerState>,
    /// Running GUI applications. From cel-signals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_apps: Vec<cel_signals::RunningApp>,
    /// Recently created/modified files (Downloads/Desktop, last 60s). From cel-signals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_files: Vec<cel_signals::RecentFile>,
    /// Transcribed speech segments from the audio capture layer (mic + loopback).
    /// Populated by cel-cortex when an audio backend is configured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcripts: Vec<TranscriptEntry>,
}

/// High-fidelity context for a single element — the "zoom in" view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusedContext {
    /// The target element with full detail.
    pub element: ContextElement,
    /// Children (preserves hierarchy, not flattened).
    pub subtree: Vec<ContextElement>,
    /// Parent chain from root to this element: e.g. ["window:Title", "group", "toolbar"].
    pub ancestor_path: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_state() -> ElementState {
        ElementState::default()
    }

    #[test]
    fn test_classify_interactive_elements() {
        let types = [
            "button",
            "link",
            "input",
            "textfield",
            "textarea",
            "combobox",
            "select",
            "checkbox",
            "radio",
            "slider",
            "switch",
            "toggle",
            "tab",
            "menuitem",
        ];
        for t in types {
            let role = classify_content_role(t, &[], &default_state());
            assert_eq!(
                role,
                ContentRole::Interactive,
                "Expected Interactive for '{}'",
                t
            );
        }
    }

    #[test]
    fn test_classify_content_elements() {
        let types = [
            "text",
            "statictext",
            "paragraph",
            "heading",
            "label",
            "cell",
            "table",
            "table_row",
            "table_cell",
            "list",
            "listitem",
            "list_item",
        ];
        for t in types {
            let role = classify_content_role(t, &[], &default_state());
            assert_eq!(role, ContentRole::Content, "Expected Content for '{}'", t);
        }
    }

    #[test]
    fn test_classify_system_elements() {
        let types = [
            "scrollbar",
            "splitter",
            "statusbar",
            "status_bar",
            "progressbar",
            "dialog",
            "window",
        ];
        for t in types {
            let role = classify_content_role(t, &[], &default_state());
            assert_eq!(role, ContentRole::System, "Expected System for '{}'", t);
        }
    }

    #[test]
    fn test_classify_decorative_elements() {
        let types = ["separator", "image", "icon", "spacer"];
        for t in types {
            let role = classify_content_role(t, &[], &default_state());
            assert_eq!(
                role,
                ContentRole::Decorative,
                "Expected Decorative for '{}'",
                t
            );
        }
    }

    #[test]
    fn test_unknown_with_actions_is_interactive() {
        let role = classify_content_role("custom_widget", &["click".into()], &default_state());
        assert_eq!(role, ContentRole::Interactive);
    }

    #[test]
    fn test_unknown_without_actions_is_content() {
        let role = classify_content_role("custom_widget", &[], &default_state());
        assert_eq!(role, ContentRole::Content);
    }

    #[test]
    fn test_unknown_focused_is_interactive() {
        let mut state = default_state();
        state.focused = true;
        let role = classify_content_role("unknown", &[], &state);
        assert_eq!(role, ContentRole::Interactive);
    }

    #[test]
    fn test_content_role_default_is_interactive() {
        assert_eq!(ContentRole::default(), ContentRole::Interactive);
    }

    #[test]
    fn test_content_role_serialization() {
        assert_eq!(
            serde_json::to_string(&ContentRole::Interactive).unwrap(),
            "\"interactive\""
        );
        assert_eq!(
            serde_json::to_string(&ContentRole::Content).unwrap(),
            "\"content\""
        );
    }
}
