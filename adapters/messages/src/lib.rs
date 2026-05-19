//! Apple Messages adapter (macOS, READ-ONLY in v1).
//!
//! Reads the Messages chat database at `~/Library/Messages/chat.db` directly
//! via SQLite — different shape than the other AppleScript-backed adapters
//! (mail, calendar, reminders, notes). Messages.app exposes a paper-thin
//! AppleScript surface that misses thread structure, history, and snippet
//! data; the SQLite path is far richer and entirely read-only.
//!
//! Operations:
//!
//! - `list_threads` — list recent chats with last message timestamp + snippet.
//! - `read_thread` — read recent messages in a thread.
//! - `search` — substring match against message text across all threads.
//!
//! **Send is deliberately out of scope in v1.** Sending iMessages reliably
//! requires either AppleScript-with-entitlements (fragile, no consent gate)
//! or Messages.framework (requires private entitlements signed by Apple).
//! Both are too risky to ship without explicit user consent plumbing.
//! Every op in this adapter declares `mutates_state: false`.
//!
//! Permissions: the host process must have Full Disk Access. Without it,
//! `~/Library/Messages/chat.db` returns a permission-denied error. Grant via
//! System Settings → Privacy & Security → Full Disk Access, then restart the
//! host (Terminal / Claude Code / Cursor / etc.).
//!
//! Quirks discovered during testing (May 2026):
//!
//! 1. **Apple Epoch is in nanoseconds on modern macOS, seconds on legacy.**
//!    `message.date` is the count since 2001-01-01T00:00:00Z. Big Sur+
//!    stores nanoseconds, pre-Big Sur stores seconds. The adapter's SQL
//!    uses `CASE WHEN date > 1e15` to detect and normalize. To Unix epoch:
//!    `apple_seconds + 978307200`.
//!
//! 2. **`message.text` is NULL for rich-content messages.** Messages
//!    composed with URL previews, stickers, Tapbacks, or attachments store
//!    their text inside a binary `attributedBody` NSKeyedArchiver blob. The
//!    adapter does NOT decode this in v1 — affected messages surface with
//!    `body == ""`. Decoding the blob requires NSKeyedUnarchiver / typedstream
//!    parsing; v2 task.
//!
//! 3. **`chat.guid` is the stable thread identifier.** Format examples:
//!    `iMessage;-;+15551234567` (1:1), `iMessage;+;chat<digits>` (group),
//!    `SMS;-;+15551234567` (SMS fallback). The adapter uses `guid` as
//!    `thread_id`; `chat.ROWID` is unstable across database compaction.
//!
//! 4. **Search uses SQL LIKE — case-insensitive ASCII only by default.**
//!    SQLite's `LIKE` ignores case for ASCII but not for Unicode. Searching
//!    for "café" finds "Café" only if both have the same ASCII fold.
//!    Workable for the common case; document the limitation.

#![cfg(target_os = "macos")]

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use cel_context::ContextElement;
use cel_cortex::adapter::{LifecycleDeclaration, VerificationDeclaration};
use cel_cortex::{
    ActionDeclaration, ActionResult, AdapterDriver, AdapterError, AdapterManifest,
    ContextDeclaration,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

pub struct MessagesAdapter {
    manifest: AdapterManifest,
}

impl Default for MessagesAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MessagesAdapter {
    pub fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                name: "messages".into(),
                display_name: "Apple Messages (read-only)".into(),
                app_patterns: vec![String::from("(?i)^messages$")],
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
                // SQLite reads don't need Messages.app to be frontmost or
                // even running — the database file is independent.
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
                actions: messages_actions(),
            },
        }
    }
}

#[async_trait]
impl AdapterDriver for MessagesAdapter {
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
            "list_threads" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(list_threads(&params)?),
            }),
            "read_thread" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(read_thread(&params)?),
            }),
            "search" => Ok(ActionResult {
                success: true,
                error: None,
                data: Some(search_messages(&params)?),
            }),
            _ => Err(AdapterError::ExecutionFailed(format!(
                "Messages adapter does not expose action \"{action}\" (note: send is out of scope in v1)"
            ))),
        }
    }

    async fn probe(&self) -> bool {
        chat_db_path().exists()
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

fn list_threads(params: &Value) -> Result<Value, AdapterError> {
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(20);

    let conn = open_chat_db()?;

    // Compute last_message_at (apple_epoch normalized to ISO via SQLite
    // strftime) and last_snippet (text of the most-recent message) for
    // each chat. Threads with no messages are skipped (lhs of MAX is NULL).
    //
    // The `1e15` threshold detects nanoseconds-vs-seconds: 1e15 ns ≈ year
    // 2032 in seconds, comfortably past current dates, and 1e15 s would
    // be ~year 31.5M AD. Either bound is safe.
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
              c.guid,
              c.chat_identifier,
              c.display_name,
              strftime(
                '%Y-%m-%dT%H:%M:%SZ',
                CASE WHEN last_date > 1000000000000000
                     THEN last_date / 1000000000 + 978307200
                     ELSE last_date + 978307200
                END,
                'unixepoch'
              ) AS last_message_at,
              last_text,
              (
                SELECT GROUP_CONCAT(h.id, ', ')
                FROM chat_handle_join chj
                JOIN handle h ON h.ROWID = chj.handle_id
                WHERE chj.chat_id = c.ROWID
              ) AS participants_csv
            FROM (
              SELECT
                cmj.chat_id AS chat_id,
                MAX(m.date) AS last_date,
                (
                  SELECT m2.text
                  FROM chat_message_join cmj2
                  JOIN message m2 ON m2.ROWID = cmj2.message_id
                  WHERE cmj2.chat_id = cmj.chat_id
                  ORDER BY m2.date DESC
                  LIMIT 1
                ) AS last_text
              FROM chat_message_join cmj
              JOIN message m ON m.ROWID = cmj.message_id
              GROUP BY cmj.chat_id
            ) AS recent
            JOIN chat c ON c.ROWID = recent.chat_id
            WHERE last_date IS NOT NULL
            ORDER BY last_date DESC
            LIMIT ?1
            "#,
        )
        .map_err(sql_err)?;

    let rows = stmt
        .query_map([limit], |row| {
            let guid: String = row.get(0)?;
            let chat_identifier: Option<String> = row.get(1)?;
            let display_name: Option<String> = row.get(2)?;
            let last_message_at: Option<String> = row.get(3)?;
            let last_text: Option<String> = row.get(4)?;
            let participants_csv: Option<String> = row.get(5)?;
            Ok((
                guid,
                chat_identifier,
                display_name,
                last_message_at,
                last_text,
                participants_csv,
            ))
        })
        .map_err(sql_err)?;

    let mut threads: Vec<Value> = Vec::new();
    for row in rows {
        let (guid, chat_identifier, display_name, last_message_at, last_text, participants_csv) =
            row.map_err(sql_err)?;
        let participants: Vec<String> = participants_csv
            .as_deref()
            .map(|s| {
                s.split(", ")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let snippet = last_text
            .as_deref()
            .map(|t| truncate_to_chars(&normalize_whitespace(t), 200))
            .unwrap_or_default();
        threads.push(json!({
            "thread_id": guid,
            "chat_identifier": chat_identifier,
            "display_name": display_name,
            "last_message_at": last_message_at,
            "last_snippet": snippet,
            "participants": participants,
        }));
    }
    Ok(json!({
        "count": threads.len(),
        "threads": threads,
    }))
}

fn read_thread(params: &Value) -> Result<Value, AdapterError> {
    let thread_id = required_string(params, "thread_id")?;
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(50);

    let conn = open_chat_db()?;

    let chat_rowid: i64 = conn
        .query_row(
            "SELECT ROWID FROM chat WHERE guid = ?1 LIMIT 1",
            [&thread_id],
            |row| row.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                AdapterError::ExecutionFailed(format!("thread_id not found: {thread_id}"))
            }
            other => sql_err(other),
        })?;

    let mut stmt = conn
        .prepare(
            r#"
            SELECT
              m.ROWID AS message_rowid,
              COALESCE(h.id, 'me') AS sender,
              strftime(
                '%Y-%m-%dT%H:%M:%SZ',
                CASE WHEN m.date > 1000000000000000
                     THEN m.date / 1000000000 + 978307200
                     ELSE m.date + 978307200
                END,
                'unixepoch'
              ) AS sent_at,
              m.text,
              m.is_from_me
            FROM chat_message_join cmj
            JOIN message m ON m.ROWID = cmj.message_id
            LEFT JOIN handle h ON h.ROWID = m.handle_id
            WHERE cmj.chat_id = ?1
            ORDER BY m.date DESC
            LIMIT ?2
            "#,
        )
        .map_err(sql_err)?;

    let rows = stmt
        .query_map(rusqlite::params![chat_rowid, limit], |row| {
            let rowid: i64 = row.get(0)?;
            let sender: String = row.get(1)?;
            let sent_at: Option<String> = row.get(2)?;
            let text: Option<String> = row.get(3)?;
            let is_from_me: i64 = row.get(4)?;
            Ok((rowid, sender, sent_at, text, is_from_me != 0))
        })
        .map_err(sql_err)?;

    let mut messages: Vec<Value> = Vec::new();
    for row in rows {
        let (rowid, sender, sent_at, text, is_outgoing) = row.map_err(sql_err)?;
        let body = text.unwrap_or_default();
        messages.push(json!({
            "message_rowid": rowid,
            "from": if is_outgoing { "me".to_string() } else { sender },
            "sent_at": sent_at,
            "body": body,
            "is_outgoing": is_outgoing,
        }));
    }
    // Re-order most-recent-last for human reading consistency.
    messages.reverse();
    Ok(json!({
        "thread_id": thread_id,
        "count": messages.len(),
        "messages": messages,
    }))
}

fn search_messages(params: &Value) -> Result<Value, AdapterError> {
    let query = required_string(params, "query")?;
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .unwrap_or(20);

    let conn = open_chat_db()?;

    let pattern = format!("%{}%", sql_like_escape(&query));

    let mut stmt = conn
        .prepare(
            r#"
            SELECT
              m.ROWID AS message_rowid,
              c.guid AS thread_id,
              COALESCE(h.id, 'me') AS sender,
              strftime(
                '%Y-%m-%dT%H:%M:%SZ',
                CASE WHEN m.date > 1000000000000000
                     THEN m.date / 1000000000 + 978307200
                     ELSE m.date + 978307200
                END,
                'unixepoch'
              ) AS sent_at,
              m.text,
              m.is_from_me
            FROM message m
            LEFT JOIN chat_message_join cmj ON cmj.message_id = m.ROWID
            LEFT JOIN chat c ON c.ROWID = cmj.chat_id
            LEFT JOIN handle h ON h.ROWID = m.handle_id
            WHERE m.text LIKE ?1 ESCAPE '\'
            ORDER BY m.date DESC
            LIMIT ?2
            "#,
        )
        .map_err(sql_err)?;

    let rows = stmt
        .query_map(rusqlite::params![pattern, limit], |row| {
            let rowid: i64 = row.get(0)?;
            let thread_id: Option<String> = row.get(1)?;
            let sender: String = row.get(2)?;
            let sent_at: Option<String> = row.get(3)?;
            let text: Option<String> = row.get(4)?;
            let is_from_me: i64 = row.get(5)?;
            Ok((rowid, thread_id, sender, sent_at, text, is_from_me != 0))
        })
        .map_err(sql_err)?;

    let mut results: Vec<Value> = Vec::new();
    for row in rows {
        let (rowid, thread_id, sender, sent_at, text, is_outgoing) = row.map_err(sql_err)?;
        let body = text.unwrap_or_default();
        results.push(json!({
            "message_rowid": rowid,
            "thread_id": thread_id,
            "from": if is_outgoing { "me".to_string() } else { sender },
            "sent_at": sent_at,
            "body": body,
            "is_outgoing": is_outgoing,
        }));
    }
    Ok(json!({
        "query": query,
        "count": results.len(),
        "messages": results,
    }))
}

// --- SQLite helpers ---

fn chat_db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    PathBuf::from(home).join("Library/Messages/chat.db")
}

fn open_chat_db() -> Result<Connection, AdapterError> {
    let path = chat_db_path();
    if !path.exists() {
        return Err(AdapterError::ExecutionFailed(format!(
            "Messages database not found at {} — Messages.app has never run on this Mac, or HOME is misconfigured",
            path.display()
        )));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    Connection::open_with_flags(&path, flags).map_err(|e| {
        AdapterError::ExecutionFailed(format!(
            "Cannot open Messages database at {}: {e}. macOS likely requires Full Disk Access for the host process — grant via System Settings → Privacy & Security → Full Disk Access, then restart the host.",
            path.display()
        ))
    })
}

fn sql_err(e: rusqlite::Error) -> AdapterError {
    AdapterError::ExecutionFailed(format!("SQLite error: {e}"))
}

/// Escape LIKE pattern metachars `%`, `_`, and `\` with `\` so user query
/// strings substring-match literally. Pair with `ESCAPE '\'` in the SQL.
pub(crate) fn sql_like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Truncate a string to at most `max` characters (NOT bytes), preserving
/// UTF-8 boundaries. Multibyte sequences (emoji, CJK) are kept whole.
pub(crate) fn truncate_to_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Collapse runs of whitespace (newline, tab, CR, repeated spaces) into a
/// single space. Used for snippet rendering so a multi-line message doesn't
/// blow out the list view.
pub(crate) fn normalize_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_was_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
        } else {
            out.push(c);
            prev_was_space = false;
        }
    }
    out.trim().to_string()
}

fn required_string(params: &Value, field: &str) -> Result<String, AdapterError> {
    params
        .get(field)
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .ok_or_else(|| AdapterError::ExecutionFailed(format!("missing `{field}` string field")))
}

fn messages_actions() -> HashMap<String, ActionDeclaration> {
    HashMap::from([
        (
            String::from("list_threads"),
            ActionDeclaration {
                params: HashMap::from([(String::from("limit"), String::from("number? (default 20)"))]),
                description: String::from(
                    "List recent message threads. Returns {thread_id (chat.guid), display_name, last_message_at (ISO-8601), last_snippet, participants}. Read-only.",
                ),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        ),
        (
            String::from("read_thread"),
            ActionDeclaration {
                params: HashMap::from([
                    (String::from("thread_id"), String::from("string (chat.guid)")),
                    (String::from("limit"), String::from("number? (default 50)")),
                ]),
                description: String::from(
                    "Read recent messages in a thread (most-recent first in storage, oldest-first in result). Returns {from, sent_at, body, is_outgoing}. Read-only. Body may be empty for rich-content messages (URL preview, attachments) — see adapter docs § Quirk 2.",
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
                    (String::from("query"), String::from("string (substring)")),
                    (String::from("limit"), String::from("number? (default 20)")),
                ]),
                description: String::from(
                    "Substring search against message text across all threads. Case-insensitive for ASCII only. Returns {message_rowid, thread_id, from, sent_at, body, is_outgoing}. Read-only.",
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
    fn sql_like_escape_handles_metachars() {
        assert_eq!(sql_like_escape("hello"), "hello");
        assert_eq!(sql_like_escape("100%"), r"100\%");
        assert_eq!(sql_like_escape("a_b"), r"a\_b");
        assert_eq!(sql_like_escape(r"back\slash"), r"back\\slash");
        assert_eq!(sql_like_escape("a%_b\\c"), r"a\%\_b\\c");
    }

    #[test]
    fn truncate_to_chars_preserves_utf8_boundaries() {
        let emoji = "👋🌍🎉";
        assert_eq!(truncate_to_chars(emoji, 2), "👋🌍");
        assert_eq!(truncate_to_chars(emoji, 10), emoji);
        // CJK
        assert_eq!(truncate_to_chars("你好世界", 2), "你好");
    }

    #[test]
    fn normalize_whitespace_collapses_runs() {
        assert_eq!(normalize_whitespace("hello\n\nworld"), "hello world");
        assert_eq!(normalize_whitespace("  a  b  c  "), "a b c");
        assert_eq!(
            normalize_whitespace("line\tone\rline\ntwo"),
            "line one line two"
        );
        assert_eq!(normalize_whitespace(""), "");
    }

    #[test]
    fn chat_db_path_under_home() {
        // Smoke check: path goes under HOME and points at Library/Messages/chat.db.
        let p = chat_db_path();
        let s = p.to_string_lossy();
        assert!(s.ends_with("Library/Messages/chat.db"), "got: {s}");
    }
}
