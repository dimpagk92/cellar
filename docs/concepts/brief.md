# Briefs

`cel-brief` answers:

> What should the model see this turn?

It does not discover live truth and it does not store memory. It assembles
already-available sources into a provider-agnostic `Brief`.

## Core Types

- `Source` — async contributor of prompt material.
- `Contribution` — one source's candidate content.
- `BriefBuilder` — fans out to sources, budgets, prunes, applies governance.
- `Brief` — structured output: system, messages, tools, receipt.
- `BriefReceipt` — what was included, dropped, redacted, and budgeted.
- `Governance` — hook that may allow, redact, or reject a brief.

## Why It Exists

Every non-trivial agent loop eventually has to combine:

- system prompt
- user message
- memory retrieval
- recent history
- tool catalog
- current context or perception snapshot
- policy redactions
- token budget pressure

Without a brief layer, that logic becomes ad-hoc string concatenation inside the
agent loop. `cel-brief` turns it into a testable, receipted pipeline.

## Source Model

```text
SystemPromptSource
UserMessageSource
HistorySource
MemorySource
PerceptionSource
ToolCatalogSource
        ↓
BriefBuilder
        ↓
Brief + BriefReceipt
```

Any downstream runtime can implement its own sources. The `perception` feature
defines a generic perception trait; it does not depend on `cel-cortex`.

## Examples

Standalone:

```sh
cargo run -p cel-brief --example no_cellar
```

With memory:

```sh
cargo run -p cel-brief --features memory --example with_memory
```

## Governance Boundary

The OSS crate exposes the governance hook and records redactions in the receipt.
The commercial product can provide the policy store, approvals, review UI, and
compliance reporting.

See [../../examples/build-brief](../../examples/build-brief).
