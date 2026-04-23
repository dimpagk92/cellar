//! AppleScript / JXA bridge for deterministic spreadsheet cell writes.
//!
//! The problem this solves: driving Numbers (or any spreadsheet) via
//! raw keystrokes is not atomic. The sequence `navigate → Delete →
//! type → Return` can be perturbed between steps — focus drifts, the
//! selection moves, retries concatenate into the same cell, the AX
//! tree lags so the planner can't reliably observe whether a write
//! landed. We've watched the agent produce concatenated garbage like
//! `"23.5023502107251 15423.5023.50"` from exactly this failure mode.
//!
//! Numbers has a first-class scripting interface. `cell.value = "..."`
//! writes directly into the document model, bypasses the UI event
//! loop entirely, and is immediately queryable via `cell.value()`.
//! One JXA script handles a whole batch of cells in a single
//! `osascript` spawn.
//!
//! This module is the Rust side of that bridge. It builds a JXA
//! script from the requested writes, spawns `osascript`, parses the
//! pipe-separated readback, and surfaces a structured
//! `InputError::AppleScriptPermission` when macOS Automation
//! permission hasn't been granted (error code `-1743`).

use std::process::Command;

use crate::InputError;

/// One cell write request.
#[derive(Debug, Clone)]
pub struct CellWrite {
    /// A1-notation cell reference, e.g. `"B2"`.
    pub cell_ref: String,
    /// Value to write. Pass raw numeric strings for numeric cells
    /// (`"108432.50"`, not `"$108,432.50"`) — Numbers formats them
    /// according to the cell's display format. Text values pass
    /// through unchanged.
    pub value: String,
}

/// Write a batch of cells into Numbers in a single AppleScript call.
///
/// Resolves to `sheet 1 / table 1` of the frontmost Numbers document
/// when `sheet` and `table` are `None`. Most Numbers documents fit
/// that default.
///
/// When `verify` is `true`, the script reads each cell back after
/// writing and returns the readbacks in the same order. The caller
/// can compare against the requested values — Numbers canonicalizes
/// formatted input (e.g. `"108432.50"` → `108432.5`), so compare as
/// numbers when both sides parse as `f64`.
///
/// Failure modes:
///
/// * **Permission denied** (macOS `-1743`): returns
///   `InputError::AppleScriptPermission { app: "Numbers" }`. The user
///   must grant Automation → Numbers in System Settings → Privacy &
///   Security → Automation → <host app>. We deliberately do NOT fall
///   back to keystrokes — the keystroke path is what produced the
///   garbage data this helper exists to replace.
/// * **Numbers not running / no document** (`-1728` or similar):
///   returns `InputError::ScriptingUnavailable { app, reason }`. The
///   planner should precede this action with `activate_app: Numbers`.
/// * **Script syntax / runtime errors**: bubbled up as
///   `InputError::Failed(stderr)`.
pub fn write_numbers_cells(
    sheet: Option<&str>,
    table: Option<&str>,
    writes: &[CellWrite],
    verify: bool,
) -> Result<Vec<String>, InputError> {
    if writes.is_empty() {
        return Ok(Vec::new());
    }

    let script = build_jxa_script(sheet, table, writes, verify);

    let output = Command::new("osascript")
        .arg("-l")
        .arg("JavaScript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| InputError::Failed(format!("failed to spawn osascript: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !output.status.success() {
        // macOS error -1743 = not authorized to send events to Numbers
        if stderr.contains("-1743") || stderr.to_lowercase().contains("not authorized") {
            return Err(InputError::AppleScriptPermission {
                app: "Numbers".into(),
            });
        }
        // -1728 = can't get <object> (doc/sheet/table missing);
        // -600 = application isn't running.
        if stderr.contains("-1728") || stderr.contains("-600") {
            return Err(InputError::ScriptingUnavailable {
                app: "Numbers".into(),
                reason: format!(
                    "Numbers is not running or has no open document. Precede with \
                     activate_app: Numbers. (raw: {stderr})"
                ),
            });
        }
        return Err(InputError::Failed(format!(
            "osascript exited {:?}: {}",
            output.status.code(),
            if stderr.is_empty() { &stdout } else { &stderr }
        )));
    }

    if !verify {
        return Ok(Vec::new());
    }

    // Verification mode: stdout is pipe-separated readbacks.
    Ok(stdout.split('|').map(|s| s.to_string()).collect())
}

/// Build the JXA script. Strings are embedded via JSON encoding to
/// dodge quoting landmines (apostrophes, backslashes, unicode).
fn build_jxa_script(
    sheet: Option<&str>,
    table: Option<&str>,
    writes: &[CellWrite],
    verify: bool,
) -> String {
    let sheet_js = sheet_selector(sheet);
    let table_js = table_selector(table);

    // Encode the writes as a JSON array so embedding is safe regardless
    // of the values' content. `serde_json::to_string` produces valid
    // JavaScript literal syntax.
    let writes_json: Vec<serde_json::Value> = writes
        .iter()
        .map(|w| serde_json::json!({ "ref": w.cell_ref, "value": w.value }))
        .collect();
    let writes_json_literal =
        serde_json::to_string(&writes_json).unwrap_or_else(|_| "[]".into());

    format!(
        r#"
        var numbers = Application('Numbers');
        if (numbers.documents.length === 0) {{
            throw new Error('-1728: no open Numbers document');
        }}
        var doc = numbers.documents[0];
        var sheet = {sheet_js};
        var table = {table_js};
        var writes = {writes_json_literal};
        var results = [];
        for (var i = 0; i < writes.length; i++) {{
            var w = writes[i];
            var cell = table.cells[w.ref];
            cell.value = w.value;
            if ({verify}) {{
                var got = cell.value();
                results.push(got === null || got === undefined ? '' : String(got));
            }}
        }}
        results.join('|')
        "#,
        sheet_js = sheet_js,
        table_js = table_js,
        writes_json_literal = writes_json_literal,
        verify = if verify { "true" } else { "false" },
    )
}

fn sheet_selector(sheet: Option<&str>) -> String {
    match sheet {
        Some(name) => format!("doc.sheets[{}]", json_string(name)),
        None => "doc.sheets[0]".into(),
    }
}

fn table_selector(table: Option<&str>) -> String {
    match table {
        Some(name) => format!("sheet.tables[{}]", json_string(name)),
        None => "sheet.tables[0]".into(),
    }
}

fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_uses_json_encoded_values() {
        // Ensures a value with quotes / backslashes / newlines doesn't
        // break the embedded literal.
        let writes = vec![CellWrite {
            cell_ref: "A1".into(),
            value: "he said \"hi\"\nand\\goodbye".into(),
        }];
        let script = build_jxa_script(None, None, &writes, true);
        // JSON encoding inside the script source
        assert!(script.contains(r#""value":"he said \"hi\"\nand\\goodbye""#));
        // Default sheet / table addressing
        assert!(script.contains("doc.sheets[0]"));
        assert!(script.contains("sheet.tables[0]"));
        // Verify readback enabled
        assert!(script.contains("cell.value()"));
    }

    #[test]
    fn script_handles_named_sheet_and_table() {
        let writes = vec![CellWrite {
            cell_ref: "B2".into(),
            value: "108432.50".into(),
        }];
        let script = build_jxa_script(Some("Prices"), Some("Main"), &writes, false);
        assert!(script.contains(r#"doc.sheets["Prices"]"#));
        assert!(script.contains(r#"sheet.tables["Main"]"#));
    }

    #[test]
    fn empty_writes_returns_empty_without_spawning() {
        let res = write_numbers_cells(None, None, &[], true).unwrap();
        assert!(res.is_empty());
    }
}
