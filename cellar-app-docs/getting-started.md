# Cellar — Getting Started

> Cellar is the trust and execution layer of and for AI on your device — a
> macOS background daemon that watches ambient events (processes, files,
> app/window focus), intercepts every AI agent action that flows through
> the `cel_act` gateway, and runs your rules against both.

This guide takes a fresh machine to a running daemon with a single
hand-authored rule that fires on real desktop activity.

## Install (developer build)

```sh
git clone <repo>
cd cellar
cargo build --release -p cel-cortex-daemon -p cellar-cli
```

The `release` binaries land in `target/release/`:
- `target/release/cel-cortex-daemon` — the daemon
- `target/release/cellar` — the CLI

Copy them somewhere on `PATH` if you want short names:

```sh
sudo cp target/release/cel-cortex-daemon /usr/local/bin/
sudo cp target/release/cellar             /usr/local/bin/
```

## Run the daemon

The daemon defaults to:

- Socket:   `~/.cellar/daemon.sock`
- Rules DB: `~/.cellar/rules.sqlite`
- Memory:   `~/.cellar/memory.sqlite`

Override with the `CELLAR_DAEMON_SOCK` / `CELLAR_RULES_DB` /
`CELLAR_MEMORY_DB` env vars (use `:memory:` for memory to keep the
agent's chat history ephemeral).

```sh
cel-cortex-daemon
```

You should see a startup log similar to:

```
INFO cel-cortex-daemon starting
INFO opening rules store path=/Users/you/.cellar/rules.sqlite
INFO memory subsystem wired
INFO rules store wired (sqlite, file-backed, hot-reload via Arc clones) rules=0
INFO gateway subsystem wired
INFO matcher consumer task spawned cooldown=true webhooks=false fires=true
INFO process poller spawned
INFO signals poller spawned
INFO fsevents adapter spawned
INFO ipc server listening (mode 0600, owner only) path=/Users/you/.cellar/daemon.sock
INFO daemon ready — waiting for SIGINT (Ctrl-C) to stop
```

## Health-check the daemon

In another terminal:

```sh
cellar doctor
```

```
[doctor] socket path: /Users/you/.cellar/daemon.sock
  ✓ socket file exists
  ✓ daemon.status responds (uptime 12s)
  ✓ 9 capabilities advertised
  i rules.compile not wired (no LLM provider configured)
  i agent.message not wired (no LLM provider configured)
[doctor] all checks passed
```

The "rules.compile not wired" and "agent.message not wired" notes are
expected on a vanilla install — both need an LLM provider configured.
See [LLM Configuration](#llm-configuration) below.

## Add a rule

The example rules under `cellar-app-docs/example-rules/` give you
hand-crafted JSON you can pipe straight into the daemon:

```sh
cellar rules add cellar-app-docs/example-rules/big-file-deletion.json
# → added rule: big_file_deletion

cellar rules list
#          big_file_deletion         "watcher"  Big file deletion (>1GB)
```

The rule is now live — the matcher consumer task sees it on the next
event without a daemon restart (see `cellar-rules-store/tests/hot_reload.rs`).

## See it fire

The example rule above fires on any `file_deleted` event with
`size_bytes >= 1 GiB`. Trigger one:

```sh
dd if=/dev/zero of=/tmp/big_test.bin bs=1m count=1024  # ~1 GiB file
rm /tmp/big_test.bin
```

Then:

```sh
cellar activity fires
# 2026-05-24T18:42:11Z       big_file_deletion  on file_deleted
```

The fire is also visible to the matcher's memory audit trail via
`memory.recent` (when wired) and any open `fires.subscribe` stream.

## Add a rule that watches the browser

For a rule that pairs a watchlist with browser navigation, see
[`url-change-guard.json`](example-rules/url-change-guard.json) — it
matches `url_changed` events (forwarded from the Tauri Cortex via
`events.publish`) against the `blocked_domains` watchlist and asks
for confirmation before letting the agent stay on a flagged page.
A seed watchlist lives at
[`example-watchlists/blocked-domains.json`](example-watchlists/blocked-domains.json);
the cookbook covers the `www.`-prefix gotcha in recipe #6.

```sh
cellar rules add cellar-app-docs/example-rules/url-change-guard.json
cellar watchlists set blocked_domains \
  facebook.com www.facebook.com \
  instagram.com www.instagram.com
```

## LLM configuration

`rules.compile` (natural-language → typed `Rule`) and `agent.message`
(the embedded chat agent) both need an LLM provider. Set the env vars
before starting the daemon:

```sh
# Anthropic (default — recommended for v1)
export CELLAR_DEFAULT_PROVIDER=anthropic
export CELLAR_DEFAULT_MODEL=claude-opus-4-7
export ANTHROPIC_API_KEY=sk-ant-...

# Or OpenAI-compatible (OpenRouter, LiteLLM, vLLM, LM Studio, etc.)
export CELLAR_DEFAULT_PROVIDER=openai
export CELLAR_DEFAULT_MODEL=gpt-4o-mini
export CELLAR_DEFAULT_BASE_URL=https://api.openai.com/v1
export OPENAI_API_KEY=sk-...

# Or Ollama (local)
export CELLAR_DEFAULT_PROVIDER=ollama
export CELLAR_DEFAULT_MODEL=llama3.2:3b
export CELLAR_DEFAULT_BASE_URL=http://localhost:11434
```

Per-subsystem overrides also work:
`CELLAR_NL_COMPILER_PROVIDER` / `CELLAR_NL_COMPILER_MODEL` for the rule
compiler, `CELLAR_AGENT_PROVIDER` / `CELLAR_AGENT_MODEL` for the agent.

Restart the daemon. `cellar doctor` should now show `rules.compile` and
`agent.message` as wired.

## Compile a natural-language rule

```sh
cellar rules compile "alert me when any app outside my approved list launches"
```

The CLI shows the human-readable summary, any warnings, and the draft
JSON. Pipe the draft back into `rules add` if you want to save it.

## Install as a LaunchAgent (autostart)

The `cel-cortex-daemon/launchagent/com.cellar.daemon.plist` template
makes the daemon start at login. Customize the paths and install:

```sh
cp cel-cortex-daemon/launchagent/com.cellar.daemon.plist \
   ~/Library/LaunchAgents/com.cellar.daemon.plist
sed -i '' "s|__DAEMON_PATH__|$(which cel-cortex-daemon)|" \
   ~/Library/LaunchAgents/com.cellar.daemon.plist
sed -i '' "s|__LOG_DIR__|$HOME/.cellar/logs|" \
   ~/Library/LaunchAgents/com.cellar.daemon.plist
mkdir -p ~/.cellar/logs
launchctl load -w ~/Library/LaunchAgents/com.cellar.daemon.plist
```

Uninstall:

```sh
launchctl unload -w ~/Library/LaunchAgents/com.cellar.daemon.plist
rm ~/Library/LaunchAgents/com.cellar.daemon.plist
```

## Where to go next

- [Rule cookbook](rule-cookbook.md) — patterns for common scenarios.
- [CLI reference](cli-reference.md) — every `cellar` subcommand.
- [Architecture](architecture.md) — how the daemon fits together.
- [IPC protocol](../../.claude/plans/cellar-ipc-protocol.md) — the wire
  contract every client (Tauri, CLI, MCP) speaks.
