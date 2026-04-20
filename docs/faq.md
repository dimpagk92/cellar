# FAQ

## What is Cellar, in one sentence?

A computer use runtime that reads the screen through accessibility trees and native APIs — not just screenshots — and ships as an MCP server so any agent can use it.

## How is this different from Browser-Use or Stagehand?

Browser-Use and Stagehand are outstanding for browser automation. They don't touch native desktop apps — that's out of scope by design.

Cellar handles both. It uses the same CDP primitives for browsers *and* a structured accessibility bridge for native macOS apps, fused into one context stream. When a workflow crosses from a browser into Excel or Finder, Cellar's Cortex tracks that transition; a browser-only runtime simply stops working.

If your workflow is 100% browser, Browser-Use / Stagehand are great and simpler. If it ever leaves the browser, you need what Cellar provides.

## How is this different from Anthropic's computer-use API?

Anthropic's API is vision-first: every step sends a screenshot to Claude and waits for it to reason about what it sees. That's powerful but expensive and slow — each action pays for a full vision inference.

Cellar is structure-first. It reads the accessibility tree, CDP state, and native APIs before touching vision. Vision is the fallback, not the foundation. That makes Cellar roughly 200x cheaper per step for typical workflows, and it works offline with local models like Gemma 4.

You can absolutely combine both. Cellar's `cel_see` tool can still ask a vision model to describe pixels when structure is ambiguous — that's what the escalation ladder is for.

## Does it work offline / with local models?

Yes. The structured perception layer (a11y tree, CDP, input, network) is entirely local. The LLM is pluggable: point Cellar at Ollama with Gemma 4 E4B and the whole stack runs on your machine. No network calls, no telemetry.

This is a real differentiator for privacy-sensitive use cases — healthcare data entry, finance, legal, anything where screenshots of user workflows leaving the machine is a non-starter.

## Does it send data to your servers?

No. Cellar has no backend. No telemetry. No analytics. The only network calls it makes are to the LLM provider *you* configure (and those only happen when an agent asks an LLM to reason about something).

## Which OS does it support?

- **macOS 13+** — production-ready. Primary platform.
- **Linux** — working. AT-SPI2 accessibility bridge, CDP, input injection all functional. Ubuntu 22.04+ recommended.
- **Windows** — planned. UI Automation bridge is designed but not implemented.

## Which LLM providers does it support?

- **Cloud**: OpenAI, Anthropic, Google Gemini
- **Local**: Ollama (Gemma 4, Llama 3, any model Ollama can serve)
- **Compatible**: any OpenAI-compatible endpoint (LiteLLM, vLLM, etc.)

Per-role routing is supported — use a heavy model for planning, cheap-fast for observation, something vision-capable for vision, anything local for validation.

## Which models do you recommend?

For day-to-day use, **Gemini 2.0 Flash** is the sweet spot — cheap, fast, and scores as well as larger models on CEL's benchmarks for most tasks. For hard multi-step goals, **Claude Sonnet 4.5** as planner + Gemini as observer is a strong combo. For fully-local, **Gemma 4 E4B** via Ollama handles ~80% of tasks.

## Is it really "best in class"?

For the combination of (hybrid runtime + structure-first perception + MCP-native), yes. No other OSS project does all three today.

For **pure browser automation**, Browser-Use and Stagehand are state-of-the-art and we won't claim otherwise. Cellar is competitive but not demonstrably better on browser-only benchmarks.

For **crossing the browser-to-native boundary** (hybrid workflows), Cellar is the only OSS runtime we know of that handles the full pipeline.

We publish benchmark methodology, not just numbers — run the eval yourself to verify.

## How fast is it?

Context extraction: 100–400ms for 500+ elements, no LLM needed. That's a structural read of the screen, not an LLM call. For comparison, a vision-first approach spends 1–5 seconds per step on the LLM alone.

End-to-end action latency (see screen → plan → execute) depends on the LLM. With Gemini 2.0 Flash it's typically 1–3 seconds. With local Gemma 4, 2–5 seconds.

## What can't it do?

Honest list:
- **Mobile automation** (iOS, Android) — not on the roadmap
- **Windows** — planned, not shipped
- **Anything captcha-gated** — agents bypassing captchas is a dead end we're not pursuing
- **"Browse the web like a human"** for 30 minutes unattended — long-horizon autonomy is still an open research problem; we're honest about the 70–80% success rate on complex goals
- **Replace deterministic scripts** for well-understood repetitive tasks — if you have a selector-stable workflow, Playwright is faster and more reliable

## How do I build a custom adapter?

See [building-adapters.md](building-adapters.md). Adapters wrap a native API (COM, scripting, CLI) and surface elements with higher confidence than the accessibility tree alone provides. Good candidates: Excel, Figma, Logic Pro, terminal-based software — anything with a structured API that vision can't see into.

Adapter-common is the trait; implement it for your app, register it, and the Cortex will fuse your data with the other sources.

## Is this legit OSS or a "source-available" bait-and-switch?

Apache 2.0, no patent traps, no CLA required, no "community edition vs enterprise" paywall. See [LICENSE](../LICENSE).

The commercial plans (managed cloud, hosted workers) are separate code that depends on this OSS — they don't restrict what you can do with the code in this repo.

## Can I use Cellar in a commercial product?

Yes. Apache 2.0 permits commercial use. You can fork, modify, embed, resell — subject to the license terms (attribution, preserve the notice, etc.). We'd love to know about it.

## How do I report a security issue?

Open a [private security advisory](https://github.com/dimpagk92/cellar/security/advisories/new). Don't open a public issue. See [SECURITY.md](../SECURITY.md) for the full policy.

## How do I contribute?

Start with [CONTRIBUTING.md](../CONTRIBUTING.md). Areas we want help with:
- Windows UI Automation bridge
- More application adapters
- Tests on platforms we don't run day-to-day
- Docs and examples (especially tutorials)
- Reproducing + triaging issues

## Is there a Discord / community space?

Use [GitHub Discussions](https://github.com/dimpagk92/cellar/discussions) for questions and show-and-tell. We'll set up a Discord if Discussions feels too asynchronous — watch for an announcement if activity picks up.

## Will this get funded / acquired / pivot?

The OSS code is Apache 2.0 — whatever happens to Cellar as a commercial entity, the runtime in this repo stays open, forkable, and usable.
