# Launch Messaging

Use this copy when announcing the OSS direction, writing crates.io descriptions,
or explaining the commercial boundary.

## One-Liner

CEL is the open context and trust data plane for AI agents: fuse context, persist
memory, brief models, and inspect what agents saw and did.

## Short Description

CEL standardizes the data plane around agent operations. It gives developers
open Rust crates for canonical context snapshots, durable memory, governed model
briefing, and receipt contracts. The full Cellar/Dilipod runtime operates those
contracts continuously with live cortex, governance, monitoring, compliance, and
hosted execution.

## Developer Pitch

If you already have an agent, CEL gives it a common language for:

- what the agent can see
- what should persist across turns
- what the model should see this turn
- what was dispatched and what evidence remains

Start with `cel-context`, `cel-memory`, `cel-memory-sqlite`, and `cel-brief`.

## Namespace

Keep the public namespace simple:

- GitHub umbrella: `github.com/dimpagk92/cellar`
- crates.io owner: `dimpagk92`
- distribution unit: individual `cel-*` crates

Do not announce new per-crate repos. A developer who only wants memory installs
`cel-memory` / `cel-memory-sqlite`; a developer who wants context installs
`cel-context`.

## Commercial Pitch

Open CEL defines the contracts. Cellar/Dilipod operates them:

- live cortex runtime
- policy enforcement and approvals
- monitoring and audit timelines
- compliance exports
- hosted workers and fleet operations

## Suggested Announcement

We are refocusing CEL OSS around the open contracts that agent builders can use
without adopting the full Cellar runtime:

- `cel-context` for canonical context snapshots
- `cel-memory` for durable memory contracts
- `cel-memory-sqlite` for local-first storage
- `cel-brief` for governed per-turn model input
- `cel-contracts` for action, planning, and receipt schemas

MCP, CLI, and SDK surfaces remain useful transports, but the OSS identity is now
the context/memory/brief/receipt data plane. The live cortex runtime and
governance/compliance product remain the commercial operating layer.

## Avoid

- “Cellar OSS is the full runtime.”
- “Cortex is open by default.”
- “MCP for computer use” as the headline.
- “Adapters are the marketplace/product.”

## Preferred Phrases

- “Open context and trust contracts for agents.”
- “The data plane for agent operations.”
- “Open contracts, commercial operations.”
- “CEL standardizes what agents see, remember, send to models, and prove later.”
