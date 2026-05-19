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

/// What the runtime should observe after an action lands to confirm the
/// side-effect actually materialised.
///
/// Cortex polls the page (via CDP `Runtime.evaluate`) until the
/// expectation holds or the timeout fires. The result is reported back
/// in the action's `ActionResult` so the planner sees not just
/// "dispatched ok" but "dispatched ok AND observed the expected change".
///
/// Closes the "click reported ok but page didn't react" gap:
/// `e.preventDefault()` from a validation handler, a remounted DOM node
/// the click landed on but is no longer wired up, an animation that
/// swallowed the click — all previously reported `ok` and forced the
/// planner to verify state from screenshots after the fact. With
/// `expect_after` the runtime knows immediately the side-effect didn't
/// materialise and surfaces an `EffectMissing` error to the planner.
///
/// All variants carry a `timeout_ms` with a sensible default
/// (2_000 ms) — long enough for animations / debounced handlers, short
/// enough not to add real latency on the happy path (most expectations
/// resolve in one CDP round-trip when the action actually fired).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectExpectation {
    /// A new element matching `selector` becomes visible
    /// (`offsetParent !== null`). Use for "after submit, success
    /// message appears", "after open, modal appears" patterns.
    SelectorAppears {
        selector: String,
        #[serde(default = "default_effect_timeout_ms")]
        timeout_ms: u64,
    },
    /// An element matching `selector` becomes invisible (detached or
    /// `offsetParent === null`). Use for "after close, modal disappears",
    /// "after delete, row goes away" patterns.
    SelectorDisappears {
        selector: String,
        #[serde(default = "default_effect_timeout_ms")]
        timeout_ms: u64,
    },
    /// An element matching `selector` contains the given text. Use for
    /// state changes like "button label flips from 'Approve' to
    /// 'Approved ✓'", or "row gains an 'Approved' status cell".
    SelectorTextContains {
        selector: String,
        substring: String,
        #[serde(default = "default_effect_timeout_ms")]
        timeout_ms: u64,
    },
    /// The page DOM changed in any meaningful way after the action.
    /// Compares a "before" snapshot (captured at dispatch entry) to a
    /// "after" snapshot polled until either differs or the timeout
    /// fires. The snapshot is small + cheap: visible text length,
    /// interactive element count, and current URL.
    ///
    /// Use when the post-state isn't a single named selector but you
    /// know the action SHOULD cause SOME visible change — e.g.:
    ///   • a delete button that removes a row (count decreases),
    ///   • a tab switch that swaps the entire content panel,
    ///   • a submit that navigates to a thank-you page (URL changes),
    ///   • a "load more" that appends results (text length grows).
    ///
    /// Strictly weaker than `SelectorAppears`/`Disappears`/`TextContains`
    /// — those tell you EXACTLY what should change, this just tells
    /// you SOMETHING did. Use the selector-based variants when you
    /// have a verbatim `selector="..."` from perception; fall back
    /// to `DomChanged` when you don't.
    ///
    /// False-positive risk: pages with timestamp tickers / animated
    /// counters / live data feeds produce diffs every tick. The 2s
    /// default timeout is short enough that most non-action-triggered
    /// changes don't have time to land — but if you're on a chatty
    /// page, prefer a selector-based variant if you can name one.
    DomChanged {
        #[serde(default = "default_effect_timeout_ms")]
        timeout_ms: u64,
    },
}

fn default_effect_timeout_ms() -> u64 {
    2_000
}

/// The action the planner wants to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlannedAction {
    Click {
        target_id: String,
        /// Optional post-state verification. When set, the runtime
        /// polls the page after the click and reports the action as
        /// failed if the expectation doesn't hold within `timeout_ms`.
        /// Closes the "click reported ok but DOM didn't react" gap.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_after: Option<EffectExpectation>,
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
        /// Optional post-state verification — see [`EffectExpectation`].
        /// Most useful for `<select>` where setting the value should
        /// flip an `aria-selected` attribute on the chosen option, or
        /// for `<input type="text">` in framework-controlled forms
        /// where the visible label should reflect the typed value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_after: Option<EffectExpectation>,
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
        /// AX hash id from the same perception snapshot that produced
        /// the plan. May be missing (empty / null) when the planner
        /// only knows the visible label — the cortex falls back to
        /// `label` + `role_hint` resolution in that case.
        ///
        /// Lenient deserialization accepts `null` from the LLM (some
        /// models emit `"target_id": null` when they want label-only
        /// dispatch) and treats it as an empty string. Without this,
        /// the whole turn parse-errors with
        /// `invalid type: null, expected a string` and the run dies
        /// before the runner gets a chance to fall back.
        #[serde(default, deserialize_with = "deserialize_string_or_value")]
        target_id: String,
        /// The action to perform: "click" (AXPress), "activate" (AXConfirm),
        /// "increment", "decrement", "show_menu"
        action: String,
        /// Label hint for fallback element resolution. AX IDs are hashes
        /// that include bounds + depth and therefore change whenever the
        /// UI mutates between plan time and dispatch time. If the cortex
        /// can't find `target_id` in the live AX tree (or `target_id`
        /// is missing entirely), it falls back to searching for the
        /// first visible element whose role matches `role_hint` (if
        /// provided) and whose label equals `label`. Planner must
        /// populate this from the same perception snapshot that
        /// produced the target_id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role_hint: Option<String>,
        /// Optional post-state verification — see [`EffectExpectation`].
        /// Same shape as the equivalent field on `Click`/`SetValue`.
        /// Mostly useful when the AX action lands on a browser DOM
        /// element (the runtime routes those through CDP), so the page
        /// state is observable via querySelector.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_after: Option<EffectExpectation>,
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
    /// Canonical navigation action. The cortex routes this through the
    /// browser adapter (Playwright) when one is registered + active, and
    /// otherwise falls back to in-cortex `cel_cdp::Page.navigate` plus a
    /// `document.readyState` poll keyed by `wait_until`.
    ///
    /// All three control fields are optional with sensible defaults so
    /// the legacy `{"type":"navigate","url":"..."}` payload still parses
    /// unchanged. Aliases `href` / `to` on `url` survive too — LLMs
    /// gravitate toward both.
    Navigate {
        #[serde(alias = "href", alias = "to")]
        url: String,
        /// One of `"none"`, `"domcontentloaded"`, `"load"`, `"networkidle"`.
        /// Unknown values are treated as the default. Default: `"domcontentloaded"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_until: Option<String>,
        /// Upper bound for the lifecycle wait. Default: 30_000 ms.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// When true (default), the cortex fallback runs a best-effort
        /// cookie-banner / overlay-dismiss script after the page loads.
        /// The TS browser adapter does this unconditionally inside its
        /// own navigate handler (process-driver.ts) so this flag only
        /// affects the in-cortex fallback path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dismiss_overlays: Option<bool>,
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
            Self::Click { target_id, .. }
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
            expect_after: None,
        };
        assert_eq!(a.target_ids(), vec!["a11y:42"]);
    }

    #[test]
    fn click_without_expect_after_round_trips_omitting_field() {
        // Back-compat: planners that don't know about `expect_after`
        // still round-trip through the same JSON shape they always
        // emitted (no field added on serialize, no field required on
        // deserialize).
        let raw = r#"{"type":"click","target_id":"dom:button:submit"}"#;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match a {
            PlannedAction::Click {
                target_id,
                expect_after,
            } => {
                assert_eq!(target_id, "dom:button:submit");
                assert!(expect_after.is_none());
            }
            _ => panic!("expected Click"),
        }
        let serialised = serde_json::to_string(&PlannedAction::Click {
            target_id: "dom:button:submit".into(),
            expect_after: None,
        })
        .unwrap();
        // `skip_serializing_if = "Option::is_none"` keeps the JSON
        // identical to the pre-`expect_after` shape.
        assert!(!serialised.contains("expect_after"));
    }

    #[test]
    fn click_with_selector_appears_expectation_round_trips() {
        let raw = r##"{
            "type": "click",
            "target_id": "dom:button:submit",
            "expect_after": {
                "kind": "selector_appears",
                "selector": "#success-message",
                "timeout_ms": 3000
            }
        }"##;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match a {
            PlannedAction::Click {
                target_id,
                expect_after:
                    Some(EffectExpectation::SelectorAppears {
                        selector,
                        timeout_ms,
                    }),
            } => {
                assert_eq!(target_id, "dom:button:submit");
                assert_eq!(selector, "#success-message");
                assert_eq!(timeout_ms, 3000);
            }
            other => panic!("expected Click with SelectorAppears, got {other:?}"),
        }
    }

    #[test]
    fn effect_expectation_default_timeout_when_omitted() {
        // `timeout_ms` has a serde default of 2000ms — the planner can
        // omit it for the common case and only override when the page
        // is known to be slow (heavy SPA render, lazy-loaded modal).
        let raw = r##"{"kind": "selector_appears", "selector": "#x"}"##;
        let e: EffectExpectation = serde_json::from_str(raw).unwrap();
        match e {
            EffectExpectation::SelectorAppears {
                selector,
                timeout_ms,
            } => {
                assert_eq!(selector, "#x");
                assert_eq!(timeout_ms, 2_000);
            }
            other => panic!("expected SelectorAppears, got {other:?}"),
        }
    }

    #[test]
    fn effect_expectation_all_four_variants_parse() {
        let appears: EffectExpectation =
            serde_json::from_str(r#"{"kind":"selector_appears","selector":".success"}"#).unwrap();
        assert!(matches!(appears, EffectExpectation::SelectorAppears { .. }));
        let disappears: EffectExpectation =
            serde_json::from_str(r#"{"kind":"selector_disappears","selector":".modal.open"}"#)
                .unwrap();
        assert!(matches!(
            disappears,
            EffectExpectation::SelectorDisappears { .. }
        ));
        let text: EffectExpectation = serde_json::from_str(
            r##"{"kind":"selector_text_contains","selector":"#status","substring":"Approved"}"##,
        )
        .unwrap();
        assert!(matches!(
            text,
            EffectExpectation::SelectorTextContains { .. }
        ));
        // `dom_changed` is the diff-based fallback for actions whose
        // post-state isn't a single named selector. No `selector`
        // field — just an optional timeout.
        let changed: EffectExpectation = serde_json::from_str(r#"{"kind":"dom_changed"}"#).unwrap();
        match changed {
            EffectExpectation::DomChanged { timeout_ms } => {
                // Default timeout when omitted.
                assert_eq!(timeout_ms, 2_000);
            }
            other => panic!("expected DomChanged, got {other:?}"),
        }
        let changed_custom: EffectExpectation =
            serde_json::from_str(r#"{"kind":"dom_changed","timeout_ms":5000}"#).unwrap();
        match changed_custom {
            EffectExpectation::DomChanged { timeout_ms } => {
                assert_eq!(timeout_ms, 5_000);
            }
            other => panic!("expected DomChanged with custom timeout, got {other:?}"),
        }
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
                    expect_after: None,
                },
                PlannedAction::SetValue {
                    target_id: "a11y:input".into(),
                    value: "v".into(),
                    expect_after: None,
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
                    expect_after: None,
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

    #[test]
    fn ax_action_accepts_null_target_id_as_empty_string() {
        // Some models emit `"target_id": null` when they only know
        // the visible label and want the runtime to resolve. The
        // previous contract treated this as a fatal parse error
        // (`invalid type: null, expected a string`), killing the
        // entire turn before the label-fallback dispatcher could
        // run. Lenient deserialization converts null to an empty
        // string; the cortex dispatcher already handles empty
        // target_id by going straight to label resolution.
        let raw = r#"{
            "type": "ax_action",
            "target_id": null,
            "action": "click",
            "label": "Export to Notes",
            "role_hint": "button"
        }"#;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match a {
            PlannedAction::AxAction {
                target_id,
                action,
                label,
                role_hint,
                ..
            } => {
                assert_eq!(target_id, "");
                assert_eq!(action, "click");
                assert_eq!(label.as_deref(), Some("Export to Notes"));
                assert_eq!(role_hint.as_deref(), Some("button"));
            }
            _ => panic!("expected AxAction"),
        }
    }

    #[test]
    fn ax_action_accepts_missing_target_id_field_entirely() {
        // Defensive: if the planner omits the field rather than
        // explicitly sending null, the `#[serde(default)]` falls
        // back to `String::default()` (empty string), same
        // dispatch path.
        let raw = r#"{
            "type": "ax_action",
            "action": "click",
            "label": "Submit"
        }"#;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match a {
            PlannedAction::AxAction {
                target_id, label, ..
            } => {
                assert_eq!(target_id, "");
                assert_eq!(label.as_deref(), Some("Submit"));
            }
            _ => panic!("expected AxAction"),
        }
    }

    #[test]
    fn ax_action_still_accepts_explicit_target_id_string() {
        // Regression guard: the lenient path must not break the
        // normal case where the planner emits a real id.
        let raw = r#"{
            "type": "ax_action",
            "target_id": "ax:AXButton/0x1234",
            "action": "click"
        }"#;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match a {
            PlannedAction::AxAction { target_id, .. } => {
                assert_eq!(target_id, "ax:AXButton/0x1234");
            }
            _ => panic!("expected AxAction"),
        }
    }

    #[test]
    fn navigate_legacy_payload_still_parses() {
        // The pre-canonical wire shape — no wait/timeout/dismiss
        // fields — must keep working unchanged. Every existing
        // planner (LangGraph, internal tests, MCP clients on older
        // SDKs) emits this; flipping to required fields would silently
        // brick navigation.
        let raw = r#"{"type":"navigate","url":"https://example.com"}"#;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match a {
            PlannedAction::Navigate {
                url,
                wait_until,
                timeout_ms,
                dismiss_overlays,
            } => {
                assert_eq!(url, "https://example.com");
                assert!(wait_until.is_none());
                assert!(timeout_ms.is_none());
                assert!(dismiss_overlays.is_none());
            }
            _ => panic!("expected Navigate"),
        }
    }

    #[test]
    fn navigate_extended_payload_parses_all_fields() {
        let raw = r#"{
            "type":"navigate",
            "url":"https://example.com",
            "wait_until":"load",
            "timeout_ms":10000,
            "dismiss_overlays":false
        }"#;
        let a: PlannedAction = serde_json::from_str(raw).unwrap();
        match a {
            PlannedAction::Navigate {
                url,
                wait_until,
                timeout_ms,
                dismiss_overlays,
            } => {
                assert_eq!(url, "https://example.com");
                assert_eq!(wait_until.as_deref(), Some("load"));
                assert_eq!(timeout_ms, Some(10_000));
                assert_eq!(dismiss_overlays, Some(false));
            }
            _ => panic!("expected Navigate"),
        }
    }

    #[test]
    fn navigate_url_aliases_href_and_to_still_work() {
        // The original `Navigate` variant accepted `href` and `to`
        // aliases on the URL field for LLM ergonomics. The extended
        // variant inherits those aliases — pin both forms to catch
        // an accidental drop during a future refactor.
        for raw in [
            r#"{"type":"navigate","href":"https://example.com"}"#,
            r#"{"type":"navigate","to":"https://example.com"}"#,
        ] {
            let a: PlannedAction = serde_json::from_str(raw).expect(raw);
            match a {
                PlannedAction::Navigate { url, .. } => {
                    assert_eq!(url, "https://example.com")
                }
                _ => panic!("expected Navigate for {raw}"),
            }
        }
    }
}
