# Quickstart: CEL with any agent

Get CEL running locally in under 5 minutes, then point your preferred agent at it. CEL is agent-agnostic — the install steps are identical regardless of which planner you use; only Step 4 (configuration) branches by agent.

Already set up and just need the per-agent config? Jump to [Step 4 — pick your agent](#step-4-pick-your-agent).

## Prerequisites

| Requirement | Version | Check |
|-------------|---------|-------|
| macOS | 13+ (Ventura or later) | `sw_vers` |
| Node.js | 20+ | `node --version` |
| pnpm | 9+ | `pnpm --version` |
| Rust | 1.75+ | `rustc --version` |

## Step 1: Build

```bash
cd cellar

# Install dependencies
pnpm install

# Build the Rust native module (CEL core + napi bindings)
cargo build --release -p cel-napi

# Copy the native binary to the agent package
cp target/release/libcel_napi.dylib cel/cel-napi/cel-napi.darwin-arm64.node

# Code-sign the native binary (required on macOS)
codesign -fs - cel/cel-napi/cel-napi.darwin-arm64.node

# Build TypeScript packages (agent + MCP server)
pnpm -r build
```

## Step 2: Grant Accessibility Permissions

CEL reads the screen via macOS Accessibility API. The host process needs permission:

1. Open **System Settings > Privacy & Security > Accessibility**
2. Add your terminal app (Terminal.app, iTerm2, Warp, etc.)
3. If using Claude Code in VS Code/Cursor, add that app too

> Without Accessibility permissions, `cel_see` returns empty context.

## Step 3: Configure an LLM provider

The fastest path — run the interactive setup, which writes `~/.cellar/config.toml`:

```bash
cellar init
```

Options offered:
- **Gemini / Anthropic / OpenAI** — paste an API key (or reuse one already in your env).
- **Ollama + Gemma 4 E4B (~4GB)** — runs locally, no cloud key needed. Requires Ollama installed (`brew install ollama && brew services start ollama`); `cellar init` can auto-pull the model.

If you'd rather configure by env var (for MCP `.mcp.json` embedding), skip `init` and continue to the Claude Code config below.

## Step 4: Pick your agent

CEL exposes the same tool surface (`cel_see`, `cel_act`, `cel_perceive`, `cel_think`) to every agent. Pick the branch that matches how you want to drive it. Full per-agent cookbooks live under [`agents/`](./agents/).

**Hello world used in each branch:** "Open Calculator, type 2+2, read the result." Three actions, no network, works with any of the five paths below.

> **Which LLM?** If the agent framework brings its own LLM (Claude Code, Cursor, Codex, LangGraph with BYOM), CEL does not need one — you can skip `CEL_LLM_*` env vars entirely. You only need them when you use CEL's built-in `cel_think run_goal` fallback. See [`bring-your-own-llm.md`](./bring-your-own-llm.md).

### 4a: Claude Code (MCP)

Create or edit `.mcp.json` in your project root (or `~/.mcp.json` for global):

```json
{
  "mcpServers": {
    "cellar": {
      "command": "node",
      "args": ["/absolute/path/to/cellar/mcp-server/dist/index.js"]
    }
  }
}
```

Restart Claude Code. The `cel_see` / `cel_act` / `cel_perceive` / `cel_think` tools appear in the tool list.

Hello world: ask Claude Code "Open Calculator, type 2+2 with cel_act, then read the result with cel_see." Full cookbook: [`agents/claude-code.md`](./agents/claude-code.md).

### 4b: Cursor (MCP)

Cursor reads MCP servers from `.cursor/mcp.json` (project) or `~/.cursor/mcp.json` (global). Same schema as Claude Code:

```json
{
  "mcpServers": {
    "cellar": {
      "command": "node",
      "args": ["/absolute/path/to/cellar/mcp-server/dist/index.js"]
    }
  }
}
```

Restart Cursor, enable the server in Settings → MCP. Hello world identical to Claude Code. Full cookbook: [`agents/cursor.md`](./agents/cursor.md).

### 4c: LangGraph (SDK / MCP)

Two paths:

- **MCP** — run CEL's MCP server and connect with `langchain-mcp-adapters`. Good for graphs that already speak MCP.
- **SDK / N-API** — embed CEL in-process by importing from `cel/cel-napi` (Node) or the Rust crates (for a native LangGraph sidecar). Lower latency.

Hello world (MCP path): the graph's tool node calls `cel_act ax_action` to open Calculator, then `cel_act type`, then `cel_see context` to read the display. Full cookbook and sidecar reference: [`agents/README.md`](./agents/README.md) (LangGraph section) plus [`langgraph-rust-sidecar.md`](./langgraph-rust-sidecar.md).

### 4d: Raw MCP client

Any MCP-speaking client (Python `mcp` library, custom Node client, n8n MCP node, etc.):

```bash
node /absolute/path/to/cellar/mcp-server/dist/index.js
```

The server speaks stdio JSON-RPC with the standard MCP handshake. Tools surface the same `cel_see` / `cel_act` / `cel_perceive` / `cel_think` contract documented in [`mcp-server.md`](./mcp-server.md).

Hello world: issue `cel_act { mode: "ax_action", action: "open_app", app: "Calculator" }`, then `cel_act { mode: "type", text: "2+2" }`, then `cel_see { mode: "context" }` and read the result field. Full cookbook: [`agents/README.md`](./agents/README.md) (raw MCP section) and [`api-reference.md`](./api-reference.md).

### 4e: Built-in `cel_think run_goal` fallback

If you want CEL to plan and execute on its own (no external agent), use the built-in planner. This is the path from the previous version of this doc — preserved so users following the old quickstart are not stranded.

Configure `.mcp.json` with LLM env vars so `cel_think` can call out:

```json
{
  "mcpServers": {
    "cellar": {
      "command": "node",
      "args": ["/absolute/path/to/cellar/mcp-server/dist/index.js"],
      "env": {
        "CEL_LLM_PROVIDER": "gemini",
        "CEL_LLM_API_KEY": "your-gemini-api-key",
        "CEL_LLM_MODEL": "gemini-2.0-flash"
      }
    }
  }
}
```

Or skip MCP entirely and run from the CLI:

```bash
cellar run-goal "Open Calculator, type 2+2, and read the result"
```

**Optional: separate planner model for higher-quality delegated runs:**

```json
"env": {
  "CEL_LLM_PROVIDER": "gemini",
  "CEL_LLM_API_KEY": "your-gemini-api-key",
  "CEL_LLM_MODEL": "gemini-2.5-flash",
  "CEL_LLM_PLANNER_PROVIDER": "gemini",
  "CEL_LLM_PLANNER_API_KEY": "your-gemini-api-key",
  "CEL_LLM_PLANNER_MODEL": "gemini-2.5-flash"
}
```

The built-in planner is a **reference client**, not the definition of CEL — it is fine to use, but any of the branches above is equally first-class. See [`mcp-server.md`](./mcp-server.md) for the full `cel_think` reference.

## Step 5: Verify

Whichever branch you picked, the verification is the same: confirm `cel_see` returns structured context.

**From Claude Code / Cursor / any MCP-speaking agent:**

```
Take a screenshot of my screen using cel_see
```

or

```
What apps are open on my screen?
```

**From the CLI:**

```bash
cellar context            # human-readable
cellar context --json     # raw structured output
```

The agent (or CLI) should return a list of `ContextElement`s with labels, roles, bounds, and confidence scores.

## Step 6: Chrome CDP (Optional but Recommended)

CDP (Chrome DevTools Protocol) gives CEL deep browser access — page content, DOM interaction, cookie banner dismissal, iframe access. Without it, CEL can still click/type in browsers, but can't read page content or interact with invisible elements.

**Option A: Launch CEL's dedicated browser**

```bash
cellar browser ensure
```

**Option B: Launch Chromium manually on CEL's port**

```bash
/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
  --remote-debugging-port=9333 \
  --remote-allow-origins=* \
  --user-data-dir="$HOME/.cellar/cdp-profiles/google-chrome"
```

CEL prefers its dedicated browser instance on port `9333`, so it does not have to guess between multiple Chrome sessions.

**Verify CDP:**

```
Check if CDP is available using cel_see cdp_status
```

## Claude Code skills (optional convenience layer)

Claude-Code-specific only. If you're using a different agent, skip this section — your agent's own tool-invocation surface already covers the same ground.

If you've installed the Claude Code skills, slash commands wrap the raw MCP tools:

### `/cellar` — Claude as Orchestrator

Claude observes, plans, acts, and verifies. Most powerful mode.

```
/cellar Open Finder and navigate to the Downloads folder
```

```
/cellar Fill out the contact form on the current webpage with my name and email
```

```
/cellar Find the price of AAPL stock in the Yahoo Finance tab
```

### `/cellar-auto` — Fire-and-Forget

Delegates to CEL's built-in planner/runtime. Useful as a convenience path, but it is only one client of the CEL platform.

```
/cellar-auto Open TextEdit and type "Hello World"
```

```
/cellar-auto Navigate to google.com and search for "weather today"
```

## What's Running

When the MCP server starts, three things happen automatically:

| Component | What it does | Always on? |
|-----------|-------------|------------|
| **CEL native** | Accessibility tree, screen capture, input injection | Yes |
| **Cortex** | Background perception — keeps a mental model of the screen warm | Yes |
| **CDP bridge** | Chrome DevTools Protocol for deep browser access | Only if Chrome is running with CDP |

The Cortex means `cel_see` `context` and `cel_perceive` `read` are instant — the screen model is already built before you ask for it.

## Tools Overview

| Tool | Purpose | Common modes/actions |
|------|---------|---------------------|
| **cel_see** | Read screen | `context`, `screenshot`, `cdp_page`, `wait_for_idle` |
| **cel_act** | Interact | `ax_action`, `set_value`, `click`, `type`, `cdp_eval`, `write_cells`, `read_cells` |
| **cel_think** | Optional built-in planning / memory | `run_goal`, `plan`, `store_knowledge`, `search_knowledge` |
| **cel_perceive** | Continuous awareness | `start`, `read`, `feed`, `checkpoint`, `stop` |

See [mcp-server.md](mcp-server.md) for the full tool reference with all modes, parameters, and examples.

## Troubleshooting

| Issue | Fix |
|-------|-----|
| `cel_see` returns empty context | Grant Accessibility permissions (Step 2) |
| `CEL native module not available` | Rebuild: `cargo build --release -p cel-napi` + copy + codesign |
| CDP not connecting | Run `cellar browser ensure`, then confirm with `cellar browser status` |
| `cel_act` clicks wrong position | Use `ax_action` or `set_value` instead of coordinate clicks |
| MCP server not showing in Claude Code | Check `.mcp.json` path is absolute, restart Claude Code |
| Cortex not booting | Check stderr output: `timeout 3 node cellar/mcp-server/dist/index.js` |

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `CEL_LLM_PROVIDER` | For cel_think | `gemini`, `anthropic`, `openai`, `ollama`, `compatible` |
| `CEL_LLM_API_KEY` | For cel_think | API key for the provider |
| `CEL_LLM_MODEL` | For cel_think | Model name (e.g., `gemini-2.0-flash`) |
| `CEL_LLM_ENDPOINT` | Optional | Custom endpoint URL |
| `CEL_LLM_PLANNER_PROVIDER` | Optional | Separate provider for run_goal planner |
| `CEL_LLM_PLANNER_API_KEY` | Optional | API key for planner provider |
| `CEL_LLM_PLANNER_MODEL` | Optional | Model for planner (default: `gemini-2.5-flash`, escalation: `claude-sonnet-4-20250514`) |
