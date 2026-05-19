# CEL Adapters

Adapters give CEL app-specific perception and execution that bypass the GUI-driving fragility of generic AX / coordinate clicks. They speak the app's native API (AppleScript, COM, SQLite, REST, …) and surface results through the `AdapterDriver` trait.

## Two runtimes

Every adapter declares one of two runtimes in its `adapter.json`:

### `runtime: "native"` — linked into the host

The adapter is a Rust crate compiled into `cel-napi` (and other consumers). Registration is **manual** in each consumer's cortex boot:

```rust
// cel/cel-napi/src/cortex.rs
cortex.register_adapter(Box::new(adapter_browser::BrowserAdapter::new()));
```

Pros: zero IPC overhead, shared state with the cortex (e.g. CDP client). Cons: requires editing each consumer when added, can't be dropped in by users.

**Use for:** adapters that need to share runtime state with the cortex (browser-rs holds the same CDP client, numbers shares the document_model surface).

### `runtime: "process"` — discovered at runtime ← **preferred for new adapters**

The adapter is a standalone executable that speaks JSON over stdin/stdout. The cortex discovers it via `cel_cortex::discover_adapters` (a recursive scan of `adapters/*/adapter.json` and `~/.cellar/adapters/*/adapter.json`).

**To add a new productivity adapter:** drop a folder with `adapter.json` + an executable. **No code edits to `cel-napi`, `cel-eval`, or anywhere else.**

This is the path every CEL consumer (the MCP server, the eval harness, future CLIs) already supports — see `cel/cel-napi/src/cortex.rs` for the discovery loop. Adding `discover_adapters` to a new consumer is one paste.

## Reference: the four AppleScript / SQLite adapters

The mail / calendar / reminders / messages adapters live in this directory as ProcessDriver adapters. They share the same shape:

```
adapters/mail/
├── Cargo.toml          # library + [[bin]] target
├── adapter.json        # ProcessDriver manifest (runtime: process)
├── src/
│   ├── lib.rs          # AdapterDriver impl — the actual adapter code
│   └── main.rs         # 4-line wrapper: run_stdio_loop(MailAdapter::new())
```

The `lib.rs` is identical to what it would be for an in-process adapter — the only difference is the small `main.rs` and the `runtime: "process"` line in `adapter.json`. Every adapter's `main.rs` is the same shape:

```rust
use cel_adapter_runtime::run_stdio_loop;
use adapter_mail::MailAdapter;

fn main() {
    run_stdio_loop(MailAdapter::new());
}
```

The `cel-adapter-runtime` crate handles the stdio JSON-RPC protocol (`activate`, `deactivate`, `get_context`, `snapshot`, `execute`, `verify_action`, `bootstrap`) so individual adapters never write that code.

## Building the binaries

```bash
make build-adapters
# or, per adapter:
cargo build --release -p adapter-mail
```

The `entrypoint` field in `adapter.json` points at `../../target/release/adapter-<name>` relative to the adapter folder, so a `cargo build` in the workspace root puts the binary exactly where the cortex looks.

## Adding a new adapter (the full workflow)

You want a Salesforce adapter. Here's what you do:

1. **Pick a language.** Rust gets you `cel-adapter-runtime` and type-safe `AdapterDriver`. Python / Node / Go work too — you just have to implement the JSON-line protocol yourself (see [`cel/cel-cortex/src/process_driver.rs`](../cel/cel-cortex/src/process_driver.rs) for the spec).

2. **Create `adapters/salesforce/`** with `adapter.json` + your adapter code.

3. **For Rust:** add it as a workspace member in the root `Cargo.toml`, mirror the shape of `adapters/mail/Cargo.toml` (lib + `[[bin]]`).

4. **For non-Rust:** put the executable next to `adapter.json` and reference it via `entrypoint`. The cortex auto-detects `.py` / `.ts` / `.js` and invokes `python3` / `node`; anything else is treated as a binary.

5. **Build (if Rust).** `cargo build --release -p adapter-salesforce`. Add to `make build-adapters` if you want it in the bundle.

6. **Done.** Restart the MCP server / eval / app. The cortex picks it up.

## Why ProcessDriver over native registration

| | Native | ProcessDriver |
|---|---|---|
| Drop-in for users | ❌ | ✅ |
| IPC overhead | 0 | ~1–10 ms / call |
| Language | Rust only | Any |
| Crash isolation | None | Process boundary |
| Shared cortex state | ✅ | ❌ |

For AppleScript-heavy adapters the dominant cost is `osascript` (100s of ms), so the IPC overhead is rounding error. For SQLite-fast adapters like `messages`, IPC is the same order as the actual work but still well under the cortex's tick budget.

## Existing adapters

| Adapter | Runtime | Notes |
|---|---|---|
| `browser-rs` | native | Shares CDP client with cortex; needs runtime state |
| `browser` | process (TS) | Playwright peer of `browser-rs` |
| `numbers` | native | Shares document_model surface |
| `notes` | native | Predates ProcessDriver migration; could move later |
| `mail` | **process** | AppleScript |
| `calendar` | **process** | AppleScript |
| `reminders` | **process** | AppleScript |
| `messages` | **process** | SQLite read-only |
| `excel`, `sap-gui`, `bloomberg`, `metatrader` | native (stubs) | TBD |

When existing native adapters need rewrites, prefer migrating them to ProcessDriver unless they truly need shared cortex state.
