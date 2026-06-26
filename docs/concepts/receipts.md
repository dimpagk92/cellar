# Receipts

CEL uses receipts to separate three claims that are often conflated:

| Claim | Receipt / Evidence |
|---|---|
| What did the model see? | `BriefReceipt` |
| What action did the runtime dispatch? | `ExecutionReceipt` |
| Was the user goal completed? | Post-state evidence, readback, context reread, or runtime diff |

Receipts are open schemas. Audit timelines, retention controls, review UI,
alerting, and compliance exports are commercial product surfaces.

## BriefReceipt

`cel-brief` emits a `BriefReceipt` with:

- total tokens
- per-source contribution stats
- dropped contributions and why they were dropped
- redactions and rule identifiers
- timing for fanout, tokenization, pruning, governance, and total build

Use it to answer:

> What did we send to the model, and what did we intentionally leave out?

## ExecutionReceipt

`cel-contracts` defines `ExecutionReceipt` with:

- `receipt_id`
- optional `run_id` / `trace_id`
- `action_kind`
- target identifier
- actual `DispatchRoute`
- `ObservedEffect`
- evidence references
- timing
- terminal status
- error details

Use it to answer:

> What did the runtime try to do, how did it route the action, and what did it observe afterward?

## Completion Still Requires Evidence

A receipt is not a completion proof by itself.

Examples:

- A click receipt proves dispatch. It does not prove the invoice was submitted.
- A brief receipt proves model input. It does not prove the model followed policy.
- A memory write proves persistence. It does not prove the remembered fact is true.

Completion proof comes from readback:

- adapter read
- CDP / AX state
- context snapshot
- screenshot
- runtime diff
- external system confirmation

## Commercial Boundary

Open CEL should keep receipt schemas inspectable. Commercial Cellar/Dilipod can
add:

- timeline storage
- alerting
- policy packs
- retention controls
- compliance exports
- reviewer workflows

See [../../examples/receipt-inspection](../../examples/receipt-inspection).
