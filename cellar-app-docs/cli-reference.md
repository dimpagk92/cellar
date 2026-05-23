# `cellar` CLI Reference

The `cellar` binary is a shell-friendly wrapper around the daemon's
JSON-RPC IPC surface. Every subcommand corresponds to one or two IPC
methods; the contract is identical to what the Tauri app talks.

## Global flags

| Flag | Env var | Default | Notes |
|---|---|---|---|
| `--socket <PATH>` | `CELLAR_DAEMON_SOCK` | `~/.cellar/daemon.sock` | Override the daemon socket path. |
| `--json` | — | off | Pretty-print JSON instead of human-readable tables. |

## Health and inspection

| Command | What |
|---|---|
| `cellar status` | Human-readable `daemon.status` summary (uptime, counts, version). |
| `cellar doctor` | Socket + daemon.status + capabilities checks. Non-zero exit on failure. |
| `cellar capabilities` | Full `system.hello` capability dump. |

## Rules

| Command | What |
|---|---|
| `cellar rules list` | All rules; `[paused]` marker on disabled. |
| `cellar rules get <id>` | One rule as JSON. |
| `cellar rules add <file>` | Add a rule from JSON (`-` for stdin). |
| `cellar rules compile "<nl>"` | Natural-language → typed Rule (preview only). |
| `cellar rules pause <id>` | Disable without deleting. |
| `cellar rules resume <id>` | Re-enable. |
| `cellar rules remove <id>` | Delete by id. |
| `cellar rules test <id> [--since RFC3339]` | Replay the events ring through one rule. |

## Watchlists

| Command | What |
|---|---|
| `cellar watchlists list` | All watchlists with their items. |
| `cellar watchlists get <name>` | One watchlist as JSON. |
| `cellar watchlists set <name> <items...>` | Replace items atomically; creates if absent. |
| `cellar watchlists add-item <name> <item>` | Add a single item. |
| `cellar watchlists remove-item <name> <item>` | Remove a single item. |
| `cellar watchlists remove <name>` | Delete an entire watchlist. |

## Webhooks

| Command | What |
|---|---|
| `cellar webhooks list` | All webhook configs (id + URL). |
| `cellar webhooks add <file>` | Add a webhook from JSON. |
| `cellar webhooks test <id>` | Send a synthetic POST; report status + elapsed. |
| `cellar webhooks remove <id>` | Delete by id. |

Note: `webhooks add` records the webhook in the SQLite store, but the
running `WebhookService` is built from a startup snapshot. New webhooks
added via the CLI need a daemon restart to start delivering. Hot-reload
is a Phase 2.x follow-up.

## Activity inspection

| Command | What |
|---|---|
| `cellar activity events [--kind X] [--limit N]` | Recent events from the ring. |
| `cellar activity fires [--rule X] [--limit N]` | Recent fires from the ring. |

The ring is in-memory and bounded (default 1024 entries each). Daemon
restart clears it.

## Confirmation flow

| Command | What |
|---|---|
| `cellar confirmation list` | Pending confirmations (rule + expires_at). |
| `cellar confirmation resolve <id> allow|deny|always-allow` | Send a decision over IPC. |

Use this when the Tauri modal isn't running (e.g., automated tests) or
when scripting confirmation responses.

## Examples

### Tail recent fires for one rule

```sh
cellar activity fires --rule big_file_deletion --limit 100
```

### Add a rule from clipboard

```sh
pbpaste | cellar rules add -
```

### Backup all rules + watchlists

```sh
cellar rules list --json > rules.bak.json
cellar watchlists list --json > watchlists.bak.json
```

### Health check in a deploy script

```sh
cellar doctor || { echo "daemon unhealthy"; exit 1; }
```

### Resolve every pending confirmation as Allow (don't do this casually)

```sh
cellar confirmation list --json \
  | jq -r '.pending[].id' \
  | while read id; do
      cellar confirmation resolve "$id" allow
    done
```
