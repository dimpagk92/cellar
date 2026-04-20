# Quickstart: CEL with Claude Code

Get CEL running as an MCP server in Claude Code in under 5 minutes.

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

## Step 4: Configure Claude Code

Create or edit `.mcp.json` in your project root (or `~/.mcp.json` for global):

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

**LLM env vars** are needed for `cel_think` planning and `run_goal` autonomous execution. If you only need `cel_see` + `cel_act` (with Claude as the planner), you can omit them.

**Optional: separate planner model** for higher-quality autonomous execution:

```json
{
  "mcpServers": {
    "cellar": {
      "command": "node",
      "args": ["/absolute/path/to/cellar/mcp-server/dist/index.js"],
      "env": {
        "CEL_LLM_PROVIDER": "gemini",
        "CEL_LLM_API_KEY": "your-gemini-api-key",
        "CEL_LLM_MODEL": "gemini-2.5-flash",
        "CEL_LLM_PLANNER_PROVIDER": "gemini",
        "CEL_LLM_PLANNER_API_KEY": "your-gemini-api-key",
        "CEL_LLM_PLANNER_MODEL": "gemini-2.5-flash"
      }
    }
  }
}
```

## Step 5: Restart Claude Code

After saving `.mcp.json`, restart Claude Code (or start a new conversation). The MCP server starts automatically.

**What happens on startup:**
1. CEL native module loads (Rust via napi-rs)
2. Cortex boots — always-on perception engine starts monitoring the screen
3. CDP auto-detect — if Chrome is running with remote debugging, CEL connects to it

You should see the `cel_see`, `cel_act`, `cel_think`, and `cel_perceive` tools available.

## Step 6: Verify

In Claude Code, ask:

```
Take a screenshot of my screen using cel_see
```

Or:

```
What apps are open on my screen?
```

Claude should call `cel_see` and describe what it sees.

## Step 7: Chrome CDP (Optional but Recommended)

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

## Using the Skills

If you've installed the Claude Code skills, you can use slash commands:

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

Delegates to CEL's internal planner. Faster and cheaper, but less capable.

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
| **cel_act** | Interact | `ax_action`, `set_value`, `click`, `type`, `cdp_eval` |
| **cel_think** | Reason | `run_goal`, `plan`, `store_knowledge`, `search_knowledge` |
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
