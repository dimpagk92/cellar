# Adapter Lifecycle

This doc describes how an adapter moves from "a crate on disk" to "actively feeding context into Cortex" and back, how Cortex decides which adapter is relevant for the frontmost app, how versioning between adapters and Cortex is enforced, and how adapters are deprecated and removed.

Adapters are Layer 1 in the three-layer model (see [adapters-cel-agents.md](./adapters-cel-agents.md)). Cortex owns the dispatch; adapters just declare and respond.

## Loading

### Today: Monorepo Registration

First-party adapters are wired into the Cortex at build time. The hookup site is `cel/cel-cortex/src/cortex.rs::Cortex::register_adapter`, which takes a `Box<dyn AdapterDriver>` and wraps it in a `RegisteredAdapter` with its compiled app-pattern regex set.

Native-Rust first-party adapters:

- Linked directly as workspace crates (`adapters/excel`, `adapters/sap-gui`, etc. — see `Cargo.toml` workspace members).
- Registered by NAPI glue for Node-side consumers: `cel/cel-napi/src/adapter_registry.rs` exposes `registerAdapter(name)` which maps name → concrete driver via `create_adapter()`.

Process-runtime adapters (`runtime: "process"` in `adapter.json`):

- The `browser` adapter is the canonical example. Its `adapter.json` points at `dist/process-driver.js`.
- Cortex's `discover_adapters(base_dir)` scans `base_dir/*/adapter.json` and returns `(path, manifest)` pairs. A process-runtime shim then launches the entrypoint and speaks JSON-lines over stdio to fulfil the `AdapterDriver` trait.

### Future: Dynamic Registry

Goals (TODO):

- A standard user-local adapter directory (`~/.cellar/adapters/<name>/adapter.json` — path TBD).
- A manifest registry file listing enabled adapters and their install paths.
- Hot reload in dev mode so adapter authors don't have to restart the whole Cortex to test.

None of this is live yet. For now, treat adapter installation as either "in-tree" or "dev mode via a custom `discover_adapters` base dir."

## App Matching

When Cortex observes a change in the frontmost app, it picks the adapter whose `app_patterns` regex set matches:

```rust
pub fn matches_app(&self, app_name: &str) -> bool {
    self.compiled_patterns.iter().any(|re| re.is_match(app_name))
}
```

Matching rules:

- Patterns are case-sensitive by default. Use `(?i)` for case-insensitive — every real adapter uses this.
- The first adapter that matches wins for context reads. Conflict resolution is covered below.
- Patterns are compiled once at `RegisteredAdapter::new`. Invalid patterns are silently dropped — adapter authors should test their regex.

App name sources (TODO: confirm the exact field Cortex uses — likely bundle ID on macOS, window class on Linux, process name on Windows). Adapter authors should write patterns against the display name the OS surfaces for that app.

## Lifecycle States

```
Registered → Probed → Activated → Running → Deactivated → Unloaded
```

| State | How it's entered | What Cortex does |
| --- | --- | --- |
| **Registered** | `register_adapter()` called before boot | Compiled patterns, `AdapterState::Inactive`. Not yet talking to the app. |
| **Probed** | Cortex periodically calls `probe()` on inactive adapters whose patterns match the frontmost app | `probe()` returns `true` when the target app is reachable via its native API. |
| **Activated** | `activate()` returned `Ok` | `AdapterState::Active`, `get_context()` starts being called at `refresh_ms` cadence. |
| **Running** | Steady state | Cortex merges the adapter's elements into fused context each tick. |
| **Deactivated** | App loses focus, Cortex shuts down, or `activate()` → error | `deactivate()` is called. State goes back to `Inactive` or to `Error`. |
| **Unloaded** | Process exit, dev-mode hot reload | Drop semantics; the driver's `Drop` impl runs. |

Mapped to the `AdapterState` enum in `cel/cel-cortex/src/adapter.rs`:

```rust
pub enum AdapterState { Inactive, Active, Error }
```

`Error` is sticky: an adapter that fails `activate()` stays in `Error` until explicitly reset (TODO: confirm reset path — today we rely on process restart).

## Refresh Cadence

Each adapter declares `context.refresh_ms` in its manifest (default 200ms, matching the Cortex tick). `RegisteredAdapter::should_read` enforces this:

```rust
pub fn should_read(&self, tick_ms: u64) -> bool {
    let refresh_ms = self.driver.manifest().context.refresh_ms;
    let elapsed = self.ticks_since_last_read * tick_ms;
    elapsed >= refresh_ms
}
```

Adapter authors: if your underlying API is slow (e.g., AppleScript calls that take 50ms), bump `refresh_ms` to something like 500ms so you don't starve the Cortex loop.

## Hot Reload

Not supported today. Changing an adapter requires restarting the Cortex (and, for Node bindings, restarting the host process that owns the NAPI module).

Future dev-mode hot reload (TODO) would look like: watch `adapter.json` and the adapter's compiled artifact, on change call `deactivate()`, drop the driver, re-register, and let normal probing reactivate.

## Version Compat Matrix

Each major version of `adapter-common` corresponds to a range of `cel-cortex` versions it supports. The trait surface is the binding contract — if a method is added to `AdapterDriver`, existing adapters built against the older trait will not compile against the new cortex, and loading them at runtime (for process adapters, via manifest) must fail with a clear error.

| adapter-common | cel-cortex | Status |
| --- | --- | --- |
| 0.1.x | 0.1.x | Current (pre-v1). No cross-compat guarantees. |
| 0.2.x (TODO) | TBD | First minor-version bump after unifying `Adapter` and `AdapterDriver`. |
| 1.0.0 | 1.0.0 | Frozen trait surface. See [adapter-sdk.md](./adapter-sdk.md#status-and-versioning). |

Enforcement:

- Native Rust adapters: compile-time. Linker errors if the trait version is incompatible.
- Process adapters: manifest declares adapter-common version it targets (TODO: add `adapter_api_version` field); Cortex refuses to load if incompatible.
- WASM adapters (future): module imports adapter-common via a pinned ABI; Cortex validates on load.

## Conflict Handling

If two registered adapters have patterns that match the same frontmost app:

- **Precedence rules** (proposal — TODO: confirm with user):
  1. First-party > community > user-local. A first-party Slack adapter beats a community one.
  2. Within the same tier, manifest `priority` integer wins (TODO: add to schema).
  3. Within the same tier and priority, registration order (first registered wins).
- The losing adapter is not activated. Its elements are not merged.
- Cortex should log the conflict at `warn` so the user can see which adapter lost. (TODO: confirm logging exists.)

For many cases (e.g., a generic "browser" adapter plus a site-specific overlay), the right answer is not precedence but composition — letting both contribute to fused context with different `element_types`. That's a future extension.

## Deprecation Policy

When a first-party adapter is being retired (e.g., replaced by a better implementation):

1. **Announce** one minor version ahead in the adapter's README and in the top-level `CHANGELOG`.
2. **Log** a deprecation warning at Cortex boot whenever the deprecated adapter is registered.
3. **Continue to ship** for at least one minor version before removal.
4. **Document migration** — if there's a replacement adapter, point to it; if the capability is being folded into CEL core, say so.

Third-party adapters are out of scope for this policy — maintainers set their own.

## The Numbers Case (Concrete Example)

Numbers (Apple's spreadsheet) is currently served by AppleScript helpers inside `cel-input` / Cortex (`read_numbers_cells` / `write_numbers_cells` in `cel/cel-input/src/applescript.rs`). It does not yet exist as a separate adapter.

The planned graduation path — covered in [adapter-roadmap.md](./adapter-roadmap.md) — is:

1. Extract the AppleScript layer into a first-party `adapters/numbers` crate.
2. Give it a manifest matching `(?i)numbers`.
3. Map the existing `write_cells` / `read_cells` actions into its `execute()` surface under the `custom` action type.
4. Cortex starts preferring the adapter when it's Active; Cortex-side AppleScript usage becomes a fallback.
5. After one minor version with both paths live, the Cortex-side helpers are marked deprecated and eventually removed.

This is the reference workflow for every "capability currently in the core, should become an adapter" graduation.

## Also See

- [adapters-cel-agents.md](./adapters-cel-agents.md) — the three-layer north star.
- [building-adapters.md](./building-adapters.md) — how-to / tutorial.
- [adapter-sdk.md](./adapter-sdk.md) — the public contract and versioning policy.
- [adapter-security.md](./adapter-security.md) — trust model and review requirements.
- [adapter-catalog.md](./adapter-catalog.md) — maturity labels referenced by the lifecycle states.
