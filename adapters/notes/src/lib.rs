//! Apple Notes adapter (macOS).
//!
//! Provides deterministic note operations through AppleScript — bypasses
//! the focus-loss / keystroke-fragility problems that plague external
//! agents trying to drive Notes via cursor positioning. Operations:
//!
//! - `notes.create` — create a note with title + body in a folder
//! - `notes.set_body` — replace a note's body atomically
//! - `notes.append` — append text to a note's body
//! - `notes.get_body` — read a note's current body
//! - `notes.list` — list notes (default 50, oldest-first by mod date)
//! - `notes.find` — search notes by name
//! - `notes.delete` — soft-delete (Recently Deleted folder, 30-day recovery)
//!
//! Body format: AppleScript's `body` property on a Notes note is HTML. The
//! adapter accepts a `format` field on body-writing operations: `"plain"`
//! (default) converts `\n` to `<br>` and escapes HTML special chars so
//! caller-supplied multi-line text renders correctly; `"html"` passes
//! the body through unchanged for callers that have their own formatting.
//!
//! Quirks discovered during testing (May 2026):
//!
//! 1. **Setting the body changes the displayed title.** Notes derives the
//!    title from the first line of the body, so a `set_body` call with body
//!    starting `"<h2>Updated</h2>..."` will rename the note to "Updated"
//!    regardless of what `name` was on creation. Callers who need to
//!    preserve a stable title should either include the title as the first
//!    line of every set_body call, or use the `name` parameter on create
//!    knowing it'll be overwritten on the first set_body.
//!
//! 2. **`whose name contains` doesn't match special characters reliably.**
//!    AppleScript's name-contains filter struggles with `[`, `]`, and
//!    other punctuation. `notes.find` works best with alphanumeric queries.
//!
//! 3. **Notes filters/decodes HTML in body input.** Even when format=plain
//!    escapes `<` to `&lt;`, Notes' rich-text engine may decode the entity
//!    back to a literal `<` (and then strip it as an unknown tag). Callers
//!    who need literal angle-bracket characters in body text should consider
//!    Unicode look-alikes (`⟨⟩` U+27E8/U+27E9) or accept the loss.
//!
//! 4. **`list` is O(folder size).** For users with 600+ notes the
//!    AppleScript iteration is slow (5-15s). The `limit` parameter applies
//!    *after* iteration in Rust; v1 has no server-side pagination. Use
//!    `find` when you know what you're looking for.

#![cfg(target_os = "macos")]

use std::collections::HashMap;

use async_trait::async_trait;
use cel_context::ContextElement;
use cel_cortex::adapter::{LifecycleDeclaration, VerificationDeclaration};
use cel_cortex::{
    ActionDeclaration, ActionResult, AdapterDriver, AdapterError, AdapterManifest,
    ContextDeclaration,
};
use serde_json::{json, Value};
use std::process::Command;

pub struct NotesAdapter {
    manifest: AdapterManifest,
    connected: bool,
}

impl Default for NotesAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl NotesAdapter {
    pub fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                name: "notes".into(),
                display_name: "Apple Notes".into(),
                app_patterns: vec![String::from("(?i)^notes$")],
                platform: vec![String::from("macos")],
                runtime: String::from("native"),
                entrypoint: None,
                manifest_alias: None,
                manifest_extends: None,
                context: ContextDeclaration {
                    element_types: vec![],
                    refresh_ms: 5000,
                    confidence: 0.95,
                    truth_surface: String::from("document_model"),
                },
                // Notes operations are pure AppleScript and work regardless of
                // which app is frontmost — we never need Notes.app to be focused.
                // background_refresh=true + requires_frontmost=false means the
                // adapter stays Active as long as probe() returns true (it always
                // does on macOS), so MCP callers can issue notes.create/etc. at
                // any time without an "adapter inactive" failure.
                lifecycle: LifecycleDeclaration {
                    requires_frontmost: false,
                    bootstrap_on_activate: false,
                    background_refresh: true,
                    response_timeout_ms: None,
                },
                verification: VerificationDeclaration {
                    truth_surface: String::from("document_model"),
                    readback_action: Some(String::from("get_body")),
                    snapshot_action: None,
                },
                actions: notes_actions(),
            },
            connected: false,
        }
    }
}

#[async_trait]
impl AdapterDriver for NotesAdapter {
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
        // Notes context (list of recent notes etc.) is opt-in via the
        // `list` action — surfacing 600+ notes in every cel_see context
        // would dominate the AX tree. Adapter returns empty here on
        // purpose; callers ask explicitly via execute("list", ...).
        Ok(Vec::new())
    }

    async fn snapshot(&self) -> Result<Vec<ContextElement>, AdapterError> {
        self.get_context().await
    }

    async fn execute(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<ActionResult, AdapterError> {
        match action {
            "create" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(create_note(&params)?),
            }),
            "set_body" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(set_body(&params)?),
            }),
            "append" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(append(&params)?),
            }),
            "get_body" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(get_body(&params)?),
            }),
            "list" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(list_notes(&params)?),
            }),
            "find" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(find_notes(&params)?),
            }),
            "delete" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(delete_note(&params)?),
            }),
            _ => Err(AdapterError::ExecutionFailed(format!(
                "Notes adapter does not expose custom action \"{action}\""
            ))),
        }
    }

    async fn verify_action(
        &self,
        action: &str,
        params: &serde_json::Value,
        _result: &ActionResult,
    ) -> Result<Option<ActionResult>, AdapterError> {
        // Only verify body-write actions, and only when the caller didn't
        // opt out via `verify: false`. Verification reads the body back
        // and includes it in a wrapper ActionResult — caller decides what
        // to do with mismatches.
        if action != "set_body" && action != "append" && action != "create" {
            return Ok(None);
        }
        if params.get("verify").and_then(Value::as_bool) == Some(false) {
            return Ok(None);
        }
        // For verification to work the caller needs a stable note_id;
        // for `create`, that id comes back in the action's `data` — but
        // verify_action receives only `params`, so verify-after-create
        // would need a different flow. Skip verify for create in v1.
        if action == "create" {
            return Ok(None);
        }
        let note_id = match params.get("note_id").and_then(Value::as_str) {
            Some(id) => id.to_string(),
            None => return Ok(None),
        };
        let body = applescript_get_body(&note_id).map_err(AdapterError::ExecutionFailed)?;
        Ok(Some(ActionResult {
            success: true,
            error: None,
            data: Some(json!({
                "note_id": note_id,
                "body_after": body,
            })),
        }))
    }

    async fn probe(&self) -> bool {
        true
    }

    async fn facts_for_planning_view(
        &self,
        _goal: &str,
        _context: &cel_context::ScreenContext,
    ) -> Vec<cel_contracts::AdapterFactRef> {
        Vec::new()
    }
}

// --- Action handlers ---

fn create_note(params: &Value) -> Result<Value, AdapterError> {
    let title = required_string(params, "title")?;
    let body_raw = required_string(params, "body")?;
    let folder = optional_string(params, "folder").unwrap_or_else(|| "Notes".to_string());
    let account = optional_string(params, "account").unwrap_or_else(|| "iCloud".to_string());
    let format = optional_string(params, "format").unwrap_or_else(|| "plain".to_string());
    let body_html = render_body(&body_raw, &format);

    let script = format!(
        r#"tell application "Notes"
  set newNote to make new note at folder "{folder}" of account "{account}" with properties {{name:"{title}", body:"{body}"}}
  return id of newNote
end tell"#,
        folder = applescript_escape(&folder),
        account = applescript_escape(&account),
        title = applescript_escape(&title),
        body = applescript_escape(&body_html),
    );
    let id = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "note_id": id.trim(),
        "title": title,
        "folder": folder,
        "account": account,
    }))
}

fn set_body(params: &Value) -> Result<Value, AdapterError> {
    let note_id = required_string(params, "note_id")?;
    let body_raw = required_string(params, "body")?;
    let format = optional_string(params, "format").unwrap_or_else(|| "plain".to_string());
    let body_html = render_body(&body_raw, &format);

    let script = format!(
        r#"tell application "Notes"
  set body of note id "{id}" to "{body}"
end tell"#,
        id = applescript_escape(&note_id),
        body = applescript_escape(&body_html),
    );
    run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "note_id": note_id,
        "format": format,
    }))
}

fn append(params: &Value) -> Result<Value, AdapterError> {
    let note_id = required_string(params, "note_id")?;
    let text_raw = required_string(params, "text")?;
    let format = optional_string(params, "format").unwrap_or_else(|| "plain".to_string());
    let appended_html = render_body(&text_raw, &format);

    // AppleScript-side concat: read existing body, append, write back. We
    // do this in one script so there's no observable intermediate state.
    let script = format!(
        r#"tell application "Notes"
  set existingBody to body of note id "{id}"
  set body of note id "{id}" to existingBody & "<br>" & "{appended}"
end tell"#,
        id = applescript_escape(&note_id),
        appended = applescript_escape(&appended_html),
    );
    run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "note_id": note_id,
        "appended_chars": text_raw.len(),
    }))
}

fn get_body(params: &Value) -> Result<Value, AdapterError> {
    let note_id = required_string(params, "note_id")?;
    let body = applescript_get_body(&note_id).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "note_id": note_id,
        "body": body,
    }))
}

fn list_notes(params: &Value) -> Result<Value, AdapterError> {
    let folder = optional_string(params, "folder").unwrap_or_else(|| "Notes".to_string());
    let account = optional_string(params, "account").unwrap_or_else(|| "iCloud".to_string());
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(50);

    // Sort: AppleScript doesn't sort directly. We pull (id, name, mod-date)
    // for all notes in the folder, then return the first `limit` as-stored
    // (Notes typically returns them most-recently-modified first, but
    // that's implementation-defined; document this).
    //
    // For folders with thousands of notes, this AppleScript is slow. The
    // `limit` doesn't reduce work on the AS side — we still iterate every
    // note. Truncation happens in Rust. v2 could pre-bind a max via the
    // `notes ... from index 1 to limit` AppleScript idiom.
    let script = format!(
        r#"set output to ""
tell application "Notes"
  set theFolder to folder "{folder}" of account "{account}"
  set theNotes to notes of theFolder
  repeat with n in theNotes
    set output to output & (id of n) & "|||" & (name of n) & "|||" & (modification date of n as string) & linefeed
  end repeat
end tell
return output"#,
        folder = applescript_escape(&folder),
        account = applescript_escape(&account),
    );
    let stdout = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;

    let mut items: Vec<Value> = Vec::new();
    for line in stdout.lines().take(limit as usize) {
        let parts: Vec<&str> = line.splitn(3, "|||").collect();
        if parts.len() < 3 {
            continue;
        }
        items.push(json!({
            "note_id": parts[0].trim(),
            "title": parts[1].trim(),
            "modified_at": parts[2].trim(),
        }));
    }
    Ok(json!({
        "folder": folder,
        "account": account,
        "count": items.len(),
        "notes": items,
    }))
}

fn delete_note(params: &Value) -> Result<Value, AdapterError> {
    let note_id = required_string(params, "note_id")?;
    // macOS Notes `delete` moves the note to Recently Deleted (30-day
    // recovery), so this is reversible — but still document the move so
    // callers don't expect immediate permanent removal.
    let script = format!(
        r#"tell application "Notes"
  delete note id "{id}"
end tell"#,
        id = applescript_escape(&note_id),
    );
    run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "note_id": note_id,
        "moved_to": "Recently Deleted",
        "permanence": "30-day recovery window before iCloud purges",
    }))
}

fn find_notes(params: &Value) -> Result<Value, AdapterError> {
    let query = required_string(params, "query")?;
    let folder = optional_string(params, "folder").unwrap_or_else(|| "Notes".to_string());
    let account = optional_string(params, "account").unwrap_or_else(|| "iCloud".to_string());
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(20);

    // AppleScript's `whose name contains` filter is server-side and much
    // faster than pulling every note then filtering. We search name first;
    // body search would require iterating + reading body of every note,
    // which is prohibitively slow for large folders. v2 could add an opt-in
    // `body_search: true` for users who want it.
    let script = format!(
        r#"set output to ""
tell application "Notes"
  set theFolder to folder "{folder}" of account "{account}"
  set hits to notes of theFolder whose name contains "{query}"
  repeat with n in hits
    set output to output & (id of n) & "|||" & (name of n) & "|||" & (modification date of n as string) & linefeed
  end repeat
end tell
return output"#,
        folder = applescript_escape(&folder),
        account = applescript_escape(&account),
        query = applescript_escape(&query),
    );
    let stdout = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;

    let mut items: Vec<Value> = Vec::new();
    for line in stdout.lines().take(limit as usize) {
        let parts: Vec<&str> = line.splitn(3, "|||").collect();
        if parts.len() < 3 {
            continue;
        }
        items.push(json!({
            "note_id": parts[0].trim(),
            "title": parts[1].trim(),
            "modified_at": parts[2].trim(),
        }));
    }
    Ok(json!({
        "query": query,
        "folder": folder,
        "account": account,
        "count": items.len(),
        "notes": items,
    }))
}

// --- AppleScript helpers ---

fn applescript_get_body(note_id: &str) -> Result<String, String> {
    let script = format!(
        r#"tell application "Notes"
  return body of note id "{id}"
end tell"#,
        id = applescript_escape(note_id),
    );
    run_osascript(&script).map(|s| s.trim_end_matches('\n').to_string())
}

fn run_osascript(script: &str) -> Result<String, String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("Failed to spawn osascript: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "AppleScript failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Escape a string for safe interpolation into an AppleScript double-quoted
/// string literal. AppleScript strings need backslash-escaping for `"` and
/// `\` only — newlines are allowed literally but agents typically pass
/// `\n`, which we let the body renderer handle for plain-mode bodies.
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Convert agent-supplied body text to the HTML Notes expects.
///
/// - `plain` (default): escape HTML special chars, convert `\n` to `<br>`,
///   preserve consecutive newlines as `<br><br>` (Notes paragraph break).
/// - `html`: pass through unchanged. Caller is responsible for escaping.
fn render_body(body: &str, format: &str) -> String {
    if format == "html" {
        return body.to_string();
    }
    // Plain mode: HTML-escape special chars, then convert newlines.
    let escaped = body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    escaped.replace('\n', "<br>")
}

fn required_string(params: &Value, field: &str) -> Result<String, AdapterError> {
    params
        .get(field)
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .ok_or_else(|| AdapterError::ExecutionFailed(format!("missing `{field}` string field")))
}

fn optional_string(params: &Value, field: &str) -> Option<String> {
    params
        .get(field)
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

fn notes_actions() -> HashMap<String, ActionDeclaration> {
    HashMap::from([
        (
            String::from("create"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("title"), String::from("string")),
                    (String::from("body"), String::from("string")),
                    (String::from("folder"), String::from("string?")),
                    (String::from("account"), String::from("string?")),
                    (String::from("format"), String::from("string? (plain|html)")),
                ]),
                description: String::from(
                    "Create a new note. Returns note_id for use with set_body / get_body / append.",
                ),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("set_body"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("note_id"), String::from("string")),
                    (String::from("body"), String::from("string")),
                    (String::from("format"), String::from("string? (plain|html)")),
                    (String::from("verify"), String::from("boolean?")),
                ]),
                description: String::from(
                    "Replace the body of a note. Default format=plain treats input as text and converts newlines to <br>.",
                ),
                mutates_state: true,
                requires_verification: true,
                returns_data: true,
            },
        ),
        (
            String::from("append"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("note_id"), String::from("string")),
                    (String::from("text"), String::from("string")),
                    (String::from("format"), String::from("string? (plain|html)")),
                ]),
                description: String::from(
                    "Append text to the end of a note's body. A <br> separator is inserted between the existing body and the appended text.",
                ),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("get_body"),
            ActionDeclaration {
                params: HashMap::from([(String::from("note_id"), String::from("string"))]),
                description: String::from(
                    "Return the current HTML body of a note. Use for verification or extraction.",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("list"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("folder"), String::from("string?")),
                    (String::from("account"), String::from("string?")),
                    (String::from("limit"), String::from("number?")),
                ]),
                description: String::from(
                    "List notes in a folder. Default folder=Notes, account=iCloud, limit=50.",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("delete"),
            ActionDeclaration {
                params: HashMap::from([(String::from("note_id"), String::from("string"))]),
                description: String::from(
                    "Move a note to the Recently Deleted folder. Soft delete: 30-day recovery window before iCloud purges.",
                ),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("find"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("query"), String::from("string")),
                    (String::from("folder"), String::from("string?")),
                    (String::from("account"), String::from("string?")),
                    (String::from("limit"), String::from("number?")),
                ]),
                description: String::from(
                    "Search notes by title (server-side `whose name contains` filter). Body search is not supported in v1 because it requires reading every note's body.",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_body_plain_escapes_html_and_converts_newlines() {
        let out = render_body("line 1\nline 2\n\nAT&T <div>", "plain");
        assert_eq!(out, "line 1<br>line 2<br><br>AT&amp;T &lt;div&gt;");
    }

    #[test]
    fn render_body_html_passes_through() {
        let raw = "<h1>Title</h1><p>Body</p>";
        assert_eq!(render_body(raw, "html"), raw);
    }

    #[test]
    fn applescript_escape_quotes_and_backslashes() {
        assert_eq!(
            applescript_escape(r#"he said "hi"\n"#),
            r#"he said \"hi\"\\n"#
        );
    }
}
