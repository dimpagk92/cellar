# Example: Receipt Inspection

Goal: understand what a CEL receipt proves and what it does not prove.

## Brief Receipt

Run:

```sh
cargo run -p cel-brief --example no_cellar
```

Look for:

- `total_tokens`
- `dropped`
- `redactions`
- `by_source`
- timing fields

This answers: what did the model see, and why?

## Execution Receipt

`cel-contracts` defines the runtime receipt schema:

```rust
use cel_contracts::{
    DispatchRoute, ExecutionReceipt, ObservedEffect, ReceiptStatus,
};

let receipt = ExecutionReceipt {
    receipt_id: "example-1".into(),
    run_id: Some("run-123".into()),
    trace_id: None,
    action_kind: "click".into(),
    target: Some("button:submit".into()),
    route: DispatchRoute::Accessibility,
    observed_effect: ObservedEffect::not_checked(),
    evidence: vec![],
    requested_at_ms: 1_000,
    completed_at_ms: 1_030,
    duration_ms: 30,
    status: ReceiptStatus::Ok,
    error: None,
};
```

This answers: what did the runtime dispatch, through which route, and what did
it observe?

## Completion Proof

Receipts are not enough for final claims. Pair them with evidence:

- adapter readback
- CDP / AX state
- a fresh `ScreenContext`
- screenshot evidence
- external system confirmation

Commercial Cellar/Dilipod stores, filters, alerts on, and exports these records.
The schema remains open.
