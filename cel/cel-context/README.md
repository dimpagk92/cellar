# cel-context

Portable context snapshots for AI agents.

`cel-context` gives any stream a common shape an agent can consume. Browser DOM,
accessibility trees, logs, traces, metrics, tickets, database rows, app events,
OCR, and domain APIs can all emit `ContextElement`s and merge into one
`ContextSnapshot`.

`ContextSnapshot` is a point-in-time operating context: the facts an agent
should know before it plans, answers, or acts.

## Purpose

Use `cel-context` when you have multiple streams and want to make them useful to
an agent without coupling the agent to each stream's native format.

```text
metrics / logs / traces / tickets / DOM / APIs / database rows
        -> ContextElement[]
        -> ContextContribution
        -> ContextMerger
        -> ContextSnapshot
        -> agent, planner, brief builder, monitor, or evaluator
```

The crate does not collect the streams for you. It defines the portable contract
they can emit.

## What's Included

- `ContextElement` — one source-derived fact: UI control, log line, metric, alert, ticket, row, or event.
- `ContextSnapshot` — one point-in-time snapshot of normalized context elements.
- `ContextSource` — source labels such as external stream, accessibility, CDP, native API, OCR, vision, merged.
- `ContentRole` — prompt-injection-aware classification.
- `ContextReference` — resilient targeting data that survives ephemeral IDs.
- `ContextContribution` / `ContextMerger` — generic source contribution merge mechanics.

## Out Of Scope

- Capturing live desktop, browser, OCR, vision, network, log, trace, metric, ticket, or database streams.
- Deciding freshness, anomaly, prioritization, or policy rules over time.
- Dispatching actions back into an app or browser.
- Hosting dashboards, audit timelines, or compliance exports.

Those responsibilities belong in whichever runtime, adapter, or product embeds
the snapshot contract.

## Example

```sh
cargo run -p cel-context --example context_snapshot -- --json
```

The example constructs a snapshot from caller-provided stream facts. Any source
that can produce `ContextElement`s can participate.

## Minimal Element

```rust
use cel_context::{ContentRole, ContextElement, ContextSource};
use std::collections::HashMap;

let element = ContextElement {
    id: "incident:metric:error_rate".into(),
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

## License

Apache-2.0
