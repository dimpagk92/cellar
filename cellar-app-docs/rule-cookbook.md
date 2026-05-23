# Cellar Rule Cookbook

Hand-authored rule recipes for the common patterns. Every example is
a JSON file in `cellar-app-docs/example-rules/` — drop them into the
daemon with `cellar rules add <file>`.

## Anatomy of a rule

Every rule is one JSON object that round-trips with `cellar_types::Rule`:

```json
{
  "id": "stable_unique_id",
  "name": "Human-readable display name",
  "nl_original": "what the user typed",
  "kind": "watcher | guard | audit",
  "enabled": true,
  "match": { /* boolean expression tree */ },
  "action": { "type": "...", /* type-specific args */ },
  "cooldown_seconds": 0,
  "created_at": "2026-05-22T00:00:00Z"
}
```

- `kind` — UI-level taxonomy.
  - `watcher`: notify-only. Pair with `action.type = webhook` or
    `log_only`.
  - `guard`: intervene on `cel_act` agent actions. Pair with
    `require_confirmation`, `veto`, or `soft_block`.
  - `audit`: silent. Pair with `log_only`.

- `match` — boolean tree of leaves and `all`/`any` combinators. Leaves
  pick a field (`kind`, `source`, `data.<dotted-path>`) and apply an
  operator. See the [operator reference](#operators) below.

- `action` — what happens on match. See [actions](#actions) below.

- `cooldown_seconds` — minimum seconds between consecutive fires of
  this rule. `0` disables.

## Operators

| Operator | Purpose | Example |
|---|---|---|
| `eq` / `not_eq` | Exact match | `{ "field": "kind", "op": "eq", "value": "file_deleted" }` |
| `gt` / `gte` / `lt` / `lte` | Numeric comparison | `{ "field": "data.size_bytes", "op": "gte", "value": 1073741824 }` |
| `contains` / `not_contains` | Substring | `{ "field": "data.path", "op": "contains", "value": "/Documents/" }` |
| `starts_with` / `not_starts_with` | Prefix | `{ "field": "data.path", "op": "starts_with", "value": "/Users/" }` |
| `regex_match` / `not_regex_match` | Regex (Rust regex syntax) | `{ "field": "data.url", "op": "regex_match", "value": "https?://facebook\\.com" }` |
| `in` / `not_in` | Membership in a literal array | `{ "field": "data.action_type", "op": "in", "value": ["fs.copy", "fs.move"] }` |
| `in_watchlist` / `not_in_watchlist` | Membership in a named watchlist | `{ "field": "data.bundle_id", "op": "in_watchlist", "value": "approved_apps" }` |

## Actions

| Type | Use with `kind` | Effect |
|---|---|---|
| `log_only` | `watcher` / `audit` | Write a Fire chunk to memory. Nothing else. |
| `webhook` | `watcher` | Enqueue a HTTP POST through the configured webhook (`webhook_id`). |
| `require_confirmation` | `guard` | Pause the action; surface a `confirmation_required` event over IPC; await user response (or `timeout_s` default 300s). |
| `veto` | `guard` | Reject the action outright. |
| `soft_block` | `guard` | Reject the action; daemon attempts a counter-measure (close window, navigate away). Best-effort. |

## Recipes

### 1. Notify on big file deletion

[`big-file-deletion.json`](example-rules/big-file-deletion.json) —
fires whenever a file ≥1 GiB is deleted, anywhere.

```sh
cellar rules add cellar-app-docs/example-rules/big-file-deletion.json
```

Caveat (v1): `data.size_bytes` is only present on `file_created` and
`file_modified` events — by the time the `file_deleted` event fires,
the file is gone and `stat()` fails. The rule above works for the
typical case (file modified just before deletion writes a recent
size into the matcher's view) but a recently-observed-sizes cache
is a known follow-up.

### 2. Watch a directory

[`documents-file-deletion.json`](example-rules/documents-file-deletion.json) —
fires on any deletion under `~/Documents`. The path filter uses
`starts_with` on the literal home, plus `contains` for the subfolder
so the same JSON works for any user (replace `/Users/` only if you've
moved your home).

### 3. Confirm before the agent touches important files

[`agent-fs-copy-guard.json`](example-rules/agent-fs-copy-guard.json) —
the canonical Scenario 4 rule. When the embedded agent (or any
`cel_act` caller) tries to `fs.copy` or `fs.move` a file outside
`~/Workspace/`, the gateway pauses the action and pushes a
`confirmation_required` event. Replace `/Users/you/Workspace/` with
your actual workspace path.

### 4. Allowlist with watchlist

[`process-allowlist.json`](example-rules/process-allowlist.json) — fires
when a process starts whose name isn't in the `approved_apps` watchlist.

You need to populate the watchlist first:

```sh
cellar watchlists set approved_apps Safari "Google Chrome" Slack zsh ssh
```

The rule then fires on every other `process_started` event. Cooldown
of 30s keeps it from spamming during a busy day.

### 5. Block the agent from distracting sites

[`blocked-domains-guard.json`](example-rules/blocked-domains-guard.json) —
require confirmation before the agent navigates to facebook/x/tiktok.
Pattern is `regex_match` so you can extend with more domains by
editing the JSON or composing per-domain rules.

## Authoring with the NL compiler

If you've configured an LLM provider, you can author rules in English:

```sh
cellar rules compile "tell me when any file larger than 1 GB gets deleted"
```

The CLI prints the human-readable summary, any warnings, and the draft
JSON. Pipe the draft into `rules add`:

```sh
cellar rules compile "alert me on big deletes" --json \
  | jq '.draft_rule' \
  | cellar rules add -
```

## Testing a rule against history

```sh
cellar rules test big_file_deletion --since 2026-05-22T00:00:00Z
```

Walks the daemon's recent-events ring buffer, applies the named rule's
match expression, and prints the events that would have fired it.
Useful for tuning thresholds before pushing the rule live.

## Authoring tips

- **Be specific.** Broad `match` expressions fire a lot. Pair with
  `cooldown_seconds` to keep noise down.
- **Use watchlists for changing sets.** A rule referencing
  `in_watchlist` reads the watchlist at match time, so adding /
  removing items takes effect immediately without rule edits.
- **Test before you ship.** `cellar rules test` is a dry run against
  the last ~hour of events; it's the cheapest way to find a typo.
- **Guard rules need explicit timeout_s.** The default
  (`default_confirmation_timeout_s` in the daemon, 300s) usually works,
  but security-critical guards (financial actions, deletes) want a
  shorter window so the action errors fast on user-away.
