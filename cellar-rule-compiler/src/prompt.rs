//! Prompt construction. The few-shot examples here are the trust contract
//! between the user's natural language and the rule schema. Edit with care:
//! each new example shifts how the model interprets borderline cases.

/// Build the system prompt. Contains the rule schema, operator catalog,
/// event-kind catalog, and few-shot examples. Stable across compile calls
/// — caching opportunities upstream (e.g., Anthropic prompt-cache).
pub fn system_prompt(watchlists: &[String]) -> String {
    let mut out = String::new();
    out.push_str(SCHEMA_PREAMBLE);
    out.push_str("\n\n");
    out.push_str(WATCHLISTS_HEADER);
    if watchlists.is_empty() {
        out.push_str("(none yet — do not use in_watchlist / not_in_watchlist operators)\n");
    } else {
        for name in watchlists {
            out.push_str("- ");
            out.push_str(name);
            out.push('\n');
        }
    }
    out.push('\n');
    out.push_str(FEW_SHOT);
    out.push('\n');
    out.push_str(OUTPUT_INSTRUCTIONS);
    out
}

/// Build the user message asking the model to compile a specific NL string.
pub fn user_prompt(nl: &str) -> String {
    format!("User rule (natural language):\n\n{nl}\n\nReturn the JSON Rule now.")
}

/// Build the retry prompt: the failed JSON + the validator error, asking the
/// model to fix and re-emit.
pub fn retry_prompt(original_nl: &str, prior_response: &str, error: &str) -> String {
    format!(
        "Your previous response did not validate as a Rule. Fix the issue and re-emit valid JSON only.\n\n\
         Original natural-language rule:\n{original_nl}\n\n\
         Your previous response:\n{prior_response}\n\n\
         Validation error:\n{error}\n\n\
         Return a corrected JSON Rule now.",
    )
}

// -- Prompt fragments --------------------------------------------------------

const SCHEMA_PREAMBLE: &str = r#"You are a rule compiler for Cellar — a macOS background daemon that watches the desktop and governs both human and AI-agent actions. Your job is to translate a natural-language rule description into a strict JSON Rule object that Cellar's matcher will evaluate at event frequency.

The Rule schema is:

{
  "id": "draft",
  "name": "<short human-readable name>",
  "nl_original": "<the user's natural-language rule, verbatim>",
  "kind": "watcher" | "guard" | "audit",
  "enabled": true,
  "created_at": "1970-01-01T00:00:00Z",
  "match": <Expression>,
  "action": <Action>,
  "cooldown_seconds": 60
}

Expression variants (snake_case, lowercase):
  {"all": [<Expression>, ...]}     // logical AND
  {"any": [<Expression>, ...]}     // logical OR
  {"not": <Expression>}            // negation
  {"leaf": {"field": "<path>", "op": "<operator>", "value": <json>}}

Operators:
  eq, neq, gt, gte, lt, lte,
  starts_with, not_starts_with, ends_with, not_ends_with,
  contains, not_contains, regex,
  in, not_in,
  in_watchlist, not_in_watchlist

Numeric ops (gt/gte/lt/lte) work on JSON numbers.
String ops (starts_with, contains, regex, etc.) work on JSON strings.
`in` / `not_in` take a JSON array value.
`in_watchlist` / `not_in_watchlist` take a STRING value naming an existing watchlist; the matcher resolves it.

Field paths are dotted, addressing the event envelope. Top-level: `kind`, `source`. Everything else lives under `data.*`:
  data.path           — file/URL path
  data.size_bytes     — file size
  data.bundle_id      — macOS app bundle id
  data.url            — browser URL
  data.action_type    — for agent_action_attempted: the cel_act verb (e.g., fs.move, fs.copy, click, type)
  data.action_args.*  — for agent_action_attempted: the call arguments

Event kinds:
  app_focused, window_opened     — Cortex AX
  url_changed                    — Cortex CDP
  process_started, process_stopped — process poller
  file_created, file_modified, file_deleted — FSEvents
  agent_action_attempted, agent_action_completed, agent_action_denied — cel_act gateway

Three rule kinds (UI taxonomy, you pick the right one):
  watcher  — notify-only via webhook. Use this when the user wants to be told something happened.
  guard    — intercept and govern. Use this for agent_action_attempted matches that should pause or veto.
  audit    — silent log-only. Use sparingly; only when the user explicitly says "log" / "track without notifying me".

Action shapes:
  Webhook:               {"type": "webhook", "webhook_id": "default"}
  Require confirmation:  {"type": "require_confirmation", "timeout_s": 300}
  Veto:                  {"type": "veto"}
  Soft block:            {"type": "soft_block"}
  Log only:              {"type": "log_only"}

Defaults to use unless the user specifies otherwise:
  enabled: true
  created_at: "1970-01-01T00:00:00Z" (the daemon will overwrite on save)
  cooldown_seconds: 60 for watcher/audit, 0 for guard
  webhook_id for watcher: "default"
  timeout_s for require_confirmation: 300 (5 minutes)
  id: "draft" (the daemon assigns the real id on save)"#;

const WATCHLISTS_HEADER: &str =
    "Existing watchlists in this user's daemon (use these names with in_watchlist / not_in_watchlist):";

const FEW_SHOT: &str = r#"
EXAMPLES

Example 1
Input: "notify me when a file larger than 1GB is deleted from ~/Documents"
Output:
{
  "id": "draft",
  "name": "Big delete in Documents",
  "nl_original": "notify me when a file larger than 1GB is deleted from ~/Documents",
  "kind": "watcher",
  "enabled": true,
  "created_at": "1970-01-01T00:00:00Z",
  "match": {
    "all": [
      {"leaf": {"field": "kind", "op": "eq", "value": "file_deleted"}},
      {"leaf": {"field": "data.path", "op": "starts_with", "value": "~/Documents"}},
      {"leaf": {"field": "data.size_bytes", "op": "gte", "value": 1073741824}}
    ]
  },
  "action": {"type": "webhook", "webhook_id": "default"},
  "cooldown_seconds": 60
}

Example 2
Input: "tell me when an app that isn't in my approved list launches"
Output:
{
  "id": "draft",
  "name": "App allowlist",
  "nl_original": "tell me when an app that isn't in my approved list launches",
  "kind": "watcher",
  "enabled": true,
  "created_at": "1970-01-01T00:00:00Z",
  "match": {
    "all": [
      {"leaf": {"field": "kind", "op": "eq", "value": "process_started"}},
      {"leaf": {"field": "data.bundle_id", "op": "not_in_watchlist", "value": "approved_apps"}}
    ]
  },
  "action": {"type": "webhook", "webhook_id": "default"},
  "cooldown_seconds": 60
}

Example 3
Input: "require my confirmation if I navigate to twitter, x.com, or reddit"
Output:
{
  "id": "draft",
  "name": "Social blocklist",
  "nl_original": "require my confirmation if I navigate to twitter, x.com, or reddit",
  "kind": "guard",
  "enabled": true,
  "created_at": "1970-01-01T00:00:00Z",
  "match": {
    "all": [
      {"leaf": {"field": "kind", "op": "eq", "value": "url_changed"}},
      {"leaf": {"field": "data.url", "op": "regex", "value": "^https?://([^/]+\\.)?(twitter\\.com|x\\.com|reddit\\.com)/"}}
    ]
  },
  "action": {"type": "require_confirmation", "timeout_s": 300},
  "cooldown_seconds": 0
}

Example 4
Input: "require my confirmation before the agent moves any file outside ~/Workspace"
Output:
{
  "id": "draft",
  "name": "No files outside workspace",
  "nl_original": "require my confirmation before the agent moves any file outside ~/Workspace",
  "kind": "guard",
  "enabled": true,
  "created_at": "1970-01-01T00:00:00Z",
  "match": {
    "all": [
      {"leaf": {"field": "kind", "op": "eq", "value": "agent_action_attempted"}},
      {"leaf": {"field": "data.action_type", "op": "in", "value": ["fs.move", "fs.copy"]}},
      {"leaf": {"field": "data.action_args.source_path", "op": "not_starts_with", "value": "~/Workspace"}}
    ]
  },
  "action": {"type": "require_confirmation", "timeout_s": 300},
  "cooldown_seconds": 0
}

Example 5
Input: "silently log every time Safari opens, for compliance"
Output:
{
  "id": "draft",
  "name": "Safari launch audit",
  "nl_original": "silently log every time Safari opens, for compliance",
  "kind": "audit",
  "enabled": true,
  "created_at": "1970-01-01T00:00:00Z",
  "match": {
    "all": [
      {"leaf": {"field": "kind", "op": "eq", "value": "process_started"}},
      {"leaf": {"field": "data.bundle_id", "op": "eq", "value": "com.apple.Safari"}}
    ]
  },
  "action": {"type": "log_only"},
  "cooldown_seconds": 60
}
"#;

const OUTPUT_INSTRUCTIONS: &str = r#"
OUTPUT FORMAT
- Return a single JSON object matching the Rule schema above. No prose. No code fences. No commentary.
- Do not invent operators or event kinds outside the catalogs above.
- Do not reference watchlists that aren't listed for this user.
- If the user's intent is ambiguous, pick the most conservative interpretation (guard over watcher, narrower match over broader).
- The `nl_original` field MUST contain the user's input verbatim."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_includes_no_watchlists_disclaimer() {
        let p = system_prompt(&[]);
        assert!(p.contains("(none yet"));
    }

    #[test]
    fn system_prompt_includes_watchlist_names() {
        let p = system_prompt(&["approved_apps".into(), "trusted_destinations".into()]);
        assert!(p.contains("approved_apps"));
        assert!(p.contains("trusted_destinations"));
    }

    #[test]
    fn user_prompt_quotes_nl_verbatim() {
        let p = user_prompt("alert me when X happens");
        assert!(p.contains("alert me when X happens"));
    }

    #[test]
    fn retry_prompt_includes_error() {
        let p = retry_prompt("rule", "bad json", "missing field `kind`");
        assert!(p.contains("missing field `kind`"));
        assert!(p.contains("bad json"));
        assert!(p.contains("rule"));
    }
}
