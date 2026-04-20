# Troubleshooting

## Claude Code doesn't see the `cellar` MCP server

1. Check the config path in `.mcp.json` is **absolute**, not relative:
   ```json
   "args": ["/Users/you/code/cellar/mcp-server/dist/index.js"]
   ```
2. Verify the file exists:
   ```bash
   ls /path/to/cellar/mcp-server/dist/index.js
   ```
   If not, rebuild: `pnpm -r build`
3. Check the logs — Claude Code shows MCP errors in the output panel. Look for "Failed to load MCP server" or a Node error.
4. Run the server directly to check it starts:
   ```bash
   node /path/to/cellar/mcp-server/dist/index.js
   ```
   Should print startup logs and hang waiting for stdio input. Kill with Ctrl+C.

## `cel_see` returns "permission denied" or empty results

macOS Accessibility permission isn't granted for the process running the MCP server.

1. Open **System Settings → Privacy & Security → Accessibility**
2. Find the app that launches the MCP server (usually Claude Code, Cursor, or your terminal)
3. Toggle it on
4. **Restart the app** — permission changes don't take effect in already-running processes

Verify with:
```bash
cellar setup
```
This prints a permission audit for AX, screen recording, and CDP.

## `cel-napi.node` fails to load

The compiled native binary is missing or wrong architecture for your Mac.

```bash
# Check what's there
ls -la cel/cel-napi/*.node

# Rebuild for your arch (Apple Silicon)
cargo build --release -p cel-napi
cp target/release/libcel_napi.dylib cel/cel-napi/cel-napi.darwin-arm64.node
codesign -fs - cel/cel-napi/cel-napi.darwin-arm64.node
```

For Intel Macs, the artifact is `cel-napi.darwin-x64.node`.

## Chrome CDP not connecting

Chrome needs to be launched with remote debugging enabled.

```bash
# Close all Chrome windows first
open -a "Google Chrome" --args --remote-debugging-port=9222
```

Verify:
```bash
curl http://127.0.0.1:9222/json/version
```

Should return JSON with the Chrome version. If not, check that Chrome fully quit before relaunch — macOS sometimes keeps background processes alive.

## "Failed to connect to Ollama"

Start the Ollama server and pre-pull the model:

```bash
brew services start ollama
ollama pull gemma3:4b
```

Verify:
```bash
curl http://127.0.0.1:11434/api/tags
```

Should list installed models including `gemma3:4b`.

If Ollama is running but Cellar still can't connect, check `~/.cellar/config.toml`:

```toml
[llm]
provider = "ollama"
model    = "gemma3:4b"
# api_base defaults to http://127.0.0.1:11434 — override here if non-default
```

## LLM API key errors

- **`LLM_MISSING_API_KEY`** — set `GEMINI_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` in your env, or run `cellar init`, or edit `~/.cellar/config.toml`.
- **`LLM_INVALID_API_KEY`** — key format looks wrong. Gemini keys start with `AIza`; OpenAI keys start with `sk-`; Anthropic keys start with `sk-ant-`.
- **`LLM_RATE_LIMITED`** — you hit the provider's rate limit. Wait, or switch to a higher tier.

## `cargo build` fails with linker errors on macOS

You likely need Xcode Command Line Tools:

```bash
xcode-select --install
```

If that's already installed, try:

```bash
sudo xcode-select --reset
cargo clean && cargo build --release -p cel-napi
```

## `pnpm install` hangs or fails

```bash
# Nuke node_modules and the lockfile, reinstall
rm -rf node_modules **/node_modules pnpm-lock.yaml
pnpm install
```

If it still fails, check your Node version:
```bash
node --version  # must be 20+
pnpm --version  # must be 9+
```

## Goal runner loops on a single step

Almost always one of:

1. **Element ambiguity** — multiple elements match the planner's target. Check logs for `selector matched N elements`. Refine the goal with more specific language ("the Save button in the toolbar" instead of "Save").

2. **Stale state** — page/app is loading, Cortex sees the old state. The freshness model *should* catch this; if it doesn't, report an issue.

3. **Permission gate** — OS dialog, auth prompt, or captcha blocking. Cortex detects these and should terminate the run; if it doesn't, the logs will show `escalation: terminal` near the top.

For persistent loops, inspect the mental-model feed:

```bash
cellar context --watch
```

## Performance is slower than expected

- **First call after startup** is slower — Cortex takes 2–3 seconds to boot. Subsequent calls are fast.
- **Vision steps** are 10–100x slower than structured steps. If most of your steps are going to vision, something is wrong with the a11y tree (wrong app focused? permission missing? native view instead of accessible widget?).
- **Screen capture** on Apple Silicon with multiple 4K displays can be slow. Reduce display count or use `cel_see mode: "context"` which doesn't capture pixels.

## "Where does Cellar store data?"

- **Config**: `~/.cellar/config.toml`
- **Local memory/store**: `~/.cellar/cel-store.db` (SQLite)
- **Goal runner history**: `~/.cellar/runs/`
- **Logs**: stderr, routed by the client. Claude Code puts them in the extension output panel.

Nothing is written outside `~/.cellar/` by default.

## Still stuck?

- Search [GitHub Issues](https://github.com/dimpagk92/cellar/issues) — likely someone hit the same thing
- Open a new issue with:
  - Your OS + version
  - Cellar git hash (`git rev-parse HEAD`)
  - Node / Rust / pnpm versions
  - The exact command you ran
  - Relevant logs (stderr or the MCP output panel)
- Or ask in [Discussions](https://github.com/dimpagk92/cellar/discussions) for anything that might be a question more than a bug
