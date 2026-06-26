# Context

`cel-context` is a portable shape for turning arbitrary streams into agent
context. It answers:

> How do many streams describe the current situation in a shape every agent can consume?

## Purpose

Use `cel-context` when an agent needs facts from multiple stream types but should
not know each stream's native format. Each source normalizes its facts into
`ContextElement`s, groups them as a `ContextContribution`, and the
`ContextMerger` produces one point-in-time `ContextSnapshot`.

## Core Types

- `ContextElement` — one normalized fact: UI control, log line, metric, alert, ticket, trace span, row, or app event.
- `ContextSnapshot` — one point-in-time snapshot containing normalized elements, transcripts, and observed events.

  In 0.1.x this type was also available as `ScreenContext`; that alias was
  removed in **0.2.0**. See [../migration-0.2.md](../migration-0.2.md).
- `ContextSource` — where an element came from: external stream, accessibility, CDP, native API, OCR, vision, merged.
- `ContextReference` — resilient targeting data that survives ephemeral element IDs.
- `ContentRole` — prompt-injection-aware classification: interactive, content, decorative, system.
- `ContextContribution` / `ContextMerger` — merge normalized source outputs into one snapshot.

## Why It Exists

Agents should not couple themselves to one source or telemetry format:

- browser DOM describes what is visible in web apps
- logs and traces describe what happened inside services
- metrics describe health, latency, rates, and saturation
- tickets and chats describe what users are reporting
- database rows and domain APIs describe business state
- accessibility, OCR, and vision describe software surfaces when needed

`cel-context` gives all of those streams one canonical snapshot language.

## Minimal Shape

```rust
use cel_context::{ContextElement, ContextSource, ContentRole};
use std::collections::HashMap;

let element = ContextElement {
    id: "metric:checkout:error_rate".into(),
    label: Some("Checkout error rate is above threshold".into()),
    description: None,
    element_type: "metric".into(),
    value: Some("7.2%".into()),
    bounds: None,
    state: Default::default(),
    parent_id: None,
    actions: vec![],
    confidence: 0.88,
    source: ContextSource::External,
    content_role: ContentRole::Content,
    properties: HashMap::new(),
};
```

## Boundary

`cel-context` can build snapshots from normalized contributions through
`ContextMerger`, but it does not prescribe how you collect, rank, refresh, store,
or act on those streams. Those choices stay with your runtime, agent framework,
adapter, or product.

## Example

```sh
cargo run -p cel-context --example context_snapshot -- --json
```

See [../../examples/merge-context](../../examples/merge-context).
