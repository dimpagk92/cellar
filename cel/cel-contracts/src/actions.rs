//! Action contract — what a planner can ask the runtime to do.
//!
//! Lives at the cortex/planner boundary so neither side has to depend on the
//! other to talk about actions.

use serde::{Deserialize, Deserializer, Serialize};

/// Deserialize a field that may be a string, object, array, or null.
/// LLMs sometimes return structured data where a plain string is expected.
fn deserialize_string_or_value<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Null => Ok(String::new()),
        other => Ok(other.to_string()),
    }
}

/// The action the planner wants to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlannedAction {
    Click {
        target_id: String,
    },
    Type {
        /// Optional: if provided, clicks the element first then types.
        /// If omitted, types into the currently focused element.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        text: String,
    },
    Key {
        key: String,
    },
    KeyCombo {
        keys: Vec<String>,
    },
    /// Set a value directly via the accessibility API (bypasses mouse/keyboard).
    /// More reliable than Type for form fields where settable=true.
    SetValue {
        target_id: String,
        value: String,
    },
    Scroll {
        dx: i32,
        dy: i32,
    },
    /// Drag from one element/position to another.
    Drag {
        from_target_id: String,
        to_target_id: String,
    },
    Wait {
        ms: u32,
    },
    Custom {
        adapter: String,
        action: String,
        #[serde(default)]
        params: serde_json::Value,
    },
    /// Extract: read data from the current page/screen without interaction.
    /// For read-only goals: "what are the prices?", "read the headlines", "what's on screen?"
    /// Returns the extracted data in the `data` field — no clicking or navigation needed.
    Extract {
        /// What data to extract (natural language description).
        #[serde(default, deserialize_with = "deserialize_string_or_value")]
        goal: String,
        /// The extracted data (filled by the planner from visible context).
        /// Optional: Gemini 2.5 Flash sometimes omits this field.
        #[serde(default, deserialize_with = "deserialize_string_or_value")]
        data: String,
    },
    /// Batch: execute multiple simple actions in sequence without re-planning.
    Batch {
        actions: Vec<PlannedAction>,
    },
    /// Natural language action — CEL resolves the instruction to the best matching element.
    Act {
        instruction: String,
    },
    /// Terminal: goal achieved. `evidence_ids` should cite element IDs
    /// from the current context that prove the goal was achieved.
    Done {
        #[serde(deserialize_with = "deserialize_string_or_value")]
        summary: String,
        #[serde(default)]
        evidence_ids: Vec<String>,
    },
    /// Terminal: cannot proceed.
    Fail {
        #[serde(deserialize_with = "deserialize_string_or_value")]
        reason: String,
    },
    /// Native accessibility action — more reliable than coordinate clicks for desktop apps.
    /// Uses macOS AXUIElementPerformAction under the hood.
    AxAction {
        target_id: String,
        /// The action to perform: "click" (AXPress), "activate" (AXConfirm),
        /// "increment", "decrement", "show_menu"
        action: String,
        /// Label hint for fallback element resolution. AX IDs are hashes
        /// that include bounds + depth and therefore change whenever the
        /// UI mutates between plan time and dispatch time. If the cortex
        /// can't find `target_id` in the live AX tree, it falls back to
        /// searching for the first visible element whose role matches
        /// `role_hint` (if provided) and whose label equals `label`.
        /// Planner must populate this from the same perception snapshot
        /// that produced the target_id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role_hint: Option<String>,
    },
    /// Activate (bring to front) a macOS application by name.
    /// Uses `open -a` under the hood — the most reliable app switching method.
    ActivateApp {
        app_name: String,
    },
    /// Select text by dragging from one coordinate to another.
    /// Used for text selection, highlighting, and marking tasks.
    Select {
        from_x: i32,
        from_y: i32,
        to_x: i32,
        to_y: i32,
    },
    /// Execute JavaScript in the focused browser tab via Chrome DevTools Protocol.
    /// Fastest way to interact with web pages: click elements, fill forms, extract data,
    /// dismiss cookie banners — all in a single action.
    CdpEval {
        /// JavaScript expression to evaluate in the page context.
        expression: String,
    },
    /// Convenience navigation action. LLMs gravitate toward `{"type":"navigate","url":"..."}`
    /// even when the prompt asks for cdp_eval, so accept it as a first-class variant and
    /// route it to the same reset_preferred_target + cdp_eval path inside the cortex.
    Navigate {
        #[serde(alias = "href", alias = "to")]
        url: String,
    },
    /// No-op: LLM sometimes puts notebook_writes inside the actions array.
    /// This variant absorbs that mistake gracefully instead of causing a parse error.
    /// The actual notebook_writes are processed from the top-level PlannedStep field.
    #[serde(alias = "notebook_write")]
    NotebookWrites {
        #[serde(default)]
        key: String,
        #[serde(default)]
        value: String,
        #[serde(default)]
        category: String,
    },
    /// Declarative extraction with selector fallbacks.
    ///
    /// Replaces the "LLM hand-writes `document.querySelector(...)` in a
    /// loop" failure mode. Runtime tries each selector in order via
    /// CDP `Runtime.evaluate`, parses according to `parse_as`, and on
    /// first success writes the value into `shared_memory[name]`. The
    /// planner supplies the scenario-specific selector knowledge as
    /// parameters; the retry/parse machinery is generic.
    ///
    /// Contract with the runner: consecutive failures for the same
    /// `name` (across turns) accumulate toward an auto-null cutoff
    /// (see the stall/retry budget in `canonical_runner.rs`). After
    /// the cutoff the runtime records `shared_memory[name] = null` so
    /// the LLM stops polishing a field that the page does not surface.
    #[serde(alias = "extract_with_fallbacks", alias = "extract_declarative")]
    ExtractWithFallback {
        /// Logical name for this extraction target (e.g. `"btc_price"`).
        /// Written into `shared_memory` on success, and used by the
        /// runner to group consecutive failures for retry budgeting.
        name: String,
        /// CSS selector or JS expression candidates, tried in order.
        /// An entry may be either a plain CSS selector (runtime wraps
        /// it into `document.querySelector(SEL)?.textContent`) or a
        /// full JS expression starting with `function` or a recognized
        /// prefix — the runtime auto-detects.
        selectors: Vec<String>,
        /// How to parse the raw string yielded by the first matching
        /// selector. One of: `"text"`, `"float"`, `"int"`, `"html"`.
        /// Unknown values fall back to `"text"`.
        #[serde(default = "default_parse_as", alias = "parse", alias = "as")]
        parse_as: String,
    },
    /// Deterministic spreadsheet cell writes via AppleScript (Numbers).
    ///
    /// Replaces the flaky keystroke recipe (`activate_app → key(arrows)
    /// → key(Delete) → type → key(Return)`) with one atomic operation
    /// against the document model. The keystroke recipe produced
    /// concatenated garbage values, duplicated headers, and values
    /// landing in the wrong cells whenever an intermediate step got
    /// perturbed by focus drift or AX tree lag. `WriteCells` sidesteps
    /// the entire UI event loop.
    ///
    /// Batch-shaped because the AppleScript spawn cost amortizes across
    /// many cells — single-cell callers pass a length-1 `writes` vector.
    #[serde(alias = "write_cell")]
    WriteCells {
        /// Target app. Currently only `"Numbers"` is implemented; other
        /// values produce a clean runtime error so the planner can pivot.
        #[serde(default = "default_spreadsheet_app")]
        app: String,
        /// Optional sheet name. `None` = first sheet of first document.
        #[serde(default)]
        sheet: Option<String>,
        /// Optional table name. `None` = first table of selected sheet.
        #[serde(default)]
        table: Option<String>,
        /// Writes to apply, in order.
        writes: Vec<CellWrite>,
        /// When true, the runtime reads each cell back after writing
        /// and includes the readback in the step result's `data` field.
        /// Recommended: keep `true` — verification is cheap (same
        /// AppleScript call) and catches Numbers' value coercions.
        #[serde(default = "default_true")]
        verify: bool,
    },
    /// Deterministic spreadsheet cell reads via AppleScript (Numbers).
    ///
    /// Use this when the agent needs spreadsheet truth from the app
    /// model instead of relying on the accessibility tree to expose
    /// the values. Particularly useful for Numbers, whose AX surface
    /// often only exposes the focused cell or formula-bar content.
    #[serde(alias = "read_cell")]
    ReadCells {
        /// Target app. Currently only `"Numbers"` is implemented.
        #[serde(default = "default_spreadsheet_app")]
        app: String,
        /// Optional sheet name. `None` = first sheet of first document.
        #[serde(default)]
        sheet: Option<String>,
        /// Optional table name. `None` = first table of selected sheet.
        #[serde(default)]
        table: Option<String>,
        /// Cells to read, in order.
        #[serde(alias = "refs", alias = "cells", alias = "addresses")]
        cell_refs: Vec<String>,
    },
}

/// One cell write inside a [`PlannedAction::WriteCells`] batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CellWrite {
    /// A1-notation cell reference, e.g. `"B2"`, `"AA17"`.
    #[serde(alias = "ref", alias = "cell", alias = "address")]
    pub cell_ref: String,
    /// Value to write. Pass raw numeric strings (`"108432.50"`, not
    /// `"$108,432.50"`); Numbers formats per the cell's display
    /// format. Text values pass through unchanged.
    pub value: String,
}

fn default_spreadsheet_app() -> String {
    "Numbers".into()
}

fn default_true() -> bool {
    true
}

fn default_parse_as() -> String {
    "text".into()
}

impl PlannedAction {
    /// Target element IDs this action depends on, if any. Used by the runner
    /// to pre-validate that planned elements still exist in the fresh Cortex
    /// context before dispatch — missing targets trigger a replan instead of
    /// silent misfire.
    ///
    /// Returns an empty slice for actions with no element target (coordinate
    /// scrolls, key events, `CdpEval`, terminal actions). `Drag` returns two
    /// IDs (from + to). `Batch` recurses — every sub-action's targets are
    /// collected flat, so if any one is missing the whole batch is aborted
    /// before any side-effect lands. Sub-batches inside batches collapse the
    /// same way.
    pub fn target_ids(&self) -> Vec<&str> {
        match self {
            Self::Click { target_id }
            | Self::SetValue { target_id, .. }
            | Self::AxAction { target_id, .. } => vec![target_id.as_str()],
            Self::Type {
                target_id: Some(id),
                ..
            } => vec![id.as_str()],
            Self::Drag {
                from_target_id,
                to_target_id,
            } => vec![from_target_id.as_str(), to_target_id.as_str()],
            Self::Batch { actions } => actions.iter().flat_map(|a| a.target_ids()).collect(),
            // No element-level targets:
            Self::Type {
                target_id: None, ..
            }
            | Self::Key { .. }
            | Self::KeyCombo { .. }
            | Self::Scroll { .. }
            | Self::Wait { .. }
            | Self::Custom { .. }
            | Self::Extract { .. }
            | Self::Act { .. }
            | Self::Done { .. }
            | Self::Fail { .. }
            | Self::ActivateApp { .. }
            | Self::Select { .. }
            | Self::CdpEval { .. }
            | Self::Navigate { .. }
            | Self::NotebookWrites { .. }
            | Self::WriteCells { .. }
            | Self::ReadCells { .. }
            | Self::ExtractWithFallback { .. } => vec![],
        }
    }
}

#[cfg(test)]
mod target_ids_tests {
    use super::*;

    #[test]
    fn click_returns_its_target() {
        let a = PlannedAction::Click {
            target_id: "a11y:42".into(),
        };
        assert_eq!(a.target_ids(), vec!["a11y:42"]);
    }

    #[test]
    fn type_without_target_returns_empty() {
        let a = PlannedAction::Type {
            target_id: None,
            text: "hi".into(),
        };
        assert!(a.target_ids().is_empty());
    }

    #[test]
    fn drag_returns_both_endpoints() {
        let a = PlannedAction::Drag {
            from_target_id: "a11y:1".into(),
            to_target_id: "a11y:2".into(),
        };
        assert_eq!(a.target_ids(), vec!["a11y:1", "a11y:2"]);
    }

    #[test]
    fn batch_flattens_sub_action_targets() {
        let a = PlannedAction::Batch {
            actions: vec![
                PlannedAction::CdpEval {
                    expression: "1".into(),
                },
                PlannedAction::Click {
                    target_id: "a11y:ghost".into(),
                },
                PlannedAction::SetValue {
                    target_id: "a11y:input".into(),
                    value: "v".into(),
                },
            ],
        };
        assert_eq!(a.target_ids(), vec!["a11y:ghost", "a11y:input"]);
    }

    #[test]
    fn nested_batches_flatten() {
        let a = PlannedAction::Batch {
            actions: vec![PlannedAction::Batch {
                actions: vec![PlannedAction::Click {
                    target_id: "a11y:deep".into(),
                }],
            }],
        };
        assert_eq!(a.target_ids(), vec!["a11y:deep"]);
    }

    #[test]
    fn cdp_eval_and_terminals_have_no_targets() {
        assert!(PlannedAction::CdpEval {
            expression: "x".into()
        }
        .target_ids()
        .is_empty());
        assert!(PlannedAction::Done {
            summary: "ok".into(),
            evidence_ids: vec![]
        }
        .target_ids()
        .is_empty());
        assert!(PlannedAction::Wait { ms: 100 }.target_ids().is_empty());
    }

    #[test]
    fn extract_with_fallback_round_trips() {
        let raw = r#"{"type":"extract_with_fallback","name":"btc_price",
            "selectors":["fin-streamer[data-field='regularMarketPrice']",
                         "[data-test='qsp-price']"],
            "parse_as":"float"}"#;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match &a {
            PlannedAction::ExtractWithFallback {
                name,
                selectors,
                parse_as,
            } => {
                assert_eq!(name, "btc_price");
                assert_eq!(selectors.len(), 2);
                assert_eq!(parse_as, "float");
            }
            _ => panic!("expected ExtractWithFallback"),
        }
        // Has no element-level targets.
        assert!(a.target_ids().is_empty());
    }

    #[test]
    fn extract_with_fallback_defaults_parse_as_to_text() {
        let raw = r#"{"type":"extract_with_fallback","name":"title",
            "selectors":["h1"]}"#;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match a {
            PlannedAction::ExtractWithFallback { parse_as, .. } => {
                assert_eq!(parse_as, "text");
            }
            _ => panic!("expected ExtractWithFallback"),
        }
    }

    #[test]
    fn extract_with_fallback_accepts_parse_alias() {
        let raw = r#"{"type":"extract_with_fallback","name":"n",
            "selectors":["a"],"parse":"int"}"#;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match a {
            PlannedAction::ExtractWithFallback { parse_as, .. } => {
                assert_eq!(parse_as, "int");
            }
            _ => panic!("expected ExtractWithFallback"),
        }
    }

    #[test]
    fn read_cells_accepts_cells_alias_and_defaults_app() {
        let raw = r#"{"type":"read_cells","cells":["A1","B2"]}"#;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match a {
            PlannedAction::ReadCells { app, cell_refs, .. } => {
                assert_eq!(app, "Numbers");
                assert_eq!(cell_refs, vec!["A1", "B2"]);
            }
            _ => panic!("expected ReadCells"),
        }
    }
}
