# API and Adapter Stability

This doc answers: what parts of CEL do we commit not to break, and on what schedule? A stable contract is the precondition for third-party adapters, external agents, and any production user trusting the runtime.

## Scope

### Covered by the stability commitment

These surfaces are governed by the versioning rules below. Downstream consumers can rely on them across versions within the commitment.

- **MCP tool signatures** — `cel_see`, `cel_act`, `cel_perceive`, `cel_think`. Argument schemas, result shapes, error codes. See [mcp-server.md](mcp-server.md) and [api-reference.md](api-reference.md).
- **AdapterDriver trait** — the Rust trait in `adapter-common` that third-party adapters implement. See [building-adapters.md](building-adapters.md).
- **ContextElement schema** — the canonical fused-context type returned by perception. Field names, types, nullability.
- **ActionResult shape** — return type of every action. Success/failure, timing, anomaly flags.
- **Action receipt shape** — `cel_act` receipt fields such as `id`, `status`, `dispatch_path`, `requires_verification`, `verification`, `evidence`, and timestamps. Additive fields are allowed; removing or renaming existing fields is breaking.
- **Environment variable names** — `CEL_*` env vars documented as user-facing (`CEL_RUNTIME_BACKEND`, `CEL_RUNTIME_URL`, `CEL_RUNTIME_TOKEN`, etc.). Renaming one is a breaking change.
- **`cellar` CLI flag names** — documented flags on the `cellar` binary. Positional argument order.
- **Node SDK `@cellar/agent` public API** — exported types and functions. npm-semver applies.

### Not covered

These are explicitly **not** promised. Consumers who depend on them do so at their own risk.

- **Internal crate APIs** — anything in `cel/cel-*` that isn't re-exported through `adapter-common` or the MCP surface. Types may change any release.
- **Built-in planner behavior** — the reference planner (`cel-goal-runner`, related runners) can change strategy without warning. The *interface* to drive it is stable; the *behavior* is not.
- **Cortex tick timing** — we reserve the right to change perception cadence, buffering, and event ordering as long as the final `ContextElement` stream remains valid.
- **Performance characteristics** — latency, memory usage, throughput. We optimize these; we don't commit to specific numbers.
- **Wire-level details between crates** — internal protobuf schemas, internal RPC formats, process layouts. Anything not exposed through the public surfaces above.
- **Eval suite contents** — task lists and scoring can change. Methodology changes are announced ([eval-leaderboard.md](eval-leaderboard.md)).

## Versioning Policy

Each public surface has its own version lineage. They move independently.

| Surface                        | Versioning scheme           | Today    | Source             |
|--------------------------------|-----------------------------|----------|--------------------|
| MCP wire format                | `cel-mcp-vX.Y.Z`            | v0.2.0   | `mcp-server/`      |
| AdapterDriver trait            | `adapter-common` crate semver | 0.x.y  | `adapters/adapter-common/` |
| Node SDK `@cellar/agent`       | npm semver                  | 0.x.y    | `agent/`           |
| `cellar` CLI flags            | `cellar` crate semver      | 0.x.y    | `cli/`             |
| Cellar repo overall            | aggregate semver (git tag)  | 0.x.y    | `CHANGELOG.md`     |

All five are pre-1.0 today. See "What triggers a 1.0" below.

## Breaking-Change Policy

- **Deprecation first.** Any breaking change to a covered surface ships with a deprecation warning at least **one minor release** before removal. The warning appears in logs, in type-system deprecation annotations where the language supports them, and in `CHANGELOG.md`.
- **Major-version gate.** Breaking changes only land on a **major version bump** (post-1.0). Pre-1.0, we reserve the right to break on minor versions, but we still announce, deprecate, and migrate — see "Pre-1.0 Rules" below.
- **Security exception.** A security-critical fix that requires a breaking change can ship on any release. We document the break, publish the CVE if applicable ([security-review-plan.md](security-review-plan.md)), and provide a migration note.

## Support Window

**TODO: user decision** — confirm or adjust the window below.

Proposed: the **latest minor of N-1 major** is supported for **6 months** after N ships.

Example: when 2.0 ships, the last 1.x minor receives backports of security fixes for 6 months. After that, 1.x is end-of-life.

Tradeoffs:

- **Longer window (12 months)**: friendlier for enterprise, more maintenance cost per release.
- **Shorter window (3 months)**: less maintenance, pushes users to upgrade faster, harder for enterprise procurement.
- **6 months**: common industry default; matches how most infrastructure OSS projects behave.

## Pre-1.0 Rules

Every covered surface is pre-1.0. That lets us break things when we need to — but there's a discipline:

- **Every break is documented.** `CHANGELOG.md` is authoritative. Entries include "what changed," "why," and "how to migrate."
- **Every break has a migration guide.** For anything non-trivial, a doc under `docs/migrations/vX.Y-to-vX.Y.md` walks through the change.
- **Renames get both names for one minor.** If we rename `CEL_FOO_BAR` to `CEL_FOO_BAZ`, both work for one minor release; one emits a deprecation warning.
- **Telemetry confirms the migration worked.** Usage of deprecated symbols is tracked (opt-in telemetry) and we don't remove anything until usage drops below a threshold. **TODO: user decision** — do we have opt-in telemetry to back this up? If not, the policy falls back to "we warn, we wait one minor, we remove."

## What Triggers a 1.0

Each surface hits 1.0 independently when it meets its own criteria.

### MCP tools → 1.0

- External-agent cookbook gallery is complete — Claude Code, Cursor, Mastra, LangGraph each have a published integration recipe.
- The MCP tool argument/result/receipt schemas have been unchanged for **2 months**.
- **≥3 production users** depend on the MCP surface (named, listed, consenting).

### AdapterDriver trait → 1.0

- **2 non-browser first-party adapters** are in production (e.g., Numbers + Excel, or Slack + Excel).
- **At least one external adapter contribution** has been received and is compatible with the current trait.
- Trait has been unchanged for 1 month.

### Node SDK `@cellar/agent` → 1.0

- Used in at least one external project.
- Exported API stable for 2 months.

### `cellar` CLI → 1.0

- Flag surface stable for 2 months.
- Integration-tested across all supported install paths (Homebrew, direct binary, container).

### Cellar repo overall → 1.0

- All four surfaces above have hit 1.0.
- `SECURITY.md` threat model is complete ([security-review-plan.md](security-review-plan.md) Phase A done).
- Public eval leaderboard has been running stably for at least 3 months ([eval-leaderboard.md](eval-leaderboard.md)).

## Release Cadence

**TODO: user decision** — proposal below, not yet committed.

- **Monthly minor releases.** Regular rhythm, predictable for consumers.
- **Patch releases as needed.** Bug fixes and security issues can ship any time.
- **Major releases driven by feature readiness**, not calendar. Probably 1–2 per year post-1.0.

Tradeoffs:

- **Faster cadence (weekly)**: more signal for enterprise, more release engineering overhead.
- **Slower cadence (quarterly)**: less overhead, harder for downstream consumers to plan.
- **Monthly**: matches most infrastructure OSS projects.

## Communication

When a release ships:

- **`CHANGELOG.md`** — authoritative. Every change that touches a covered surface is listed.
- **GitHub Release** — auto-generated summary + links to detailed notes.
- **Migration guide** — for any non-trivial change, under `docs/migrations/`.
- **Deprecation inventory** — `docs/deprecations.md` lists every deprecated symbol, when it was deprecated, and when it will be removed. **TODO**: this file does not exist yet; create it when the first deprecation lands.

## Deprecation Inventory (template)

When `docs/deprecations.md` is created, use this shape:

| Symbol                 | Deprecated in | Replacement          | Removed in (planned) |
|------------------------|---------------|----------------------|----------------------|
| `CEL_OLD_VAR`          | 0.3.0         | `CEL_NEW_VAR`        | 0.5.0                |
| `@cellar/agent.oldFn()`| 0.3.0         | `newFn()`            | 0.5.0                |

Keep the list alphabetized by symbol for easy scanning.

## Open Questions — TODO: user decision

| Question                                                       | Needed by                                 |
|----------------------------------------------------------------|-------------------------------------------|
| Support window: 3 / 6 / 12 months?                             | Before first 1.0 surface ships.           |
| Release cadence: weekly / monthly / quarterly?                  | Before the next minor release.            |
| Do we have opt-in telemetry for deprecation usage tracking?     | Before any post-1.0 removals.             |
| Who signs off on breaking changes (one person / review panel)?  | Before first post-1.0 major.              |
| Do we publish a formal RFC process, or keep it GitHub-issues-driven? | Before the first external-author RFC. |

## Related Reading

- [api-reference.md](api-reference.md) — the current state of the covered surfaces.
- [mcp-server.md](mcp-server.md) — MCP tool signatures.
- [building-adapters.md](building-adapters.md) — what adapter authors depend on.
- [adapters-cel-agents.md](adapters-cel-agents.md) — why stable contracts matter for the three-layer architecture.
- [security-review-plan.md](security-review-plan.md) — security-driven breaking changes.
- [README.md](../README.md)
