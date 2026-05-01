# What CEL Is (and Isn't)

This doc answers: what is CEL, and where does it fit next to Playwright, browser-use, Stagehand, Anthropic computer-use, and OpenClaw? It's the top-of-funnel positioning doc for agent developers, platform architects, and adopters evaluating whether CEL belongs in their stack.

## One-Sentence Definition

**CEL is agent-agnostic infrastructure for computer use — perception, execution, and adapters — exposed as MCP tools.**

That sentence carries most of the load:

- *Agent-agnostic*: CEL does not assume a specific planner. LangGraph, Mastra, Codex, Claude, Claude Code, Gemini, Cursor, n8n, and in-house runtimes are all first-class clients. See [adapters-cel-agents.md](adapters-cel-agents.md) for the three-layer architecture.
- *Infrastructure*: not an end-user product. CEL is what agent platforms build on top of.
- *Perception, execution, adapters*: the three capabilities CEL ships today.
- *MCP tools*: four tools — `cel_see`, `cel_act`, `cel_perceive`, `cel_think` (the last is optional). See [mcp-server.md](mcp-server.md).

## CEL Is

- **Device understanding on macOS.** Accessibility (AX), Chrome DevTools Protocol (CDP), vision, input/focus/process signals, and audio are fused into one stable `ContextElement` stream. Any single stream lies; the fusion does not.
- **A stable action surface.** `cel_act` exposes a canonical set of primitives (click, type, scroll, navigate, key, wait-for, etc.) that behave the same across adapters. Actions return a typed `ActionResult` with success/failure, latency, and anomaly flags.
- **An adapter layer for app-specific truth.** AX is a generic substrate; adapters encode application-specific structured truth (a Numbers cell read is not a UI guess — it's a model read). First-party adapters today: `browser` (prod) plus stubs for `excel`, `bloomberg`, `metatrader`, `sap-gui`. Third parties can ship adapters outside this repo.
- **A reference planner in-tree, optional.** `cel-goal-runner` and friends exist so the repo is runnable end-to-end, but they are clients of CEL, not CEL's identity. Replacing them with your own agent is the intended path.

## CEL Is Not

- **An opinionated planner.** We ship a reference planner; we do not ship *the* planner. Planning is commoditizing — every frontier LLM can plan. Perception + execution on real devices is not commoditizing.
- **Tied to one agent framework.** LangGraph, Mastra, and similar are integrations, not dependencies. If Mastra disappears tomorrow, CEL keeps working.
- **A web-only tool.** The browser is one adapter. CEL is a desktop runtime — native macOS apps are peers of browser contexts, not a stretch goal.
- **A multi-provider LLM router as a product.** `cel-llm` exists internally to drive the reference planner. It is not exposed, stabilized, or marketed as a model router. Don't depend on it.

## Where CEL Fits

| Feature                 | CEL                         | Playwright          | browser-use         | Stagehand v3         | Anthropic computer-use | OpenClaw                  |
|-------------------------|-----------------------------|---------------------|---------------------|----------------------|------------------------|---------------------------|
| Scope                   | Desktop + browser (macOS)   | Browser only        | Browser only        | Browser only         | Desktop + browser      | Context/memory framework  |
| Agent-agnostic          | Yes                         | N/A (no agents)     | No (built-in loop)  | Partial (own planner)| Anthropic-model-specific | Yes                     |
| Adapter extensibility   | Yes (third-party supported) | No (library API)    | No                  | Limited              | No                     | N/A                       |
| Local vs remote         | Local today; remote Phase 1 | Local               | Local               | Local / cloud        | Remote (Claude hosted) | Local                     |
| License                 | Apache 2.0                  | Apache 2.0          | MIT                 | MIT                  | Proprietary            | Apache 2.0                |
| Built-in planner        | Reference only, optional    | None                | Yes, opinionated    | Yes                  | Implicit in the model  | No (context layer)        |
| Eval harness included   | Yes                         | No                  | Partial             | Partial              | No                     | No                        |

### Reading the table honestly

- **Playwright** is a browser automation library. It has no agent, no perception beyond the DOM, and no concept of adapters. It's what you reach for when a script-level solution is enough. CEL uses Playwright/CDP under the hood in its browser adapter.
- **browser-use** ships with a built-in planning loop. It's excellent for "point an LLM at a browser and go," but less useful if you already have an agent and want just the perception/execution surface.
- **Stagehand v3** has its own planner and a focused browser surface. Strong for TypeScript teams who want a batteries-included web agent; less of a fit if you need native apps or a non-TS runtime.
- **Anthropic computer-use** is the closest spiritual peer: it's infrastructure + primitives. The difference is tight coupling to the Anthropic model and hosted execution. CEL is the self-hosted, agent-agnostic analogue on macOS.
- **OpenClaw** is a context/memory management framework. It complements CEL (you can use both); it does not replace perception or execution.

## When CEL Fits vs. Doesn't Fit

| CEL is a good fit when…                                              | CEL is the wrong tool when…                                        |
|----------------------------------------------------------------------|--------------------------------------------------------------------|
| You already have an agent and need device understanding on macOS.    | Your target is browser-only and script-level automation is enough. |
| You want to plug in different planners without rewriting perception. | You need Windows or Linux desktop today (Linux comes in Phase 1).  |
| You need native-app automation alongside browser automation.         | You want an opinionated, batteries-included browser agent.         |
| You want an open, self-hostable runtime (not a hosted-only API).     | You're locked into a single LLM vendor and want their native stack.|
| You plan to extend with third-party adapters (app-specific truth).   | You don't want to run any runtime locally at all.                  |

## The Thesis in One Paragraph

Planning is commoditizing fast. Every frontier LLM can plan, and planning-focused wrappers are a crowded category with shrinking margins. Perception and execution on real devices are not commoditizing — they require per-OS, per-app engineering that does not fall out of a bigger model. CEL's durable surface is device mastery plus a stable, agent-agnostic API that outlasts any single framework or model. That is the same shape as Anthropic's computer-use (infrastructure + primitives) and OpenClaw (stable context/memory plumbing) — two recent peers that chose the same layer.

## What This Means For You

- **If you already have an agent and need device understanding on macOS, CEL is the layer.** Drop it behind your planner over MCP, keep your orchestration, and stop writing per-app scrapers.
- **If you're starting from scratch and want a batteries-included browser agent, CEL is probably too low-level.** Use browser-use or Stagehand and come back when you outgrow them.
- **If you're building an agent platform**, CEL is the closest thing to an unopinionated substrate you can standardize on.

## Status and Scope Today

- Single supported OS: **macOS**. Linux worker lands in Phase 1 ([ROADMAP.md](ROADMAP.md)). Windows is explicitly not on the roadmap.
- Four MCP tools: `cel_see`, `cel_act`, `cel_perceive`, `cel_think` (optional).
- First-party adapters: `browser` (prod) + 4 stubs (`excel`, `bloomberg`, `metatrader`, `sap-gui`).
- Benchmarks: Hybrid suite (5 tasks), general web suite (50+). See [eval-leaderboard.md](eval-leaderboard.md).
- License: Apache 2.0 (flipped from BSL 1.1 on 2026-04-19; see [oss-boundary.md](oss-boundary.md)).

## Related Reading

- [adapters-cel-agents.md](adapters-cel-agents.md) — the three-layer north star.
- [commercial-model.md](commercial-model.md) — how open-core funds this project.
- [gtm-icp.md](gtm-icp.md) — who we think CEL is for first.
- [eval-leaderboard.md](eval-leaderboard.md) — how we measure "works with any agent."
- [stability.md](stability.md) — what the API commits to.
- [security-review-plan.md](security-review-plan.md) — the security roadmap.
- [README.md](../README.md)
