# Adapter Security

Third-party adapters are arbitrary Rust (or, via `runtime: "process"`, arbitrary code in any language) with full access to an application's native API on the user's machine. This doc defines the trust model, what adapters are and are not allowed to do, and how we plan to accept submissions without opening a hole through CEL into the user's desktop.

Without this doc, CEL cannot open community adapter contributions. It is intentionally conservative until sandboxing lands.

## Trust Model

There are three tiers. Every installed adapter fits in exactly one.

### 1. First-Party (reviewed, in-tree)

- Lives under `adapters/` in the monorepo.
- License: Apache-2.0 for adapter crates, MIT for `adapter-common`.
- Changes ship via the normal repo review process: code review by a maintainer, CI green, no surprise telemetry, no hidden network calls.
- Runs in-process with full capabilities.

Current first-party adapters: `browser`, `excel`, `bloomberg`, `metatrader`, `sap-gui`, and the in-tree `adapter-common` crate. See [adapter-catalog.md](./adapter-catalog.md).

### 2. Community (signed, process-isolated by default)

- Distributed outside the monorepo, or inside a clearly marked `community/` subtree.
- Must use `runtime: "process"` or (later) `runtime: "wasm"`. No in-process native Rust adapters from community sources until the signing story is in place.
- Must ship a threat-model statement in the adapter README (what APIs it touches, what data it reads, what it writes).
- TODO: signing policy — whether we require signed manifests and who holds signing keys is not decided.

### 3. User-Local / Dev Mode

- Loaded from a user-controlled path (typically `~/.cellar/adapters/` — TODO: confirm directory).
- No restrictions. Explicit informed consent: if you put it there, you trust it.
- Should surface a clear warning at Cortex boot: "Loaded dev-mode adapter X from ~/.cellar/adapters — unreviewed code."

## What Adapters Can Reach

An adapter runs with the same capabilities as its host runtime. Practically:

- Any OS API the adapter crate imports (AppleScript, COM, D-Bus, file I/O, network).
- Any application API its target app exposes (spreadsheet cells, browser DOM, IDE files).
- CEL-supplied inputs: action name and params from `cel_act`, plus whatever it reads from the target app.

An adapter is effectively a CEL-shaped RPC into whatever capabilities its process has. This is why the trust model exists.

## What Adapters MUST NOT Do

This is the community-adapter acceptance bar. Violations are grounds for rejection or removal.

- **Exfiltrate context data.** No network calls that ship fused context, screenshots, or app contents outside the adapter's declared target API.
- **Spawn shells without declaration.** If an adapter shells out (e.g., runs `osascript`, `powershell`, `sh`), the README must document every command and every user-supplied variable flowing into it. No dynamic shell composition from untrusted input.
- **Bypass CEL flags.** If a user disables network or vision at the CEL layer, an adapter must not re-enable equivalent capabilities through a side channel.
- **Phone home for telemetry.** No analytics, crash reporting, or update checks without explicit opt-in documented in the manifest.
- **Mutate unrelated state.** An Excel adapter does not get to touch the filesystem outside the target workbook's directory. A Slack adapter does not get to read the user's keychain.
- **Embed credentials.** API keys belong in user-supplied config, not in the adapter binary or repo.

## First-Party Review Requirements

For a PR adding or modifying a first-party adapter:

1. **Code review** by a repo maintainer. Line-by-line read of anything touching `execute()` or shelling out.
2. **Threat model** in the adapter README:
   - What APIs does this adapter touch?
   - What data is read? What data is written?
   - What happens if the target app is untrusted (e.g., malicious spreadsheet formulas)?
   - What happens on partial failure (half-applied writes)?
3. **No surprise dependencies.** New third-party crates need justification.
4. **Tests.** `probe()` path, `get_context()` with a stub target, at least one `execute()` success and failure case.
5. **No hidden network.** Network usage documented in the manifest (TODO: add `network` capability declaration field to manifest schema).
6. **No hidden telemetry.** No analytics or crash reporting without an opt-in flag and docs.
7. **Confidence claims justified.** If an adapter declares 0.98 confidence on its elements, reviewers should confirm the underlying API actually guarantees that level of accuracy.

## Sandboxing Roadmap

Today, native Rust adapters run in-process. Long term, we want defense in depth. Proposed ordering:

- **Today**: trust via review. First-party only for in-process.
- **Near term (TODO: date)**: `runtime: "process"` adapters run as child processes, communicating over stdio JSON. Crashes are contained. This already exists as a runtime option.
- **Medium term**: capability-based permissions declared in the manifest — `capabilities: ["applescript", "com", "network:api.slack.com"]`. Cortex refuses to activate an adapter that requests capabilities the user has not approved.
- **Long term**: WASM runtime (`runtime: "wasm"`) with wasmtime. Memory-safe, sandboxed, deterministic. Target for the community tier default.

None of this is implemented yet beyond the process runtime hook. Treat the current state as "reviewed in-tree, or you're on your own."

## Responsible Disclosure

If you find a security issue in CEL or a first-party adapter:

- Email: **dimpagk92@gmail.com** (see [SECURITY.md](../SECURITY.md))
- Do not open a public issue first.
- We aim for a 90-day coordinated disclosure window: acknowledgement within 72 hours, fix and disclosure within 90 days.
- For third-party adapters, contact the adapter maintainer first; CEL maintainers can help coordinate if the issue is systemic.

## User-Facing Kill Switch

Users should be able to shut off adapters globally, independent of agent or CEL configuration:

- **Proposed**: `CEL_DISABLE_ADAPTERS=1` env var. When set, Cortex skips adapter discovery and never activates any adapter, regardless of what agents request.
- TODO: verify whether this env var is already wired. If not, adding it is a prerequisite for accepting community adapters. Other `CEL_DISABLE_*` flags exist today (e.g., `CEL_DISABLE_AUDIO` in `cel/cel-napi/src/cortex.rs`), so the precedent is there.

Per-adapter disable (e.g., `CEL_DISABLE_ADAPTER_SLACK=1`) is a nice-to-have but not required for v1. TODO.

## Audit Log

Adapter actions should be auditable after the fact. Proposed:

- Every `execute()` call logged at `info` with `{adapter, action, params_hash, result.success, duration_ms}`.
- Sensitive params (credentials, long text blobs) are redacted or hashed, not stored verbatim.
- Log destination follows the existing CEL tracing setup (stdout + configurable file sink).
- TODO: confirm existing tracing in Cortex already captures this, or add it.

## Open Questions

- TODO: crates.io distribution — do we require published community adapters to be signed?
- TODO: sandboxing MVP date and acceptance criteria.
- TODO: manifest `capabilities` field — shape and enforcement semantics.
- TODO: `CEL_DISABLE_ADAPTERS` env var — exists or needs adding?
- TODO: audit log format and retention policy.

## Also See

- [adapters-cel-agents.md](./adapters-cel-agents.md) — the three-layer north star.
- [building-adapters.md](./building-adapters.md) — how-to / tutorial.
- [adapter-sdk.md](./adapter-sdk.md) — the public trait and manifest contract.
- [adapter-lifecycle.md](./adapter-lifecycle.md) — loading, activation, deprecation.
- [adapter-catalog.md](./adapter-catalog.md) — official list, licensing, maturity.
