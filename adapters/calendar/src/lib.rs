//! Apple Calendar adapter (macOS).
//!
//! Provides deterministic event operations through AppleScript — bypasses
//! the focus-loss / cursor-positioning fragility of driving Calendar.app
//! via the GUI. Operations:
//!
//! - `create_event` — create an event in a named calendar. Returns `event_id` (UID).
//! - `list_events` — list events in a date range, optionally scoped to a calendar.
//! - `update_event` — change title/start/end/notes/location on an existing event.
//! - `delete_event` — remove an event by UID.
//!
//! `event_id` is the iCalendar UID — a stable globally-unique string. Updates
//! and deletes find the event by iterating writable calendars until the UID
//! matches. The lookup is O(calendars × events-per-calendar) which is fine
//! for the typical handful of calendars; v2 could cache `uid → calendar`.
//!
//! Quirks discovered during testing (May 2026):
//!
//! 1. **`tell calendar "<name>"` is case-sensitive.** A typo silently falls
//!    through to AppleScript's "no such object" error. The adapter does NOT
//!    pre-validate calendar names; callers should use `list_events` first to
//!    discover available names if uncertain. Use the system Calendar.app
//!    sidebar as the source of truth.
//!
//! 2. **Calendar dates must be built programmatically.** `date "..."` in
//!    AppleScript is locale-dependent — the same string parses differently
//!    on US vs. EU systems. The adapter builds dates via `set year/month/day`
//!    individually. The initial `set day to 1` guards against current-date
//!    being on the 31st when target month has fewer days.
//!
//! 3. **`attendees` is read-mostly.** AppleScript's `make new attendee`
//!    works for events you own but DOES NOT send invitations. The
//!    `attendees` parameter on `create_event` is supported on a best-effort
//!    basis and surfaces a warning in the returned data. Full RSVP /
//!    invitation flow is a v2 task (EventKit framework, not AppleScript).
//!
//! 4. **Deletes are immediate.** Calendar's `delete` removes the event from
//!    the database; iCloud sync propagates within seconds. Recoverable only
//!    from a Time Machine / iCloud archive backup — document as
//!    "irreversible from the adapter's perspective."

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

pub struct CalendarAdapter {
    manifest: AdapterManifest,
}

impl Default for CalendarAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CalendarAdapter {
    pub fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                name: "calendar".into(),
                display_name: "Apple Calendar".into(),
                app_patterns: vec![String::from("(?i)^calendar$")],
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
                    response_timeout_ms: None,
                },
                verification: VerificationDeclaration {
                    truth_surface: String::from("document_model"),
                    readback_action: None,
                    snapshot_action: None,
                },
                actions: calendar_actions(),
            },
        }
    }
}

#[async_trait]
impl AdapterDriver for CalendarAdapter {
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

    async fn execute(
        &self,
        action: &str,
        params: Value,
    ) -> Result<ActionResult, AdapterError> {
        match action {
            "create_event" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(create_event(&params)?),
            }),
            "list_events" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(list_events(&params)?),
            }),
            "update_event" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(update_event(&params)?),
            }),
            "delete_event" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(delete_event(&params)?),
            }),
            _ => Err(AdapterError::ExecutionFailed(format!(
                "Calendar adapter does not expose action \"{action}\""
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

fn create_event(params: &Value) -> Result<Value, AdapterError> {
    let calendar = required_string(params, "calendar")?;
    let title = required_string(params, "title")?;
    let start_iso = required_string(params, "start")?;
    let end_iso = required_string(params, "end")?;
    let notes = optional_string(params, "notes");
    let location = optional_string(params, "location");
    let attendees = collect_string_array(params, "attendees", false)?;

    let start_snip = applescript_date_snippet(&start_iso, "startDate")?;
    let end_snip = applescript_date_snippet(&end_iso, "endDate")?;

    let mut props = format!(
        "summary:\"{title}\", start date:startDate, end date:endDate",
        title = applescript_escape(&title),
    );
    if let Some(n) = &notes {
        props.push_str(&format!(
            ", description:\"{}\"",
            applescript_escape(n)
        ));
    }
    if let Some(l) = &location {
        props.push_str(&format!(
            ", location:\"{}\"",
            applescript_escape(l)
        ));
    }

    let mut attendee_lines = String::new();
    for email in &attendees {
        attendee_lines.push_str(&format!(
            "    try\n      make new attendee at end of attendees with properties {{email:\"{}\"}}\n    end try\n",
            applescript_escape(email)
        ));
    }

    let script = format!(
        r#"tell application "Calendar"
{start_snip}{end_snip}  tell calendar "{calendar}"
    set newEvent to make new event with properties {{{props}}}
    tell newEvent
{attendee_lines}    end tell
    return uid of newEvent
  end tell
end tell"#,
        start_snip = start_snip,
        end_snip = end_snip,
        calendar = applescript_escape(&calendar),
        props = props,
        attendee_lines = attendee_lines,
    );

    let uid = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    let attendees_warning = if attendees.is_empty() {
        None
    } else {
        Some(format!(
            "attendees added to event metadata ({}) but Calendar.app does NOT send invitations via AppleScript. Use the Calendar GUI or EventKit for real invites.",
            attendees.len()
        ))
    };
    Ok(json!({
        "event_id": uid.trim(),
        "calendar": calendar,
        "title": title,
        "attendee_count": attendees.len(),
        "attendees_warning": attendees_warning,
    }))
}

fn list_events(params: &Value) -> Result<Value, AdapterError> {
    let calendar_filter = optional_string(params, "calendar");
    let start_iso = required_string(params, "start")?;
    let end_iso = required_string(params, "end")?;
    let start_snip = applescript_date_snippet(&start_iso, "rangeStart")?;
    let end_snip = applescript_date_snippet(&end_iso, "rangeEnd")?;

    // Either iterate a single calendar by name, or every calendar.
    let calendar_loop_header = match &calendar_filter {
        Some(name) => format!(
            "  set theCalendars to {{calendar \"{}\"}}\n",
            applescript_escape(name)
        ),
        None => "  set theCalendars to every calendar\n".to_string(),
    };

    let script = format!(
        r#"set output to ""
tell application "Calendar"
{start_snip}{end_snip}{cal_loop}  repeat with cal in theCalendars
    set calName to name of cal
    try
      tell cal
        set evts to (every event whose start date is greater than or equal to rangeStart and start date is less than rangeEnd)
        repeat with e in evts
          set theUid to uid of e
          set theTitle to summary of e
          set theStart to (start date of e) as string
          set theEnd to (end date of e) as string
          set theLoc to ""
          try
            set theLoc to location of e
          end try
          set theNotes to ""
          try
            set theNotes to description of e
          end try
          set output to output & theUid & "«FIELD»" & theTitle & "«FIELD»" & theStart & "«FIELD»" & theEnd & "«FIELD»" & calName & "«FIELD»" & theLoc & "«FIELD»" & theNotes & "«ROW»"
        end repeat
      end tell
    end try
  end repeat
end tell
return output"#,
        start_snip = start_snip,
        end_snip = end_snip,
        cal_loop = calendar_loop_header,
    );

    let stdout = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    let mut events: Vec<Value> = Vec::new();
    for row in stdout.split("«ROW»") {
        let trimmed = row.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.splitn(7, "«FIELD»").collect();
        if parts.len() < 7 {
            continue;
        }
        let notes = parts[6].replace('\r', " ").replace('\n', " ");
        events.push(json!({
            "event_id": parts[0].trim(),
            "title": parts[1].trim(),
            "start": parts[2].trim(),
            "end": parts[3].trim(),
            "calendar": parts[4].trim(),
            "location": parts[5].trim(),
            "notes": notes.trim(),
        }));
    }
    Ok(json!({
        "count": events.len(),
        "events": events,
    }))
}

fn update_event(params: &Value) -> Result<Value, AdapterError> {
    let event_id = required_string(params, "event_id")?;
    let new_title = optional_string(params, "title");
    let new_start_iso = optional_string(params, "start");
    let new_end_iso = optional_string(params, "end");
    let new_notes = optional_string(params, "notes");
    let new_location = optional_string(params, "location");

    if new_title.is_none()
        && new_start_iso.is_none()
        && new_end_iso.is_none()
        && new_notes.is_none()
        && new_location.is_none()
    {
        return Err(AdapterError::ExecutionFailed(
            "update_event requires at least one of: title, start, end, notes, location".into(),
        ));
    }

    let mut date_prelude = String::new();
    let mut set_lines: Vec<String> = Vec::new();
    if let Some(t) = &new_title {
        set_lines.push(format!(
            "      set summary of e to \"{}\"",
            applescript_escape(t)
        ));
    }
    if let Some(s) = &new_start_iso {
        date_prelude.push_str(&applescript_date_snippet(s, "newStart")?);
        set_lines.push("      set start date of e to newStart".into());
    }
    if let Some(e_iso) = &new_end_iso {
        date_prelude.push_str(&applescript_date_snippet(e_iso, "newEnd")?);
        set_lines.push("      set end date of e to newEnd".into());
    }
    if let Some(n) = &new_notes {
        set_lines.push(format!(
            "      set description of e to \"{}\"",
            applescript_escape(n)
        ));
    }
    if let Some(l) = &new_location {
        set_lines.push(format!(
            "      set location of e to \"{}\"",
            applescript_escape(l)
        ));
    }
    let set_block = set_lines.join("\n");

    let script = format!(
        r#"tell application "Calendar"
{date_prelude}  set found to false
  repeat with cal in (every calendar)
    try
      tell cal
        set e to first event whose uid is "{uid}"
{set_block}
      end tell
      set found to true
      exit repeat
    end try
  end repeat
  if not found then error "event not found: {uid}"
end tell
return "ok""#,
        date_prelude = date_prelude,
        uid = applescript_escape(&event_id),
        set_block = set_block,
    );
    run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "event_id": event_id,
        "updated_fields": {
            "title": new_title.is_some(),
            "start": new_start_iso.is_some(),
            "end": new_end_iso.is_some(),
            "notes": new_notes.is_some(),
            "location": new_location.is_some(),
        },
    }))
}

fn delete_event(params: &Value) -> Result<Value, AdapterError> {
    let event_id = required_string(params, "event_id")?;
    let script = format!(
        r#"tell application "Calendar"
  set found to false
  repeat with cal in (every calendar)
    try
      tell cal
        set e to first event whose uid is "{uid}"
        delete e
      end tell
      set found to true
      exit repeat
    end try
  end repeat
  if not found then error "event not found: {uid}"
end tell
return "deleted""#,
        uid = applescript_escape(&event_id),
    );
    run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "event_id": event_id,
        "status": "deleted",
        "permanence": "immediate; recoverable only via Time Machine / iCloud archive",
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

pub(crate) fn applescript_date_snippet(
    iso: &str,
    var_name: &str,
) -> Result<String, AdapterError> {
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
        "January", "February", "March", "April", "May", "June", "July", "August",
        "September", "October", "November", "December",
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
    let (date_part, time_part_raw) = match s.find(|c: char| c == 'T' || c == ' ') {
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
            let h: u32 = time_parts
                .first()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
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

fn collect_string_array(
    params: &Value,
    field: &str,
    required: bool,
) -> Result<Vec<String>, AdapterError> {
    let v = match params.get(field) {
        Some(v) => v,
        None => {
            return if required {
                Err(AdapterError::ExecutionFailed(format!(
                    "missing required `{field}` field"
                )))
            } else {
                Ok(Vec::new())
            }
        }
    };
    if let Some(arr) = v.as_array() {
        arr.iter()
            .map(|x| {
                x.as_str()
                    .map(|s| s.to_string())
                    .ok_or_else(|| {
                        AdapterError::ExecutionFailed(format!(
                            "`{field}` array entries must be strings"
                        ))
                    })
            })
            .collect()
    } else if let Some(s) = v.as_str() {
        Ok(vec![s.to_string()])
    } else {
        Err(AdapterError::ExecutionFailed(format!(
            "`{field}` must be a string or array of strings"
        )))
    }
}

fn calendar_actions() -> HashMap<String, ActionDeclaration> {
    HashMap::from([
        (
            String::from("create_event"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("calendar"), String::from("string (exact name)")),
                    (String::from("title"), String::from("string")),
                    (String::from("start"), String::from("string (ISO-8601)")),
                    (String::from("end"), String::from("string (ISO-8601)")),
                    (String::from("attendees"), String::from("string[]? (emails)")),
                    (String::from("notes"), String::from("string?")),
                    (String::from("location"), String::from("string?")),
                ]),
                description: String::from(
                    "Create an event in the named calendar. Returns event_id (the iCalendar UID). Attendees are added to event metadata but Calendar.app does NOT send invitations via AppleScript — use the GUI for real invites.",
                ),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("list_events"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("calendar"), String::from("string? (default: all calendars)")),
                    (String::from("start"), String::from("string (ISO-8601)")),
                    (String::from("end"), String::from("string (ISO-8601)")),
                ]),
                description: String::from(
                    "List events whose start date falls within [start, end). Returns {event_id, title, start, end, calendar, location, notes} per event.",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("update_event"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("event_id"), String::from("string (iCalendar UID)")),
                    (String::from("title"), String::from("string?")),
                    (String::from("start"), String::from("string? (ISO-8601)")),
                    (String::from("end"), String::from("string? (ISO-8601)")),
                    (String::from("notes"), String::from("string?")),
                    (String::from("location"), String::from("string?")),
                ]),
                description: String::from(
                    "Update one or more fields of an existing event. Looks up by UID across all writable calendars. Requires at least one field to change.",
                ),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("delete_event"),
            ActionDeclaration {
                params: HashMap::from([(String::from("event_id"), String::from("string"))]),
                description: String::from(
                    "Delete an event by UID. IRREVERSIBLE from the adapter's perspective — recoverable only from a Time Machine or iCloud archive backup.",
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
            applescript_escape(r#"Q1 "review"\path"#),
            r#"Q1 \"review\"\\path"#
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
    fn parse_iso_rejects_garbage() {
        assert!(parse_iso_components("not a date").is_err());
        assert!(parse_iso_components("2026-13-01").is_err());
        assert!(parse_iso_components("2026-05-12T25:00:00").is_err());
    }

    #[test]
    fn applescript_date_snippet_emits_set_statements() {
        let snip = applescript_date_snippet("2026-05-12T14:30:45", "d").unwrap();
        assert!(snip.contains("set d to current date"));
        assert!(snip.contains("set month of d to May"));
        assert!(snip.contains("set day of d to 12"));
        assert!(snip.contains("set year of d to 2026"));
        assert!(snip.contains("set hours of d to 14"));
        assert!(snip.contains("set minutes of d to 30"));
        assert!(snip.contains("set seconds of d to 45"));
    }

    #[test]
    fn applescript_date_snippet_guards_against_31st_pitfall() {
        // current date may be on the 31st; setting month to Feb without
        // first setting day to 1 would overflow into March. Guard the
        // ordering of set-day-1 → set-month → set-day-target.
        let snip = applescript_date_snippet("2026-02-15T10:00:00", "d").unwrap();
        let first_day = snip.find("set day of d to 1").unwrap();
        let month_set = snip.find("set month of d to February").unwrap();
        let target_day = snip.find("set day of d to 15").unwrap();
        assert!(first_day < month_set);
        assert!(month_set < target_day);
    }

    #[test]
    fn month_name_lookup() {
        assert_eq!(month_to_applescript(1).unwrap(), "January");
        assert_eq!(month_to_applescript(12).unwrap(), "December");
        assert!(month_to_applescript(0).is_err());
        assert!(month_to_applescript(13).is_err());
    }
}
