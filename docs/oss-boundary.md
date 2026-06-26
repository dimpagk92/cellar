# OSS / Commercial Boundary

Cellar is developed from a private source-of-truth repository and mirrored into the public OSS repository on release. This document describes the target boundary for that mirror.

## Boundary Thesis

The OSS project should be the **context, memory, brief, and contract layer** that lets the community build against CEL-shaped data:

- many sources merged into one canonical context snapshot (`cel-context`)
- durable agent memory contracts and a local SQLite backend (`cel-memory`, `cel-memory-sqlite`)
- governed per-turn model briefing (`cel-brief`)
- transport and receipt schemas that make MCP / CLI / SDK calls inspectable

The commercial product should be the **live operating and governance layer**:

- the continuous runtime that keeps context true over time (`cel-cortex`)
- policy enforcement, monitoring, audit timelines, alerting, compliance exports
- the Tauri app, hosted workers, control plane, cloud, billing, and fleet operations

In short: **OSS = context/memory/brief/contracts. Commercial = live cortex runtime + governance/compliance operations.**

## Target License Map

The repo is still in transition, so this table is the target boundary for mirror and packaging decisions:

| Path | License | Destined for |
|---|---|---|
| `cel/cel-context`, `cel/cel-contracts` | Apache 2.0 | Public repo |
| `cel/cel-memory`, `cel/cel-memory-sqlite`, `cel/cel-brief` | Apache 2.0 | Public repo / crates.io candidates |
| Receipt, event, MCP, CLI, and SDK schemas | Apache 2.0 | Public repo |
| Substrate/helper crates required by the OSS contracts | Apache 2.0 / MIT | Public repo when needed by the extraction allowlist |
| `mcp-server/`, `cli/` | Apache 2.0 | Public repo as transport surfaces and local entry points |
| Adapter SDK / adapter contracts | MIT / Apache 2.0 | Public repo |
| Example adapters, cookbooks, benchmarks, eval harness, docs | Apache 2.0 / MIT | Public repo |
| `cel/cel-cortex` | Proprietary by default | Commercial runtime unless explicitly promoted later |
| `cellar-runtime`, `cellar-act-gateway`, rule stores, policy wiring | Proprietary by default | Commercial governance / monitoring layer |
| `app/` (Tauri desktop app) | Proprietary | Commercial only |
| Future: `control-plane/`, `cloud/`, `billing/` | Proprietary | Commercial only |

## Rule Of Thumb

Anything a developer needs to emit, merge, store, brief, or inspect CEL-shaped data is OSS. Anything that continuously operates the machine, enforces product policy, monitors fleets, or presents compliance workflows is commercial unless explicitly carved out.

Classification notes:

- `cel-context` is the community-facing context standard: external systems should be able to map their sources into `ContextElement` / `ScreenContext` without adopting the full runtime.
- `cel-cortex` is the live engine: continuous perception, freshness, diffs, anomaly handling, adapter/source prioritization, planning views, dispatch integration, and monitoring hooks. Keep it commercial until there is a deliberate reason to open it.
- Receipts and audit records split in two: the **schema / contract** is OSS so users can inspect what happened; the **storage, UI, policy engine, alerts, and compliance exports** are commercial product.
- MCP and CLI remain OSS transports. They are not the product identity; they are ways to access the context/brief/action contracts.

## Repository And Package Strategy

Keep the namespace stable:

- GitHub umbrella repo: `github.com/dimpagk92/cellar`
- crates.io owner: `dimpagk92`
- crate names: `cel-context`, `cel-memory`, `cel-memory-sqlite`, `cel-brief`, and `cel-contracts` when needed

Do **not** create one GitHub repo per crate right now. Rust users consume the
pieces independently through crates.io, while the umbrella repo keeps shared
docs, examples, issues, and cross-crate design together.

```text
Distribution unit: crate
Source/docs unit: github.com/dimpagk92/cellar
```

This means a memory-only user can depend on `cel-memory` /
`cel-memory-sqlite` without touching `cel-context`, while the project still
presents CEL as one coherent contract ecosystem.

## Related

- [commercial-model.md](commercial-model.md) — how this boundary maps to revenue.
- [what-cel-is.md](what-cel-is.md) — user-facing positioning.
- [ROADMAP.md](ROADMAP.md) — repo split timing.
