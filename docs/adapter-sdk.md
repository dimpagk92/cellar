# Adapter SDK

This doc is the public contract for anyone writing a CEL adapter. It answers: "I'm a third-party developer. What surface can I target today, what will break, and what will not?" The tutorial for actually implementing one lives in [building-adapters.md](./building-adapters.md) — this doc is the stability and forward-compatibility story around it.

Adapters are Layer 1 in the three-layer model (see [adapters-cel-agents.md](./adapters-cel-agents.md)). They are how app-specific intelligence enters CEL. A good adapter gives CEL a deterministic read/write path into an application that generic AX or vision cannot match.

## Status and Versioning

- **`cel-adapter-sdk`: v0.1.0** (workspace version, MIT). This is the crate you depend on. It holds the `AdapterDriver` trait, the manifest types, `ActionResult`/`AdapterError`, and the discovery/registration helpers — and it depends only on the low-level shared crates (`cel-context`, `cel-contracts`, `cel-cdp`), **not** on the `cel-cortex` engine. Adding an adapter no longer pulls in the perception runtime. Pre-1.0: stable in shape, minor bumps may still change signatures.
- `cel-cortex` re-exports the entire SDK surface (`pub use cel_adapter_sdk::*` from `cel_cortex::adapter`), so older code referencing `cel_cortex::AdapterDriver` keeps compiling — but new adapters should depend on `cel-adapter-sdk` directly.
- A second, simpler trait — `adapter_common::Adapter` — predates the SDK and survives only for four Windows-finance adapters (excel, sap-gui, bloomberg, metatrader) and the NAPI adapter-registry path. **Do not target it for new work.** It will be folded into `cel-adapter-sdk` before v1. See "Which trait do I implement?" below.
- v1 criteria (proposal; TODO: user confirm):
  - `adapter_common::Adapter` retired into `cel-adapter-sdk` (single trait)
  - Manifest schema frozen (additive only after v1)
  - `ContextElement` + `ActionResult` declared stable
  - At least one third-party adapter shipping on the contract
  - `cel-adapter-sdk` published to crates.io
  - Security/review policy in [adapter-security.md](./adapter-security.md) finalized

## Which trait do I implement?

Implement **`cel_adapter_sdk::AdapterDriver`**. Full stop, for all new adapters
— native Rust (implement the trait directly) or any other language (speak the
process protocol in [building-adapters.md](./building-adapters.md), which the
runtime maps onto the same trait).

You may notice `adapter_common::Adapter` while reading the tree. It is the older,
narrower trait kept alive for the four legacy Windows-finance adapters and the
NAPI registry only. It is not the contract — `AdapterDriver` is. If you're unsure,
the rule is simply: depend on `cel-adapter-sdk`, implement `AdapterDriver`.

Until v1 is cut, treat the contract as "stable in shape, may move in name." Breaking changes will be called out in release notes.

## Minimum Viable Adapter

A conforming adapter must:

1. Declare a manifest (`adapter.json` or an inline `AdapterManifest` for native Rust).
2. Implement `probe()` so Cortex can tell whether the target app is reachable.
3. Implement `activate()` / `deactivate()` even if they are no-ops.
4. Implement `get_context()` returning `Vec<ContextElement>` tagged with `ContextSource::NativeApi`.

Execution (`execute(action, params) -> ActionResult`) is optional if your adapter is read-only, but almost every adapter will want at least one action.

The canonical trait, from `cel/cel-adapter-sdk/src/lib.rs`:

```rust
#[async_trait]
pub trait AdapterDriver: Send + Sync {
    fn manifest(&self) -> &AdapterManifest;
    async fn activate(&mut self) -> Result<(), AdapterError>;
    async fn deactivate(&mut self) -> Result<(), AdapterError>;
    async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError>;
    async fn execute(&self, action: &str, params: serde_json::Value)
        -> Result<ActionResult, AdapterError>;
    async fn probe(&self) -> bool;
}
```

For a worked example and the recommended skeleton, follow [building-adapters.md](./building-adapters.md).

## What's In-Contract

The following are stable commitments. If we change them, the adapter-common minor version bumps (or major, after v1).

- `AdapterDriver` trait method signatures — adding a method is breaking.
- `AdapterManifest` schema — adding a required field is breaking; adding an optional field with `#[serde(default)]` is not.
- `ActionResult` shape: `{ success: bool, error: Option<String>, data: Option<Value> }`.
- `AdapterError` enum variants — renaming or removing a variant is breaking.
- `ContextElement` and `ContextSource::NativeApi` — re-exported from `cel-context`. The element shape is versioned with cel-context; see [architecture.md](./architecture.md).
- Manifest discovery convention: `adapter.json` at the root of a process-runtime adapter directory.
- The `cel_act` `custom` action shape:
  ```json
  { "action": "custom", "adapter": "<name>", "action_name": "<action>", "params": { ... } }
  ```

## What's Out-of-Contract

Do not build on these; they can change at any time.

- Internal Cortex routing and tick scheduling (`RegisteredAdapter::should_read`, tick counters).
- Confidence-fusion heuristics. You declare a `confidence` in the manifest and on each `ContextElement`; how Cortex merges that with AX/CDP/vision is not part of the adapter contract.
- Which streams are active at any moment (vision may or may not be running alongside your adapter).
- In-process vs. process vs. WASM runtime selection — today native Rust adapters are compiled in, process adapters load via `adapter.json`, WASM is a future path.
- The exact shape of `MentalModel` or fused context returned by `cel_see`. Consumers read fused context, not adapter-internal buffers.
- The specific `LlmRole` and `cel-llm` behavior — see [bring-your-own-llm.md](./bring-your-own-llm.md). Adapters must never assume an LLM is available.

## Forward Compatibility Policy

| Change | Compatibility |
| --- | --- |
| Add a new method to `AdapterDriver` | Breaking (major bump) |
| Add a required manifest field | Breaking (major bump) |
| Add an optional manifest field (`#[serde(default)]`) | Non-breaking |
| Add a new `ContextSource` variant | Non-breaking for existing adapters |
| Add a new `AdapterError` variant | Non-breaking (with `#[non_exhaustive]`; TODO: confirm attribute added) |
| Change method signature | Breaking |
| Rename a manifest field | Breaking |
| Tighten `probe()` or `activate()` timing guarantees | Non-breaking if existing adapters still pass |

The cortex side declares a trait version internally. If an installed adapter was built against an incompatible major, Cortex must refuse to load it with a clear error. See [adapter-lifecycle.md](./adapter-lifecycle.md#version-compat-matrix) for the compat matrix.

## Manifest Schema

```json
{
  "name": "my-app",
  "display_name": "My App",
  "app_patterns": ["(?i)my app"],
  "platform": ["macos"],
  "runtime": "process",
  "entrypoint": "dist/driver.js",
  "context": {
    "element_types": ["record", "field"],
    "refresh_ms": 200,
    "confidence": 0.95
  },
  "actions": {
    "save": { "params": {}, "description": "Save the current record" }
  }
}
```

- `name`: unique snake-kebab identifier; must match `cel_act` `adapter` arg.
- `app_patterns`: regex patterns matched against the frontmost app name. `(?i)` prefix for case-insensitive.
- `platform`: `"macos" | "windows" | "linux"`.
- `runtime`: `"native" | "process" | "wasm"`. Defaults to `"process"`.
- `context.refresh_ms`: how often Cortex re-queries `get_context()`. Defaults to 200ms (one Cortex tick).
- `context.confidence`: base confidence hint; elements can override per-element. First-party convention: 0.95-0.98 for native-API elements.

## `ContextElement` for Adapter Authors

Required fields per element: `id`, `element_type`, `confidence`, `source: ContextSource::NativeApi`. Bounds are strongly recommended — without `bounds`, downstream click-by-reference actions can't target the element and agents fall back to vision.

Convention for adapter `id`s: `{adapter_name}:{stable_native_id}`. Example: `notes:title`, `excel:A1`, `figma:node:123:456`.

## Publishing Path

Today (pre-v1):

- **First-party adapters**: live in `adapters/` in the monorepo. Apache-2.0 licensed (workspace), with the common trait crate MIT-licensed to invite contributions.
- **Community / third-party adapters**: there is no crates.io distribution channel yet. The current path is:
  - Fork and add your crate under `adapters/your-name/` in a fork, or
  - Write a process adapter in any language (see [building-adapters.md](./building-adapters.md)) — it needs no Rust dependency at all, just the stdio protocol + an `adapter.json`, or
  - Publish your own Rust crate depending on `cel-adapter-sdk` once we ship a crates.io release (TODO: publish `cel-adapter-sdk` to crates.io; policy not yet set).
- **User-local adapters**: dev-mode loading via `adapter.json` at a known path (TODO: confirm the exact directory, currently scanned by `discover_adapters` in `cel_cortex::adapter`).

Open questions tracked here until resolved:

- TODO: when does `cel-adapter-sdk` go on crates.io, and under what version cadence?
- TODO: namespacing for third-party adapters — registry prefix (`community/`), manifest `author` field, or nothing?
- TODO: signing / attestation story — see [adapter-security.md](./adapter-security.md).

## How Adapters Are Invoked

From the agent side, everything goes through `cel_act`:

```json
{
  "action": "custom",
  "adapter": "my-app",
  "action_name": "save",
  "params": {}
}
```

Read paths go through `cel_see` — fused context already includes adapter-provided elements when the adapter is active.

## Also See

- [adapters-cel-agents.md](./adapters-cel-agents.md) — the three-layer north star.
- [building-adapters.md](./building-adapters.md) — how-to / tutorial.
- [adapter-security.md](./adapter-security.md) — trust, capabilities, and review policy.
- [adapter-lifecycle.md](./adapter-lifecycle.md) — loading, activation, and versioning.
- [adapter-catalog.md](./adapter-catalog.md) — official list of adapters and their maturity.
