# Connect Cellar to Claude Code

Cellar ships as an MCP (Model Context Protocol) server. Any MCP-compatible client can use its four tools — `cel_see`, `cel_act`, `cel_think`, `cel_perceive` — to read your screen, take actions, and reason continuously about what's happening.

This guide walks through the fastest path: **Claude Code on macOS**. If you're on a different client, the config shape is the same; only the file location changes.

---

## Prerequisites

- **macOS 13+** with an admin account
- **Node.js 20+** and **pnpm 9+** — `brew install node pnpm`
- **Rust 1.75+** — `curl https://sh.rustup.rs -sSf | sh`
- **Claude Code** installed — https://claude.com/claude-code
- **An LLM provider** — one of:
  - Gemini / Anthropic / OpenAI API key (fastest — free tiers available)
  - Or [Ollama](https://ollama.com) with Gemma 4 E4B for fully-local runs

## 1. Build Cellar

```bash
git clone https://github.com/dimpagk92/cellar.git
cd cellar
pnpm install && pnpm -r build
cargo build --release -p cel-napi
cp target/release/libcel_napi.dylib cel/cel-napi/cel-napi.darwin-arm64.node
codesign -fs - cel/cel-napi/cel-napi.darwin-arm64.node
```

Verify the MCP server binary exists:

```bash
ls mcp-server/dist/index.js
```

## 2. Configure your LLM provider

The interactive setup is the fastest path:

```bash
cellar init
```

It'll walk you through picking a provider and writing `~/.cellar/config.toml`. Options:

- **Paste an API key** (Gemini, Anthropic, OpenAI) — structured perception still works offline; only the LLM call goes to your chosen provider.
- **Install Gemma 4 E4B locally via Ollama** — fully private, no network calls. Cellar will offer to install and configure Ollama for you.

Prefer to configure manually? Skip `cellar init` and edit `~/.cellar/config.toml`:

```toml
[llm]
provider = "gemini"           # openai | anthropic | gemini | ollama | compatible
api_key  = "your-key"
model    = "gemini-2.0-flash"
```

Environment variables override config file values. See [api-reference.md](api-reference.md#environment-variables) for the full list.

## 3. Grant macOS Accessibility permissions

Cellar reads the accessibility tree and injects input events. macOS requires explicit user consent.

1. Open **System Settings → Privacy & Security → Accessibility**
2. Click the **`+`** button
3. Add `/Applications/Claude Code.app` (or whichever client will launch the MCP server)
4. Toggle it on

On first run Cellar will prompt if permission is missing — you can also run `cellar setup` anytime to verify.

## 4. Add Cellar to Claude Code

Create or edit `.mcp.json` in your project root:

```json
{
  "mcpServers": {
    "cellar": {
      "command": "node",
      "args": ["/absolute/path/to/cellar/mcp-server/dist/index.js"],
      "env": {
        "CEL_LLM_PROVIDER": "gemini",
        "CEL_LLM_API_KEY": "your-key",
        "CEL_LLM_MODEL": "gemini-2.0-flash"
      }
    }
  }
}
```

**Replace the path** with your actual clone location. Env vars here override `~/.cellar/config.toml` for this project only.

Restart Claude Code. In a new session, type:

```
/mcp
```

You should see `cellar` in the list, with four tools available: `cel_see`, `cel_act`, `cel_think`, `cel_perceive`.

## 5. Try it

Open any macOS app (Finder works). In Claude Code, ask:

> Use cel_see to show me what's on screen right now, then summarize in 3 bullets what I could do next.

Claude should call `cel_see` with `mode: "context"`, return the accessibility tree with ~50–500 structured elements, and offer concrete next actions.

For autonomous multi-step execution:

> Use cel_think run_goal to create a new folder called "test-cellar" on my Desktop.

The goal runner will plan, execute, and verify — streaming mental-model updates back to Claude as it works.

## Configuration beyond the basics

### Multi-role LLM routing

Cellar uses different models for different roles (planner, observer, vision, validator). Route each separately in `~/.cellar/config.toml`:

```toml
[llm.planner]
provider = "anthropic"
model = "claude-sonnet-4-5"

[llm.observer]
provider = "gemini"
model = "gemini-2.0-flash"

[llm.vision]
provider = "gemini"
model = "gemini-2.0-flash"

[llm.validator]
provider = "ollama"
model = "gemma3:4b"
```

Good defaults: heavy reasoning for planner, cheap-fast for observer, vision-capable for vision, anything local for validator.

### Chrome CDP auto-detection

If Chrome is running with remote debugging enabled, Cellar auto-detects and fuses CDP context with the accessibility tree. Launch Chrome with:

```bash
open -a "Google Chrome" --args --remote-debugging-port=9222
```

When CDP is active, browser elements get richer context (exact bounding boxes, shadow DOM access, network state). The agent doesn't need to know — CDP is treated as another source with higher confidence.

### Audio / transcription (optional)

If you want the Cortex to include transcribed audio in its world model:

```toml
[audio]
whisper_endpoint = "https://api.openai.com/v1/audio/transcriptions"
whisper_api_key  = "sk-..."
whisper_model    = "whisper-1"
```

Works with any Whisper-compatible endpoint (OpenAI, self-hosted, etc.).

## What's next

- **Use every tool** — see [mcp-server.md](mcp-server.md) for mode/action reference
- **Run autonomous goals** — `cel_think run_goal` is the entry point for multi-step execution
- **Build a custom adapter** — see [building-adapters.md](building-adapters.md) for adapting Cellar to new applications
- **Troubleshoot** — see [troubleshooting.md](troubleshooting.md) for common issues

## Other clients

The MCP config shape is the same for any client. Locations differ:

- **Cursor** — `~/.cursor/mcp.json`
- **Claude Desktop** — `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Any custom client** — pass the same `command` / `args` / `env` through the MCP SDK

File an issue if you want step-by-step docs for a specific client.
