//! Apple Numbers adapter (macOS).
//!
//! Provides perception-side context for Numbers — the document-model
//! preview of cells A1:F6 — fused into the Cortex's `get_context` so
//! agents see deterministic spreadsheet truth rather than relying on
//! the Numbers AX tree (which only exposes the focused cell or formula
//! bar contents).
//!
//! Action execution for `write_cells` / `read_cells` still has a standard
//! CEL surface, but the adapter also exposes those same deterministic
//! document-model operations through the adapter contract so external
//! agents and future runtimes can treat Numbers like any other adapter:
//! snapshot-capable and verification-aware.
//!
//! # History
//!
//! Originally lived in `cel-cortex/src/native_adapters.rs`. Extracted
//! into a standalone `adapters/numbers` crate to follow the same
//! packaging convention as `adapters/browser`, per the adapter-sdk
//! pivot in April 2026 (see `docs/adapter-roadmap.md` P0).

// AppleScript-only — entire adapter compiles to a no-op on non-macOS.
#![cfg(target_os = "macos")]

use std::collections::HashMap;

use async_trait::async_trait;
use cel_accessibility::ElementState;
use cel_context::{ContentRole, ContextElement, ContextSource};
use cel_cortex::adapter::{LifecycleDeclaration, VerificationDeclaration};
use cel_cortex::{
    ActionDeclaration, ActionResult, AdapterDriver, AdapterError, AdapterManifest,
    ContextDeclaration,
};
#[cfg(target_os = "macos")]
use cel_input::CellWrite;
use serde_json::{json, Value};

const NUMBERS_PREVIEW_ROWS: usize = 6;
const NUMBERS_PREVIEW_COLS: usize = 6;

pub struct NumbersAdapter {
    manifest: AdapterManifest,
    connected: bool,
}

impl Default for NumbersAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl NumbersAdapter {
    pub fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                name: "numbers".into(),
                display_name: "Apple Numbers".into(),
                app_patterns: vec![String::from("(?i)numbers")],
                platform: vec![String::from("macos")],
                runtime: String::from("native"),
                entrypoint: None,
                manifest_alias: None,
                manifest_extends: None,
                context: ContextDeclaration {
                    element_types: vec![String::from("table"), String::from("table_cell")],
                    refresh_ms: 200,
                    confidence: 0.98,
                    truth_surface: String::from("document_model"),
                },
                lifecycle: LifecycleDeclaration::default(),
                verification: VerificationDeclaration {
                    truth_surface: String::from("document_model"),
                    readback_action: Some(String::from("read_cells")),
                    snapshot_action: Some(String::from("snapshot_preview")),
                },
                actions: numbers_actions(),
            },
            connected: false,
        }
    }

    fn build_preview_elements(&self, values: &[String]) -> Vec<ContextElement> {
        let root_id = String::from("numbers:table:preview");
        let refs = preview_refs();
        let mut elements = vec![ContextElement {
            id: root_id.clone(),
            label: Some(String::from("Numbers Preview Table")),
            description: Some(format!(
                "Document-model preview of Numbers cells A1:{}{}",
                column_label(NUMBERS_PREVIEW_COLS - 1),
                NUMBERS_PREVIEW_ROWS
            )),
            element_type: String::from("table"),
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
            actions: Vec::new(),
            confidence: 0.97,
            source: ContextSource::NativeApi,
            content_role: ContentRole::Content,
            properties: HashMap::from([
                (String::from("app"), String::from("Numbers")),
                (
                    String::from("preview_range"),
                    format!(
                        "A1:{}{}",
                        column_label(NUMBERS_PREVIEW_COLS - 1),
                        NUMBERS_PREVIEW_ROWS
                    ),
                ),
            ]),
        }];

        for (idx, value) in values.iter().enumerate() {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                continue;
            }
            let cell_ref = refs[idx].clone();
            elements.push(ContextElement {
                id: format!("numbers:cell:{cell_ref}"),
                label: Some(cell_ref.clone()),
                description: Some(String::from("Numbers cell value from document model")),
                element_type: String::from("table_cell"),
                value: Some(trimmed.to_string()),
                bounds: None,
                state: ElementState {
                    focused: false,
                    enabled: true,
                    visible: true,
                    selected: false,
                    expanded: None,
                    checked: None,
                },
                parent_id: Some(root_id.clone()),
                actions: Vec::new(),
                confidence: 0.98,
                source: ContextSource::NativeApi,
                content_role: ContentRole::Content,
                properties: HashMap::from([
                    (String::from("app"), String::from("Numbers")),
                    (String::from("cell_ref"), cell_ref),
                ]),
            });
        }

        elements
    }

    #[cfg(target_os = "macos")]
    fn read_preview_values(&self) -> Result<Vec<String>, AdapterError> {
        let refs = preview_refs();
        match cel_input::read_numbers_cells(None, None, &refs) {
            Ok(values) => Ok(values),
            Err(cel_input::InputError::ScriptingUnavailable { app, .. })
                if app.eq_ignore_ascii_case("Numbers") =>
            {
                Ok(Vec::new())
            }
            Err(err) => Err(AdapterError::ContextReadFailed(err.to_string())),
        }
    }

    #[cfg(target_os = "macos")]
    fn preview_payload(&self) -> Result<Value, AdapterError> {
        let values = self.read_preview_values()?;
        let refs = preview_refs();
        Ok(json!({
            "preview_range": format!(
                "A1:{}{}",
                column_label(NUMBERS_PREVIEW_COLS - 1),
                NUMBERS_PREVIEW_ROWS
            ),
            "cells": refs
                .into_iter()
                .zip(values.into_iter())
                .map(|(cell_ref, value)| json!({ "ref": cell_ref, "value": value }))
                .collect::<Vec<_>>(),
        }))
    }

    #[cfg(target_os = "macos")]
    fn read_cells_payload(&self, params: &Value) -> Result<Value, AdapterError> {
        let sheet = optional_string_field(params, "sheet");
        let table = optional_string_field(params, "table");
        let cell_refs = string_array_field(params, "cell_refs")?;
        let values = cel_input::read_numbers_cells(sheet.as_deref(), table.as_deref(), &cell_refs)
            .map_err(|err| AdapterError::ExecutionFailed(err.to_string()))?;
        Ok(json!({
            "app": "Numbers",
            "reads": cell_refs
                .iter()
                .zip(values.iter())
                .map(|(cell_ref, value)| json!({ "ref": cell_ref, "value": value }))
                .collect::<Vec<_>>(),
        }))
    }

    #[cfg(target_os = "macos")]
    fn write_cells_payload(&self, params: &Value) -> Result<Value, AdapterError> {
        let sheet = optional_string_field(params, "sheet");
        let table = optional_string_field(params, "table");
        let verify = params
            .get("verify")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let writes = parse_cell_writes(params)?;
        let readbacks =
            cel_input::write_numbers_cells(sheet.as_deref(), table.as_deref(), &writes, verify)
                .map_err(|err| AdapterError::ExecutionFailed(err.to_string()))?;
        Ok(json!({
            "app": "Numbers",
            "writes": writes
                .iter()
                .zip(readbacks.iter().chain(std::iter::repeat(&String::new())))
                .map(|(write, readback)| {
                    json!({
                        "ref": write.cell_ref,
                        "requested": write.value,
                        "readback": readback,
                    })
                })
                .collect::<Vec<_>>(),
        }))
    }

    #[cfg(target_os = "macos")]
    fn verify_cells_result(&self, params: &Value) -> Result<ActionResult, AdapterError> {
        let reads = self.read_cells_payload(params)?;
        let requested = parse_cell_writes(params)?;
        let actual_reads = reads
            .get("reads")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AdapterError::ExecutionFailed("verify_cells expected read_cells payload".into())
            })?;

        let mismatches = requested
            .iter()
            .zip(actual_reads.iter())
            .filter_map(|(write, read)| {
                let actual = read
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if cells_match(&write.value, &actual) {
                    None
                } else {
                    Some(format!(
                        "{}: wrote \"{}\" got \"{}\"",
                        write.cell_ref, write.value, actual
                    ))
                }
            })
            .collect::<Vec<_>>();

        if mismatches.is_empty() {
            Ok(ActionResult {
                success: true,
                error: None,
                data: Some(reads),
            })
        } else {
            Ok(ActionResult::fail(format!(
                "Numbers verification failed: {}",
                mismatches.join("; ")
            )))
        }
    }
}

#[async_trait]
impl AdapterDriver for NumbersAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    async fn activate(&mut self) -> Result<(), AdapterError> {
        self.connected = true;
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<(), AdapterError> {
        self.connected = false;
        Ok(())
    }

    async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError> {
        if !self.connected {
            return Ok(Vec::new());
        }

        #[cfg(target_os = "macos")]
        {
            let values = self.read_preview_values()?;
            Ok(self.build_preview_elements(&values))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(Vec::new())
        }
    }

    async fn snapshot(&self) -> Result<Vec<ContextElement>, AdapterError> {
        self.get_context().await
    }

    async fn execute(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<ActionResult, AdapterError> {
        #[cfg(target_os = "macos")]
        {
            match action {
                "snapshot_preview" => Ok(ActionResult {
                    success: true,
                    error: None,
                    data: Some(self.preview_payload()?),
                }),
                "read_cells" => Ok(ActionResult {
                    success: true,
                    error: None,
                    data: Some(self.read_cells_payload(&params)?),
                }),
                "write_cells" => Ok(ActionResult {
                    success: true,
                    error: None,
                    data: Some(self.write_cells_payload(&params)?),
                }),
                "verify_cells" => self.verify_cells_result(&params),
                _ => Err(AdapterError::ExecutionFailed(format!(
                    "Numbers adapter does not expose custom action \"{action}\""
                ))),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = params;
            Err(AdapterError::ExecutionFailed(format!(
                "Numbers adapter action \"{action}\" requires macOS"
            )))
        }
    }

    async fn verify_action(
        &self,
        action: &str,
        params: &serde_json::Value,
        _result: &ActionResult,
    ) -> Result<Option<ActionResult>, AdapterError> {
        if action != "write_cells" {
            return Ok(None);
        }

        #[cfg(target_os = "macos")]
        {
            let verification = self.execute("verify_cells", params.clone()).await?;
            Ok(Some(verification))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(None)
        }
    }

    async fn probe(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            true
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    async fn facts_for_planning_view(
        &self,
        _goal: &str,
        _context: &cel_context::ScreenContext,
    ) -> Vec<cel_contracts::AdapterFactRef> {
        if !self.connected {
            return Vec::new();
        }

        #[cfg(target_os = "macos")]
        {
            self.preview_payload()
                .ok()
                .and_then(numbers_preview_fact)
                .into_iter()
                .collect()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Vec::new()
        }
    }
}

fn numbers_preview_fact(preview: Value) -> Option<cel_contracts::AdapterFactRef> {
    let preview_range = preview
        .get("preview_range")
        .and_then(Value::as_str)
        .unwrap_or("A1:F6")
        .to_string();
    let non_empty_cells = preview
        .get("cells")
        .and_then(Value::as_array)
        .map(|cells| {
            cells
                .iter()
                .filter(|cell| {
                    cell.get("value")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .map(|value| !value.is_empty())
                        .unwrap_or(false)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let non_empty_count = non_empty_cells.len();

    Some(cel_contracts::AdapterFactRef {
        id: Some(format!("numbers:preview:{preview_range}")),
        adapter: "numbers".into(),
        kind: "table_preview".into(),
        payload: json!({
            "preview_range": preview_range,
            "non_empty_cells": non_empty_cells,
            "non_empty_count": non_empty_count,
        }),
    })
}

fn numbers_actions() -> HashMap<String, ActionDeclaration> {
    HashMap::from([
        (
            String::from("snapshot_preview"),
            ActionDeclaration {
                params: HashMap::new(),
                description: String::from(
                    "Return a compact document-model preview of the active Numbers sheet",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("read_cells"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("sheet"), String::from("string?")),
                    (String::from("table"), String::from("string?")),
                    (String::from("cell_refs"), String::from("string[]")),
                ]),
                description: String::from(
                    "Read deterministic cell values from the Numbers document model",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("write_cells"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("sheet"), String::from("string?")),
                    (String::from("table"), String::from("string?")),
                    (String::from("writes"), String::from("cell_write[]")),
                    (String::from("verify"), String::from("boolean")),
                ]),
                description: String::from(
                    "Write deterministic cell values into the Numbers document model",
                ),
                mutates_state: true,
                requires_verification: true,
                returns_data: true,
            },
        ),
        (
            String::from("verify_cells"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("sheet"), String::from("string?")),
                    (String::from("table"), String::from("string?")),
                    (String::from("writes"), String::from("cell_write[]")),
                ]),
                description: String::from(
                    "Read Numbers cell values back and verify they match the requested writes",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
    ])
}

fn optional_string_field(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn string_array_field(params: &Value, key: &str) -> Result<Vec<String>, AdapterError> {
    let values = params
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| AdapterError::ExecutionFailed(format!("missing `{key}` array")))?;
    Ok(values
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect())
}

#[cfg(target_os = "macos")]
fn parse_cell_writes(params: &Value) -> Result<Vec<CellWrite>, AdapterError> {
    let writes = params
        .get("writes")
        .and_then(Value::as_array)
        .ok_or_else(|| AdapterError::ExecutionFailed("missing `writes` array".into()))?;

    let mut parsed = Vec::with_capacity(writes.len());
    for value in writes {
        let cell_ref = value
            .get("cell_ref")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|cell_ref| !cell_ref.is_empty())
            .ok_or_else(|| {
                AdapterError::ExecutionFailed(
                    "write_cells expects each entry to include non-empty `cell_ref`".into(),
                )
            })?;
        let cell_value = value.get("value").and_then(Value::as_str).ok_or_else(|| {
            AdapterError::ExecutionFailed(
                "write_cells expects each entry to include string `value`".into(),
            )
        })?;
        parsed.push(CellWrite {
            cell_ref: cell_ref.to_string(),
            value: cell_value.to_string(),
        });
    }

    Ok(parsed)
}

fn cells_match(expected: &str, actual: &str) -> bool {
    let norm_expected = expected.trim();
    let norm_actual = actual.trim();
    if norm_expected == norm_actual {
        return true;
    }

    let parse_number = |value: &str| -> Option<f64> {
        let cleaned = value
            .chars()
            .filter(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+'))
            .collect::<String>();
        if cleaned.is_empty() {
            None
        } else {
            cleaned.parse::<f64>().ok()
        }
    };

    match (parse_number(norm_expected), parse_number(norm_actual)) {
        (Some(left), Some(right)) => (left - right).abs() <= 0.000_001,
        _ => false,
    }
}

fn preview_refs() -> Vec<String> {
    let mut refs = Vec::with_capacity(NUMBERS_PREVIEW_ROWS * NUMBERS_PREVIEW_COLS);
    for row in 1..=NUMBERS_PREVIEW_ROWS {
        for col in 0..NUMBERS_PREVIEW_COLS {
            refs.push(format!("{}{}", column_label(col), row));
        }
    }
    refs
}

fn column_label(mut index: usize) -> String {
    let mut label = String::new();
    loop {
        let remainder = (index % 26) as u8;
        label.insert(0, (b'A' + remainder) as char);
        if index < 26 {
            break;
        }
        index = (index / 26) - 1;
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_refs_cover_first_grid() {
        let refs = preview_refs();
        assert_eq!(refs.first().map(String::as_str), Some("A1"));
        assert_eq!(refs.get(5).map(String::as_str), Some("F1"));
        assert_eq!(refs.get(6).map(String::as_str), Some("A2"));
        assert_eq!(refs.last().map(String::as_str), Some("F6"));
        assert_eq!(refs.len(), NUMBERS_PREVIEW_ROWS * NUMBERS_PREVIEW_COLS);
    }

    #[test]
    fn column_label_handles_wrapped_columns() {
        assert_eq!(column_label(0), "A");
        assert_eq!(column_label(25), "Z");
        assert_eq!(column_label(26), "AA");
        assert_eq!(column_label(27), "AB");
    }

    #[test]
    fn manifest_declares_document_model_truth_and_actions() {
        let adapter = NumbersAdapter::new();
        assert_eq!(adapter.manifest.context.truth_surface, "document_model");
        assert_eq!(
            adapter.manifest.verification.truth_surface,
            "document_model"
        );
        assert_eq!(
            adapter.manifest.verification.readback_action.as_deref(),
            Some("read_cells")
        );
        assert!(adapter.manifest.actions.contains_key("write_cells"));
        assert!(adapter
            .manifest
            .actions
            .get("write_cells")
            .map(|decl| decl.requires_verification)
            .unwrap_or(false));
    }

    #[test]
    fn preview_fact_carries_compact_document_model_truth() {
        let fact = numbers_preview_fact(json!({
            "preview_range": "A1:F6",
            "cells": [
                { "ref": "A1", "value": "Ticker" },
                { "ref": "B1", "value": "" },
                { "ref": "A2", "value": "BTC" }
            ]
        }))
        .expect("preview fact");

        assert_eq!(fact.id.as_deref(), Some("numbers:preview:A1:F6"));
        assert_eq!(fact.adapter, "numbers");
        assert_eq!(fact.kind, "table_preview");
        assert_eq!(fact.payload["non_empty_count"], 2);
        assert_eq!(fact.payload["non_empty_cells"][0]["ref"], "A1");
        assert_eq!(fact.payload["non_empty_cells"][1]["ref"], "A2");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn parse_cell_writes_requires_cell_ref_and_value() {
        let params = json!({
            "writes": [
                { "cell_ref": "A1", "value": "BTC" },
                { "cell_ref": "B1", "value": "ETH" }
            ]
        });
        let writes = parse_cell_writes(&params).expect("writes should parse");
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].cell_ref, "A1");
        assert_eq!(writes[1].value, "ETH");
    }

    #[test]
    fn cells_match_accepts_numeric_canonicalization() {
        assert!(cells_match("108432.50", "108432.5"));
        assert!(cells_match("$108,432.50", "108432.5"));
        assert!(!cells_match("BTC", "ETH"));
    }
}
