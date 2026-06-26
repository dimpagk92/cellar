# Example: Build Brief

Goal: assemble model input from pluggable sources and inspect the
`BriefReceipt`.

## Standalone Brief

```sh
cargo run -p cel-brief --example no_cellar
```

This example registers static sources, forces budget pruning, and prints both
the `Brief` and the receipt.

## Brief With Memory

```sh
cargo run -p cel-brief --features memory --example with_memory
```

This example wires `MemorySource` over a `cel_memory::MemoryProvider`.

## Integration Pattern

```rust
use cel_brief::{
    BriefBuilder, BriefContext, SystemPromptSource, TokenBudget, UserMessageSource,
};
use std::sync::Arc;

let ctx = BriefContext::new(TokenBudget::new(8_000, 512))
    .with_goal("answer the user's request")
    .with_user_message("What changed in the last run?");

let brief = BriefBuilder::new()
    .source(Arc::new(SystemPromptSource::new("You are grounded in CEL data.")))
    .source(Arc::new(UserMessageSource::new()))
    .build(&ctx)
    .await?;

println!("model sees {} message(s)", brief.messages.len());
println!("receipt tokens={}", brief.receipt.total_tokens);
```

Production callers add memory, history, tool, and perception sources. The brief
contract stays provider-agnostic.
