# cel-brief

The per-turn LLM briefing layer. Assemble memory + perception + history + tools + user message into one budgeted, governed, receipted bundle from pluggable streams.

**Status:** Phase 0 scaffold. Not yet usable. See [the implementation plan](https://github.com/dimpagk92/cellar/blob/main/plans/cellar-cel-brief.md) for the 4-phase roadmap.

## Why

Every non-trivial AI agent has to decide what to put in the prompt: memory retrievals, current screen state, recent actions, tool schemas, the user's message. Today everyone solves it ad-hoc with string concatenation inside the agent loop, with homegrown token budget code. `cel-brief` is the abstraction that fills that gap.

## Design

Three commitments:

1. **Everything is a `Source`.** Memory, perception, history, tools — all the same trait. Plug in [`cel-memory`](https://github.com/dimpagk92/cellar/tree/main/cel/cel-memory), Mem0, Letta, or your own — `cel-brief` doesn't care.
2. **Structured output.** `Brief { messages, tools, system, receipt }` is provider-agnostic; renderers map to OpenAI / Anthropic / local-model wire formats.
3. **Governance and budget are first-class.** Importance scoring, redaction hooks, token budgets, receipts — built in, not bolted on.

## Phase 2 API sketch

```rust
let brief = BriefBuilder::new()
    .source(SystemPromptSource::new("You help with code."))
    .source(UserMessageSource)
    .source(MemorySource::new(memory.clone(), 8, MemoryQueryStrategy::Hybrid))
    .source(ToolCatalogSource::new(tools))
    .budget(TokenBudget::for_model("gpt-4o"))
    .build(ctx).await?;

let response = openai.chat(brief.to_openai_request()).await?;
println!("brief receipt: {} tokens, {} dropped, {} redactions",
    brief.receipt.total_tokens,
    brief.receipt.dropped.len(),
    brief.receipt.redactions.len());
```

## Comparison

| | cel-brief | LangChain prompts | LlamaIndex ServiceContext | ad-hoc concat |
|---|---|---|---|---|
| Pluggable sources | ✓ | ✗ (template-only) | partial | n/a |
| Token budgeting | ✓ | ✗ | ✗ | rebuilt each time |
| Importance-aware pruning | ✓ | ✗ | ✗ | rebuilt each time |
| Governance/redaction hooks | ✓ | ✗ | ✗ | n/a |
| Provider-agnostic | ✓ | partial | ✓ | depends |
| Receipts | ✓ | ✗ | ✗ | ✗ |
| Language | Rust | Python | Python | n/a |

## Features

- `memory` — enable `MemorySource<P>` (depends on [`cel-memory`](https://github.com/dimpagk92/cellar/tree/main/cel/cel-memory)).
- `perception` — enable `PerceptionSnapshot` trait + `PerceptionSource<P>` (perception backends live downstream, e.g., Cellar's `cel-cortex`).

## License

Apache-2.0
