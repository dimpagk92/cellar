//! Apple Reminders adapter (macOS).
//!
//! Provides deterministic reminder operations through AppleScript. Operations:
//!
//! - `add` — create a reminder in a named list. Returns `reminder_id` (string).
//! - `list` — list reminders, optionally filtered by list / completion / due.
//! - `complete` — mark a reminder as completed.
//! - `update` — change title / due / notes on an existing reminder.
//! - `delete` — remove a reminder.
//!
//! `reminder_id` is the AppleScript `id` of the reminder — a string formatted
//! by Reminders.app (typically `x-apple-reminderkit://REMCDReminder/<uuid>`).
//! It's globally unique and stable across launches.
//!
//! Quirks discovered during testing (May 2026):
//!
//! 1. **List names are case-sensitive.** `tell list "groceries"` won't match
//!    a list named "Groceries". The adapter does NOT pre-validate names;
//!    callers should use `list` with no filter to discover available names.
//!
//! 2. **`due date` is locale-fragile when parsed from strings.** The adapter
//!    builds AppleScript dates programmatically (`set year/month/day`) to
//!    avoid the `date "..."` locale dependence. The initial `set day to 1`
//!    guards against the current-date-31st pitfall.
//!
//! 3. **`completed` reminders are visible by default in `list`.** The
//!    `completed: false` default in v1 mirrors the typical "show me what's
//!    open" use case; pass `completed: true` to see done reminders, or omit
//!    to skip the filter entirely.
//!
//! 4. **Deletes are permanent.** Unlike Notes, Reminders has no Recently
//!    Deleted folder. Deleted reminders are gone unless restored from a
//!    Time Machine / iCloud archive backup.

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

pub struct RemindersAdapter {
    manifest: AdapterManifest,
}

impl Default for RemindersAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl RemindersAdapter {
    pub fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                name: "reminders".into(),
                display_name: "Apple Reminders".into(),
                app_patterns: vec![String::from("(?i)^reminders$")],
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
                lifecycle: LifecycleDeclaration {
                    requires_frontmost: false,
                    bootstrap_on_activate: false,
                    background_refresh: true,
                    // Reminders.app's `list` walks every reminder in the
                    // list; bulk-property reads bring this from O(40s) down
                    // to O(5–10s) for iCloud-synced lists. Bump the
                    // ProcessDriver timeout accordingly so a healthy list
                    // call doesn't trip the default 30s.
                    response_timeout_ms: Some(45_000),
                },
                verification: VerificationDeclaration {
                    truth_surface: String::from("document_model"),
                    readback_action: None,
                    snapshot_action: None,
                },
                actions: reminders_actions(),
            },
        }
    }
}

#[async_trait]
impl AdapterDriver for RemindersAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    async fn activate(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }

    async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError> {
        Ok(Vec::new())
    }

    async fn snapshot(&self) -> Result<Vec<ContextElement>, AdapterError> {
        Ok(Vec::new())
    }

    async fn execute(&self, action: &str, params: Value) -> Result<ActionResult, AdapterError> {
        match action {
            "add" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(add_reminder(&params)?),
            }),
            "list" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(list_reminders(&params)?),
            }),
            "complete" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(complete_reminder(&params)?),
            }),
            "update" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(update_reminder(&params)?),
            }),
            "delete" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(delete_reminder(&params)?),
            }),
            _ => Err(AdapterError::ExecutionFailed(format!(
                "Reminders adapter does not expose action \"{action}\""
            ))),
        }
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

fn add_reminder(params: &Value) -> Result<Value, AdapterError> {
    let list_name = required_string(params, "list")?;
    let title = required_string(params, "title")?;
    let due = optional_string(params, "due");
    let notes = optional_string(params, "notes");

    let mut date_prelude = String::new();
    let mut props = format!("name:\"{}\"", applescript_escape(&title));
    if let Some(n) = &notes {
        props.push_str(&format!(", body:\"{}\"", applescript_escape(n)));
    }
    if let Some(d) = &due {
        date_prelude.push_str(&applescript_date_snippet(d, "dueDate")?);
        props.push_str(", due date:dueDate");
    }

    let script = format!(
        r#"tell application "Reminders"
{date_prelude}  tell list "{list_name}"
    set r to make new reminder with properties {{{props}}}
    return id of r
  end tell
end tell"#,
        date_prelude = date_prelude,
        list_name = applescript_escape(&list_name),
        props = props,
    );
    let id = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "reminder_id": id.trim(),
        "list": list_name,
        "title": title,
    }))
}

fn list_reminders(params: &Value) -> Result<Value, AdapterError> {
    let list_filter = optional_string(params, "list");
    let due_before = optional_string(params, "due_before");
    let completed_filter = params.get("completed").and_then(Value::as_bool);
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(50);

    // Default completed=false (the typical "what's open" case). Pass
    // explicit completed=true to see done reminders, or pass null/omit
    // for both. We can't distinguish "omitted" from "null" easily here;
    // treat absence as default-false.
    let completed_value = completed_filter.unwrap_or(false);

    let mut whose_clauses: Vec<String> = Vec::new();
    let mut date_prelude = String::new();
    if let Some(d) = &due_before {
        date_prelude.push_str(&applescript_date_snippet(d, "dueCutoff")?);
        whose_clauses.push("due date is less than dueCutoff".into());
    }
    whose_clauses.push(format!(
        "completed is {}",
        if completed_value { "true" } else { "false" }
    ));
    let whose_expr = format!(" whose {}", whose_clauses.join(" and "));

    let list_loop_header = match &list_filter {
        Some(name) => format!(
            "  set theLists to {{list \"{}\"}}\n",
            applescript_escape(name)
        ),
        None => "  set theLists to every list\n".to_string(),
    };

    // Use `properties of every reminder` for bulk-load — one IPC call
    // returns all property records, after which we iterate locally in
    // AppleScript without further round-trips. Per-property `of every
    // reminder` access is 5–10x faster than per-reminder property access
    // (verified against an 18-item list: 5s vs 40s). The trade-off: the
    // record carries every property whether we use it or not, so memory
    // usage is higher — acceptable for `limit ≤ 100` use cases.
    let script = format!(
        r#"set output to ""
tell application "Reminders"
{date_prelude}{list_loop}  set kept to 0
  repeat with l in theLists
    set listName to name of l
    try
      tell l
        set theProps to properties of (every reminder{whose})
      end tell
      repeat with p in theProps
        if kept is greater than or equal to {limit} then exit repeat
        set theId to id of p as string
        set theName to name of p
        set theDue to ""
        try
          set theDue to (due date of p) as string
        end try
        set theCompleted to completed of p
        set theNotes to ""
        try
          set theNotes to body of p
        end try
        set output to output & theId & "«FIELD»" & theName & "«FIELD»" & theDue & "«FIELD»" & listName & "«FIELD»" & (theCompleted as string) & "«FIELD»" & theNotes & "«ROW»"
        set kept to kept + 1
      end repeat
    end try
    if kept is greater than or equal to {limit} then exit repeat
  end repeat
end tell
return output"#,
        date_prelude = date_prelude,
        list_loop = list_loop_header,
        whose = whose_expr,
        limit = limit,
    );
    let stdout = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;

    let mut items: Vec<Value> = Vec::new();
    for row in stdout.split("«ROW»") {
        let trimmed = row.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.splitn(6, "«FIELD»").collect();
        if parts.len() < 6 {
            continue;
        }
        let completed = parts[4].trim().eq_ignore_ascii_case("true");
        let notes = parts[5].replace(['\r', '\n'], " ");
        items.push(json!({
            "reminder_id": parts[0].trim(),
            "title": parts[1].trim(),
            "due": parts[2].trim(),
            "list": parts[3].trim(),
            "completed": completed,
            "notes": notes.trim(),
        }));
    }
    Ok(json!({
        "count": items.len(),
        "reminders": items,
    }))
}

fn complete_reminder(params: &Value) -> Result<Value, AdapterError> {
    let reminder_id = required_string(params, "reminder_id")?;
    let script = format!(
        r#"tell application "Reminders"
  set r to first reminder whose id is "{id}"
  set completed of r to true
end tell
return "completed""#,
        id = applescript_escape(&reminder_id),
    );
    run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "reminder_id": reminder_id,
        "status": "completed",
    }))
}

fn update_reminder(params: &Value) -> Result<Value, AdapterError> {
    let reminder_id = required_string(params, "reminder_id")?;
    let new_title = optional_string(params, "title");
    let new_due = optional_string(params, "due");
    let new_notes = optional_string(params, "notes");

    if new_title.is_none() && new_due.is_none() && new_notes.is_none() {
        return Err(AdapterError::ExecutionFailed(
            "update requires at least one of: title, due, notes".into(),
        ));
    }

    let mut date_prelude = String::new();
    let mut set_lines: Vec<String> = Vec::new();
    if let Some(t) = &new_title {
        set_lines.push(format!("  set name of r to \"{}\"", applescript_escape(t)));
    }
    if let Some(d) = &new_due {
        date_prelude.push_str(&applescript_date_snippet(d, "newDue")?);
        set_lines.push("  set due date of r to newDue".into());
    }
    if let Some(n) = &new_notes {
        set_lines.push(format!("  set body of r to \"{}\"", applescript_escape(n)));
    }
    let set_block = set_lines.join("\n");

    let script = format!(
        r#"tell application "Reminders"
{date_prelude}  set r to first reminder whose id is "{id}"
{set_block}
end tell
return "ok""#,
        date_prelude = date_prelude,
        id = applescript_escape(&reminder_id),
        set_block = set_block,
    );
    run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "reminder_id": reminder_id,
        "updated_fields": {
            "title": new_title.is_some(),
            "due": new_due.is_some(),
            "notes": new_notes.is_some(),
        },
    }))
}

fn delete_reminder(params: &Value) -> Result<Value, AdapterError> {
    let reminder_id = required_string(params, "reminder_id")?;
    let script = format!(
        r#"tell application "Reminders"
  set r to first reminder whose id is "{id}"
  delete r
end tell
return "deleted""#,
        id = applescript_escape(&reminder_id),
    );
    run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "reminder_id": reminder_id,
        "status": "deleted",
        "permanence": "permanent — Reminders has no Recently Deleted folder",
    }))
}

// --- AppleScript helpers ---

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

pub(crate) fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn applescript_date_snippet(iso: &str, var_name: &str) -> Result<String, AdapterError> {
    let (year, month, day, hour, minute, second) = parse_iso_components(iso)?;
    let month_name = month_to_applescript(month)?;
    Ok(format!(
        "  set {var} to current date\n  \
         set day of {var} to 1\n  \
         set month of {var} to {mname}\n  \
         set day of {var} to {day}\n  \
         set year of {var} to {year}\n  \
         set hours of {var} to {hour}\n  \
         set minutes of {var} to {minute}\n  \
         set seconds of {var} to {second}\n",
        var = var_name,
        year = year,
        mname = month_name,
        day = day,
        hour = hour,
        minute = minute,
        second = second,
    ))
}

fn month_to_applescript(month: u32) -> Result<&'static str, AdapterError> {
    let names = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    if !(1..=12).contains(&month) {
        return Err(AdapterError::ExecutionFailed(format!(
            "invalid month: {month}"
        )));
    }
    Ok(names[(month - 1) as usize])
}

pub(crate) fn parse_iso_components(
    iso: &str,
) -> Result<(i32, u32, u32, u32, u32, u32), AdapterError> {
    let s = iso.trim();
    if s.is_empty() {
        return Err(AdapterError::ExecutionFailed("empty ISO datetime".into()));
    }
    let (date_part, time_part_raw) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let time_part = time_part_raw.map(|t| {
        let t = t.trim_end_matches('Z');
        let cut = t
            .char_indices()
            .skip(1)
            .find(|(_, c)| *c == '+' || *c == '-')
            .map(|(i, _)| i);
        match cut {
            Some(i) => &t[..i],
            None => t,
        }
    });
    let date_parts: Vec<&str> = date_part.split('-').collect();
    if date_parts.len() != 3 {
        return Err(AdapterError::ExecutionFailed(format!(
            "invalid ISO date \"{iso}\" (expected YYYY-MM-DD)"
        )));
    }
    let year: i32 = date_parts[0]
        .parse()
        .map_err(|_| AdapterError::ExecutionFailed(format!("invalid year in \"{iso}\"")))?;
    let month: u32 = date_parts[1]
        .parse()
        .map_err(|_| AdapterError::ExecutionFailed(format!("invalid month in \"{iso}\"")))?;
    let day: u32 = date_parts[2]
        .parse()
        .map_err(|_| AdapterError::ExecutionFailed(format!("invalid day in \"{iso}\"")))?;
    let (hour, minute, second) = match time_part {
        Some(t) if !t.is_empty() => {
            let time_parts: Vec<&str> = t.split(':').collect();
            let h: u32 = time_parts.first().and_then(|x| x.parse().ok()).unwrap_or(0);
            let m: u32 = time_parts.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
            let sec_str = time_parts.get(2).copied().unwrap_or("0");
            let sec_int = sec_str.split('.').next().unwrap_or("0");
            let sec: u32 = sec_int.parse().unwrap_or(0);
            (h, m, sec)
        }
        _ => (0, 0, 0),
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(AdapterError::ExecutionFailed(format!(
            "ISO date out of range: \"{iso}\""
        )));
    }
    Ok((year, month, day, hour, minute, second))
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

fn reminders_actions() -> HashMap<String, ActionDeclaration> {
    HashMap::from([
        (
            String::from("add"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("list"), String::from("string (exact list name)")),
                    (String::from("title"), String::from("string")),
                    (String::from("due"), String::from("string? (ISO-8601)")),
                    (String::from("notes"), String::from("string?")),
                ]),
                description: String::from(
                    "Create a reminder in a named list. Returns reminder_id (Apple Reminders URL-style id).",
                ),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("list"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("list"), String::from("string? (default: all lists)")),
                    (String::from("completed"), String::from("boolean? (default false)")),
                    (
                        String::from("due_before"),
                        String::from("string? (ISO-8601 cutoff)"),
                    ),
                    (String::from("limit"), String::from("number? (default 50)")),
                ]),
                description: String::from(
                    "List reminders, default filter completed=false. Returns {reminder_id, title, due, list, completed, notes} per entry.",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("complete"),
            ActionDeclaration {
                params: HashMap::from([(
                    String::from("reminder_id"),
                    String::from("string"),
                )]),
                description: String::from(
                    "Mark a reminder as completed. Reversible via `update` (set completed: false), but only if you remember the id.",
                ),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("update"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("reminder_id"), String::from("string")),
                    (String::from("title"), String::from("string?")),
                    (String::from("due"), String::from("string? (ISO-8601)")),
                    (String::from("notes"), String::from("string?")),
                ]),
                description: String::from(
                    "Update one or more fields of an existing reminder. Requires at least one field to change.",
                ),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("delete"),
            ActionDeclaration {
                params: HashMap::from([(
                    String::from("reminder_id"),
                    String::from("string"),
                )]),
                description: String::from(
                    "Delete a reminder. PERMANENT — Reminders has no Recently Deleted folder; recovery requires Time Machine or iCloud archive backup.",
                ),
                mutates_state: true,
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
    fn applescript_escape_quotes_and_backslashes() {
        assert_eq!(
            applescript_escape(r#"call "Alice"\later"#),
            r#"call \"Alice\"\\later"#
        );
    }

    #[test]
    fn parse_iso_full_datetime() {
        let r = parse_iso_components("2026-05-12T14:30:45").unwrap();
        assert_eq!(r, (2026, 5, 12, 14, 30, 45));
    }

    #[test]
    fn parse_iso_date_only_defaults_to_midnight() {
        let r = parse_iso_components("2026-05-12").unwrap();
        assert_eq!(r, (2026, 5, 12, 0, 0, 0));
    }

    #[test]
    fn applescript_date_snippet_emits_set_statements() {
        let snip = applescript_date_snippet("2026-05-12T09:00:00", "due").unwrap();
        assert!(snip.contains("set due to current date"));
        assert!(snip.contains("set month of due to May"));
        assert!(snip.contains("set day of due to 12"));
        assert!(snip.contains("set hours of due to 9"));
    }

    #[test]
    fn parse_iso_rejects_garbage() {
        assert!(parse_iso_components("not a date").is_err());
        assert!(parse_iso_components("2026-13-01").is_err());
        assert!(parse_iso_components("2026-05-12T25:00:00").is_err());
    }
}
