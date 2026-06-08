//! Apple Mail adapter (macOS).
//!
//! Provides deterministic mail operations through AppleScript — compose
//! drafts (without sending), explicitly send them, list and read inbox
//! messages, and search. Bypasses the focus-loss / keystroke-fragility
//! problems that plague external agents driving Mail via cursor positioning.
//!
//! Operations:
//!
//! - `compose` — creates an outgoing message; DOES NOT send. Returns `draft_id`.
//! - `send_draft` — sends a previously-composed draft. **Irreversible.**
//! - `list_inbox` — lists recent inbox messages with snippet.
//! - `read_message` — reads body + recipients of a specific message.
//! - `search` — substring match against subject + sender across the inbox.
//!
//! Safety: `send_draft` is irreversible — once the SMTP queue accepts it
//! there's no recall. The adapter deliberately does NOT expose a
//! "compose_and_send" convenience; every send takes two MCP round-trips so
//! the host's confirmation policy has a place to intervene before the wire.
//!
//! Quirks discovered during testing (May 2026):
//!
//! 1. **`outgoing message id` is a memory-resident handle.** The id of a
//!    newly-created `outgoing message` is valid only while Mail.app holds
//!    the in-memory reference. If Mail is quit (or the user manually closes
//!    the hidden compose window) between `compose` and `send_draft`, the id
//!    becomes invalid and `send_draft` errors. For the typical agent flow
//!    (compose → send_draft within seconds, same MCP session) this is fine.
//!
//! 2. **`whose date received > date X` is locale-fragile.** Building the
//!    AppleScript date programmatically (`set year`, `set month`, …) avoids
//!    the locale-dependent `date "..."` parser. The adapter does this
//!    internally when `since` is provided. The first `set day to 1` is a
//!    guard against current-date being on the 31st when the target month
//!    has fewer days (a well-known AppleScript pitfall).
//!
//! 3. **Inbox is the unified inbox.** Multi-account setups merge here. The
//!    `mailbox` parameter on `search` is accepted but not honored in v1 —
//!    always queries the unified inbox. Per-account inbox addressing
//!    (`inbox of account "iCloud"`) is a v2 task.
//!
//! 4. **Snippet extraction iterates `content`.** Pulling full content of
//!    every message is several seconds for 100+ messages. The adapter pulls
//!    just the first 200 chars per message and `limit` (default 20) keeps
//!    the iteration bounded. Use `read_message` for full body.

#![cfg(target_os = "macos")]

use std::collections::HashMap;

use async_trait::async_trait;
use cel_adapter_sdk::{
    ActionDeclaration, ActionResult, AdapterDriver, AdapterError, AdapterManifest,
    ContextDeclaration,
};
use cel_adapter_sdk::{LifecycleDeclaration, VerificationDeclaration};
use cel_context::ContextElement;
use serde_json::{json, Value};
use std::process::Command;

pub struct MailAdapter {
    manifest: AdapterManifest,
}

impl Default for MailAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MailAdapter {
    pub fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                name: "mail".into(),
                display_name: "Apple Mail".into(),
                app_patterns: vec![String::from("(?i)^mail$")],
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
                // Mail operations are pure AppleScript and don't need Mail.app to
                // be frontmost. background_refresh=true + requires_frontmost=false
                // keeps the adapter Active whenever probe()=true so MCP callers
                // can issue mail.compose/etc. without an "adapter inactive" error.
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
                actions: mail_actions(),
            },
        }
    }
}

#[async_trait]
impl AdapterDriver for MailAdapter {
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
            "compose" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(compose_message(&params)?),
            }),
            "send_draft" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(send_draft(&params)?),
            }),
            "list_inbox" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(list_inbox(&params)?),
            }),
            "read_message" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(read_message(&params)?),
            }),
            "search" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(search_messages(&params)?),
            }),
            _ => Err(AdapterError::ExecutionFailed(format!(
                "Mail adapter does not expose action \"{action}\""
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

fn compose_message(params: &Value) -> Result<Value, AdapterError> {
    let to_list = collect_string_array(params, "to", true)?;
    let cc_list = collect_string_array(params, "cc", false)?;
    let bcc_list = collect_string_array(params, "bcc", false)?;
    let subject = required_string(params, "subject")?;
    let body = required_string(params, "body")?;
    let attachments = collect_string_array(params, "attachments", false)?;
    let visible = params
        .get("visible")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut recipient_lines = String::new();
    for addr in &to_list {
        recipient_lines.push_str(&format!(
            "    make new to recipient at end of to recipients with properties {{address:\"{}\"}}\n",
            applescript_escape(addr)
        ));
    }
    for addr in &cc_list {
        recipient_lines.push_str(&format!(
            "    make new cc recipient at end of cc recipients with properties {{address:\"{}\"}}\n",
            applescript_escape(addr)
        ));
    }
    for addr in &bcc_list {
        recipient_lines.push_str(&format!(
            "    make new bcc recipient at end of bcc recipients with properties {{address:\"{}\"}}\n",
            applescript_escape(addr)
        ));
    }

    let mut attachment_lines = String::new();
    for path in &attachments {
        attachment_lines.push_str(&format!(
            "  tell content of theMsg to make new attachment with properties {{file name:(POSIX file \"{}\")}} at after the last paragraph\n",
            applescript_escape(path)
        ));
    }

    let script = format!(
        r#"tell application "Mail"
  set theMsg to make new outgoing message with properties {{subject:"{subject}", content:"{body}", visible:{visible}}}
  tell theMsg
{recipients}  end tell
{attachments}  return id of theMsg as string
end tell"#,
        subject = applescript_escape(&subject),
        body = applescript_escape(&body),
        visible = if visible { "true" } else { "false" },
        recipients = recipient_lines,
        attachments = attachment_lines,
    );
    let id = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "draft_id": id.trim(),
        "subject": subject,
        "to_count": to_list.len(),
        "cc_count": cc_list.len(),
        "bcc_count": bcc_list.len(),
        "attachment_count": attachments.len(),
    }))
}

fn send_draft(params: &Value) -> Result<Value, AdapterError> {
    let draft_id = required_string(params, "draft_id")?;
    let draft_int = parse_mail_id(&draft_id, "draft_id")?;
    let script = format!(
        r#"tell application "Mail"
  send (outgoing message id {id})
end tell"#,
        id = draft_int,
    );
    run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    Ok(json!({
        "draft_id": draft_id,
        "status": "sent",
        "warning": "send is irreversible — message handed to SMTP queue",
    }))
}

fn list_inbox(params: &Value) -> Result<Value, AdapterError> {
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(20);
    let since = optional_string(params, "since");
    let from_filter = optional_string(params, "from");

    let mut whose_clauses: Vec<String> = Vec::new();
    let mut prelude = String::new();
    if let Some(s) = &since {
        let snippet = applescript_date_snippet(s, "sinceDate")?;
        prelude.push_str(&snippet);
        whose_clauses.push("date received > sinceDate".to_string());
    }
    if let Some(f) = &from_filter {
        whose_clauses.push(format!("sender contains \"{}\"", applescript_escape(f)));
    }
    let messages_expr = if whose_clauses.is_empty() {
        "messages of inbox".to_string()
    } else {
        format!("(messages of inbox whose {})", whose_clauses.join(" and "))
    };

    let script = format!(
        r#"set output to ""
tell application "Mail"
{prelude}  set theMessages to {messages_expr}
  set i to 0
  repeat with m in theMessages
    if i is greater than or equal to {limit} then exit repeat
    set theId to (id of m) as string
    set theSubject to subject of m
    set theSender to sender of m
    set theDate to (date received of m) as string
    set theContent to ""
    try
      set theContent to content of m
    end try
    if length of theContent > 200 then
      set theContent to text 1 thru 200 of theContent
    end if
    set output to output & theId & "«FIELD»" & theSubject & "«FIELD»" & theSender & "«FIELD»" & theDate & "«FIELD»" & theContent & "«ROW»"
    set i to i + 1
  end repeat
end tell
return output"#,
        prelude = prelude,
        messages_expr = messages_expr,
        limit = limit,
    );
    let stdout = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;

    let mut items: Vec<Value> = Vec::new();
    for row in stdout.split("«ROW»") {
        let trimmed = row.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.splitn(5, "«FIELD»").collect();
        if parts.len() < 5 {
            continue;
        }
        let snippet = parts[4].replace(['\r', '\n'], " ");
        items.push(json!({
            "message_id": parts[0].trim(),
            "subject": parts[1].trim(),
            "sender": parts[2].trim(),
            "received_at": parts[3].trim(),
            "snippet": snippet.trim(),
        }));
    }
    Ok(json!({
        "count": items.len(),
        "messages": items,
    }))
}

fn read_message(params: &Value) -> Result<Value, AdapterError> {
    let message_id = required_string(params, "message_id")?;
    let id_int = parse_mail_id(&message_id, "message_id")?;
    // v1 always looks up by integer id within the unified inbox; the same
    // id in other mailboxes would need cross-mailbox iteration. Use
    // `search` to discover messages outside the inbox.
    let script = format!(
        r#"tell application "Mail"
  set m to first message of inbox whose id is {id}
  set theSubject to subject of m
  set theSender to sender of m
  set theDate to (date received of m) as string
  set theContent to content of m
  set toList to ""
  try
    repeat with r in (to recipients of m)
      set toList to toList & (address of r) & ", "
    end repeat
  end try
  set ccList to ""
  try
    repeat with r in (cc recipients of m)
      set ccList to ccList & (address of r) & ", "
    end repeat
  end try
  return theSubject & "«FIELD»" & theSender & "«FIELD»" & theDate & "«FIELD»" & toList & "«FIELD»" & ccList & "«FIELD»" & theContent
end tell"#,
        id = id_int,
    );
    let stdout = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;
    let parts: Vec<&str> = stdout.splitn(6, "«FIELD»").collect();
    if parts.len() < 6 {
        return Err(AdapterError::ExecutionFailed(
            "Failed to parse Mail.read_message AppleScript response".into(),
        ));
    }
    Ok(json!({
        "message_id": message_id,
        "subject": parts[0].trim().to_string(),
        "sender": parts[1].trim().to_string(),
        "received_at": parts[2].trim().to_string(),
        "recipients": {
            "to": parts[3].trim().trim_end_matches(',').trim().to_string(),
            "cc": parts[4].trim().trim_end_matches(',').trim().to_string(),
        },
        "body": parts[5].trim_end().to_string(),
    }))
}

fn search_messages(params: &Value) -> Result<Value, AdapterError> {
    let query = required_string(params, "query")?;
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(20);

    // v1 ignores `mailbox` parameter; always searches the unified inbox.
    // See module-level doc Quirk 3.
    let script = format!(
        r#"set output to ""
tell application "Mail"
  set theMessages to messages of inbox whose (subject contains "{q}" or sender contains "{q}")
  set i to 0
  repeat with m in theMessages
    if i is greater than or equal to {limit} then exit repeat
    set theId to (id of m) as string
    set theSubject to subject of m
    set theSender to sender of m
    set theDate to (date received of m) as string
    set output to output & theId & "«FIELD»" & theSubject & "«FIELD»" & theSender & "«FIELD»" & theDate & "«ROW»"
    set i to i + 1
  end repeat
end tell
return output"#,
        q = applescript_escape(&query),
        limit = limit,
    );
    let stdout = run_osascript(&script).map_err(AdapterError::ExecutionFailed)?;

    let mut items: Vec<Value> = Vec::new();
    for row in stdout.split("«ROW»") {
        let trimmed = row.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.splitn(4, "«FIELD»").collect();
        if parts.len() < 4 {
            continue;
        }
        items.push(json!({
            "message_id": parts[0].trim(),
            "subject": parts[1].trim(),
            "sender": parts[2].trim(),
            "received_at": parts[3].trim(),
        }));
    }
    Ok(json!({
        "query": query,
        "count": items.len(),
        "messages": items,
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

/// Escape for safe interpolation into an AppleScript double-quoted string
/// literal. AppleScript needs backslash-escaping for `"` and `\` only.
pub(crate) fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Build an AppleScript snippet that constructs an AppleScript date from an
/// ISO-8601 datetime string and binds it to a local variable. Returns just
/// the snippet — the caller references the bound variable by name.
///
/// Sets each date component individually (year/month/day/hour/minute/second)
/// rather than relying on `date "..."` which is locale-dependent. The
/// initial `set day to 1` guards against current-date being on the 31st
/// when the target month has fewer days (a well-known AppleScript pitfall).
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

/// Parse an ISO-8601 datetime string into `(year, month, day, hour, min, sec)`.
///
/// Accepts:
/// - `YYYY-MM-DD`              (hour/min/sec = 0)
/// - `YYYY-MM-DDTHH:MM`        (sec = 0)
/// - `YYYY-MM-DDTHH:MM:SS`
/// - `YYYY-MM-DD HH:MM:SS`     (space separator)
///
/// Trailing `Z` or `±HH:MM` timezone offsets are accepted and ignored —
/// the resulting date is interpreted as local time by AppleScript.
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
        // Strip trailing timezone offset (+HH:MM or -HH:MM). Skip index 0
        // so a leading sign on the time itself (impossible in ISO-8601,
        // but cheap to guard) doesn't truncate.
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
            // Drop fractional seconds (e.g. "30.500")
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

/// Accept either a single string or an array of strings. When `required` is
/// true, errors if the field is missing or empty.
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
    let items: Vec<String> = if let Some(arr) = v.as_array() {
        arr.iter()
            .map(|x| {
                x.as_str().map(|s| s.to_string()).ok_or_else(|| {
                    AdapterError::ExecutionFailed(format!(
                        "`{field}` array entries must be strings"
                    ))
                })
            })
            .collect::<Result<_, _>>()?
    } else if let Some(s) = v.as_str() {
        vec![s.to_string()]
    } else {
        return Err(AdapterError::ExecutionFailed(format!(
            "`{field}` must be a string or array of strings"
        )));
    };
    if required && items.is_empty() {
        return Err(AdapterError::ExecutionFailed(format!(
            "`{field}` must have at least one entry"
        )));
    }
    Ok(items)
}

fn parse_mail_id(raw: &str, field: &str) -> Result<i64, AdapterError> {
    raw.trim().parse::<i64>().map_err(|_| {
        AdapterError::ExecutionFailed(format!(
            "`{field}` must be a numeric Mail id (got \"{raw}\")"
        ))
    })
}

fn mail_actions() -> HashMap<String, ActionDeclaration> {
    HashMap::from([
        (
            String::from("compose"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("to"), String::from("string|string[] (addresses)")),
                    (String::from("cc"), String::from("string|string[]?")),
                    (String::from("bcc"), String::from("string|string[]?")),
                    (String::from("subject"), String::from("string")),
                    (String::from("body"), String::from("string (plain text)")),
                    (
                        String::from("attachments"),
                        String::from("string[]? (POSIX file paths)"),
                    ),
                    (
                        String::from("visible"),
                        String::from("boolean? (default false — compose window hidden)"),
                    ),
                ]),
                description: String::from(
                    "Create an outgoing message. DOES NOT SEND. Returns draft_id for use with send_draft. Body is plain text in v1; HTML support is a v2 task.",
                ),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("send_draft"),
            ActionDeclaration {
                params: HashMap::from([(String::from("draft_id"), String::from("string"))]),
                description: String::from(
                    "Send a previously-composed draft. IRREVERSIBLE — once accepted by the SMTP queue there is no recall. Deliberately a separate op from compose so the host's confirmation policy can intervene before the wire.",
                ),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("list_inbox"),
            ActionDeclaration {
                params: HashMap::from([
                    (
                        String::from("since"),
                        String::from("string? (ISO-8601 datetime)"),
                    ),
                    (
                        String::from("from"),
                        String::from("string? (substring match on sender)"),
                    ),
                    (String::from("limit"), String::from("number? (default 20)")),
                ]),
                description: String::from(
                    "List recent messages from the unified inbox. Returns {message_id, subject, sender, received_at, snippet}. snippet is the first 200 chars of body.",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("read_message"),
            ActionDeclaration {
                params: HashMap::from([(
                    String::from("message_id"),
                    String::from("string (Mail integer id)"),
                )]),
                description: String::from(
                    "Read body + recipients of a message in the inbox. message_id is the integer id from list_inbox/search. v1 only looks up within the unified inbox.",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("search"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("query"), String::from("string")),
                    (
                        String::from("mailbox"),
                        String::from("string? (v1 ignores — always inbox)"),
                    ),
                    (String::from("limit"), String::from("number? (default 20)")),
                ]),
                description: String::from(
                    "Substring match against subject + sender in the unified inbox. v1 ignores `mailbox`.",
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
    fn applescript_escape_quotes_and_backslashes() {
        assert_eq!(
            applescript_escape(r#"he said "hi"\n"#),
            r#"he said \"hi\"\\n"#
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
    fn parse_iso_strips_z_suffix() {
        let r = parse_iso_components("2026-05-12T14:30:00Z").unwrap();
        assert_eq!(r, (2026, 5, 12, 14, 30, 0));
    }

    #[test]
    fn parse_iso_strips_numeric_tz_suffix() {
        let r = parse_iso_components("2026-05-12T14:30:00+05:30").unwrap();
        assert_eq!(r, (2026, 5, 12, 14, 30, 0));
        let r = parse_iso_components("2026-05-12T14:30:00-08:00").unwrap();
        assert_eq!(r, (2026, 5, 12, 14, 30, 0));
    }

    #[test]
    fn parse_iso_drops_fractional_seconds() {
        let r = parse_iso_components("2026-05-12T14:30:45.500").unwrap();
        assert_eq!(r, (2026, 5, 12, 14, 30, 45));
    }

    #[test]
    fn parse_iso_accepts_space_separator() {
        let r = parse_iso_components("2026-05-12 14:30:00").unwrap();
        assert_eq!(r, (2026, 5, 12, 14, 30, 0));
    }

    #[test]
    fn parse_iso_rejects_garbage() {
        assert!(parse_iso_components("not a date").is_err());
        assert!(parse_iso_components("2026-13-01").is_err());
        assert!(parse_iso_components("2026-05-12T25:00:00").is_err());
        assert!(parse_iso_components("").is_err());
    }

    #[test]
    fn applescript_date_snippet_emits_set_statements() {
        let snip = applescript_date_snippet("2026-05-12T14:30:45", "myDate").unwrap();
        assert!(snip.contains("set myDate to current date"));
        assert!(snip.contains("set month of myDate to May"));
        assert!(snip.contains("set day of myDate to 12"));
        assert!(snip.contains("set year of myDate to 2026"));
        assert!(snip.contains("set hours of myDate to 14"));
        assert!(snip.contains("set minutes of myDate to 30"));
        assert!(snip.contains("set seconds of myDate to 45"));
    }

    #[test]
    fn applescript_date_snippet_sets_day_to_one_first() {
        // Guards against current-date-31st landing in a month with fewer
        // days (Feb, April, ...). The first `set day to 1` defuses this.
        let snip = applescript_date_snippet("2026-02-15T10:00:00", "d").unwrap();
        let first_day_idx = snip.find("set day of d to 1").unwrap();
        let target_day_idx = snip.find("set day of d to 15").unwrap();
        assert!(first_day_idx < target_day_idx);
        // And the day-to-1 must come before the month assignment, otherwise
        // the guard does nothing.
        let month_idx = snip.find("set month of d to February").unwrap();
        assert!(first_day_idx < month_idx);
    }

    #[test]
    fn collect_string_array_accepts_string_or_array() {
        let p = json!({"to": "alice@example.com"});
        assert_eq!(
            collect_string_array(&p, "to", true).unwrap(),
            vec!["alice@example.com"]
        );

        let p = json!({"to": ["alice@example.com", "bob@example.com"]});
        assert_eq!(
            collect_string_array(&p, "to", true).unwrap(),
            vec!["alice@example.com", "bob@example.com"]
        );

        let p = json!({});
        assert!(collect_string_array(&p, "to", true).is_err());
        assert_eq!(
            collect_string_array(&p, "cc", false).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn parse_mail_id_rejects_non_numeric() {
        assert!(parse_mail_id("abc", "draft_id").is_err());
        assert_eq!(parse_mail_id("12345", "draft_id").unwrap(), 12345);
        assert_eq!(parse_mail_id("  9876  ", "draft_id").unwrap(), 9876);
    }
}
