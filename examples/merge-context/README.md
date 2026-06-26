# Example: Merge Context

Goal: produce one `ContextSnapshot` from any set of streams: metrics, logs,
tickets, traces, browser state, app APIs, database rows, or UI facts.

## Run The Generic Snapshot Example

```sh
cargo run -p cel-context --example context_snapshot -- --json
```

This builds a serialized `ContextSnapshot` from example stream facts. Any adapter,
runtime, service, or script can emit the same shape.

## Emit Your Own ContextElement

```rust
use cel_context::{ContentRole, ContextElement, ContextSource};
use std::collections::HashMap;

let error_rate = ContextElement {
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

Any stream that can emit this shape can participate. `cel-context` does not care
whether the fact came from a browser, a log pipeline, a SaaS API, a database, a
support queue, or a local desktop adapter.

## Merge Contributions

```rust
use cel_context::{ContextContribution, ContextMerger};

let mut merger = ContextMerger::new().with_defaults("Incident Review", "Checkout Flow");
merger.push(ContextContribution::new("metrics_stream", vec![error_rate]));

let snapshot = merger.build();
assert_eq!(snapshot.elements.len(), 1);
```
