# cel-brief

Composable prompt briefing for AI agents. Gather memory, perception, history,
tools, and user messages from pluggable sources; enforce token budgets; apply
governance; and emit receipts.

**Status:** Phases 1–4 shipped — core types, the `Source` trait and all built-in sources, the `Governance` trait, and the `BriefBuilder` (tokenizer + priority/budget pruning + `BriefReceipt`). The crate is feature-complete for assembling per-turn briefs.

## Purpose

Use `cel-brief` when an agent has many possible prompt inputs and needs one
structured, budgeted, governed package for a model call. Sources contribute
facts, messages, tools, history, or memory; the builder prunes to budget, runs
governance, and emits a receipt of what the model saw.

## Why

Every non-trivial AI agent — chat, code, computer-use, robotics — has to decide
what to put in the prompt: memory retrievals, current screen state, recent
actions, tool schemas, and the user's message. Today many projects solve that
inside the agent loop with string concatenation and homegrown budget code.
`cel-brief` makes that step explicit, structured, and inspectable.

## Three commitments

1. **Everything is a `Source`.** Memory, perception, history, tools — all the same trait. Plug in [`cel-memory`](../cel-memory), Mem0, Letta, or your own — `cel-brief` doesn't care.
2. **Structured output, not a string.** `Brief { messages, tools, system, receipt }` is provider-agnostic; renderers map to OpenAI / Anthropic / local-model wire formats.
3. **Governance and budget are first-class.** Importance scoring, redaction hooks, token budgets, receipts — built in, not bolted on.

## Comparison

| | **cel-brief** | LangChain prompts | LlamaIndex `ServiceContext` | ad-hoc concat |
|---|---|---|---|---|
| Pluggable sources | ✓ trait-based | ✗ template strings | partial (retrievers only) | n/a |
| Token budgeting | ✓ priority-aware, per-source floor | ✗ caller's problem | ✗ caller's problem | ✗ rebuilt each turn |
| Importance-aware pruning | ✓ `[0.0, 1.0]` per contribution | ✗ | ✗ | ✗ |
| Governance / redaction hooks | ✓ `Governance` trait + receipts | ✗ | ✗ | ✗ |
| Receipts (what the model saw and why) | ✓ `BriefReceipt` with per-source stats | ✗ | ✗ | ✗ |
| Provider-agnostic output | ✓ structured `Brief` | partial | ✓ | depends |
| Async fan-out across sources | ✓ `async fn contribute` | ✗ sync | ✓ | n/a |
| Memory integration as a Source | ✓ `MemorySource` over any `MemoryProvider` | partial (`Memory` class) | ✓ | n/a |
| Perception / screen state as a Source | ✓ `PerceptionSource` over any backend | n/a | n/a | n/a |
| Language | Rust | Python | Python | n/a |

## Built-in sources

| Source | Feature | Priority | Notes |
|---|---|---|---|
| `SystemPromptSource` | default | Critical | Static system text. Never redactable. |
| `UserMessageSource` | default | Critical | Pulls `ctx.user_message`. Never redactable. |
| `ToolCatalogSource` | default | High | Owns `Vec<ToolSchema>`. |
| `HistorySource<H>` | default | Normal | Window of past N entries from any `HistoryStore`. Redactable. |
| `MemorySource<P>` | `memory` | Normal | Hybrid retrieval over any `cel_memory::MemoryProvider`. Redactable. |
| `PerceptionSource<P>` | `perception` | High | Defines the `PerceptionSnapshot` trait; downstream runtimes adapt their own perception engine into it. Redactable. |

## Quick start

```rust
use cel_brief::{
    BriefContext, BriefError, Source, SourceError, SystemPromptSource, TokenBudget,
    UserMessageSource,
};

let ctx = BriefContext::new(TokenBudget::default())
    .with_user_message("Hello!");

let sys = SystemPromptSource::new("You are a helpful assistant.");
let user = UserMessageSource::new();

let cs = sys.contribute(&ctx).await?;
assert_eq!(cs.len(), 1);
# Ok::<_, BriefError>(())
```

See [`examples/standalone.rs`](examples/standalone.rs) for a self-contained hand-fanout and [`examples/with_memory.rs`](examples/with_memory.rs) for the `cel-memory` integration:

```sh
cargo run -p cel-brief --example standalone
cargo run -p cel-brief --features memory --example with_memory
cargo run -p cel-brief --example governance
```

The `governance` example shows a custom redaction hook and the resulting
`BriefReceipt` redaction records.

## `BriefBuilder`

The `BriefBuilder` fans out to every registered source, tokenizes and prunes to budget, runs governance, and returns a `Brief` plus its `BriefReceipt`:

```rust
let brief = BriefBuilder::new()
    .source(SystemPromptSource::new("You help with code."))
    .source(UserMessageSource::new())
    .source(MemorySource::new(memory.clone(), "embedded", 8))
    .source(ToolCatalogSource::new(tools))
    .governance(NoOpGovernance)        // swap in your own Governance
    .budget(TokenBudget::new(8000, 1024))
    .build(ctx).await?;

let response = openai.chat(brief.to_openai_request()).await?;
println!(
    "brief receipt: {} tokens, {} dropped, {} redactions",
    brief.receipt.total_tokens,
    brief.receipt.dropped.len(),
    brief.receipt.redactions.len(),
);
```

## Governance

`Governance::review(&mut draft, &ctx)` runs after budget pruning, before the brief is returned. The verdict is one of:

- `Allow` — the brief is fine as-is.
- `Redacted(Vec<RedactionRecord>)` — the hook mutated redactable content; the records describe what changed and which rule did it.
- `Rejected(String)` — policy violation; the builder returns `BriefError::Rejected`.

The default `NoOpGovernance` always allows. Production callers can plug in a real implementation that consults their own rules engine.

## Features

- `memory` — enable `MemorySource<P>` (depends on [`cel-memory`](../cel-memory)).
- `perception` — enable the `PerceptionSnapshot` trait + `PerceptionSource<P>`. Perception backends live downstream: a runtime adapts its own live perception engine into a `PerceptionSnapshot`. This feature adds no dependency on any perception crate.

## Benchmark

A microbenchmark for hand-assembled briefs lives at [`benches/build.rs`](benches/build.rs). Target: under 50 ms p95 for a realistic brief once the `BriefBuilder` ships. Run with:

```sh
cargo bench -p cel-brief --features memory
```

## License

Apache-2.0
