# Cellar v1 — User & Developer Docs

This directory holds the user-facing and developer-facing documentation
for the Cellar v1 daemon. The strategic plan lives in
`/Users/dimitriospagkratis/.claude/plans/cellar-app-v1.md`.

## Start here

- [Getting Started](getting-started.md) — install, run, verify, add
  your first rule, install as a LaunchAgent.
- [Rule Cookbook](rule-cookbook.md) — patterns + 5 ready-to-load
  example rules.
- [CLI Reference](cli-reference.md) — every `cellar` subcommand.
- [Architecture](architecture.md) — developer tour of the daemon's
  internals.

## Example rules

The [`example-rules/`](example-rules/) directory ships 5 hand-crafted
rule JSONs you can load directly:

| File | What |
|---|---|
| `big-file-deletion.json` | Watcher: any file ≥1 GiB deleted. |
| `documents-file-deletion.json` | Watcher: any deletion under ~/Documents. |
| `agent-fs-copy-guard.json` | Guard: agent moves files outside ~/Workspace. |
| `process-allowlist.json` | Watcher: unknown process started (via watchlist). |
| `blocked-domains-guard.json` | Guard: agent navigates to blocked domain. |

Load any of them with:

```sh
cellar rules add cellar-app-docs/example-rules/big-file-deletion.json
```
