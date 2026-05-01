# ADR: TS vs Rust as the primary goal-runner path

> Historical note: the current repo direction is [adapters-cel-agents.md](./adapters-cel-agents.md). Planning is now treated as pluggable, so this ADR is no longer the architectural source of truth.

**Status**: **Decided — Rust-primary (Fork 2)**
**Decided**: 2026-04-19
**Decided by**: dimpagk (project owner)
**Rationale**: The rest of the planning stack is already Rust (cel-cortex, cel-accessibility, cel-planner, cel-llm, cel-goal-runner). Keeping the orchestration in a second language (TS) creates a permanent two-sided maintenance burden. The TS goal-runner was a bootstrap; the long-term direction is one runtime.

**Date**: 2026-04-17 (drafted), 2026-04-19 (decided)
**Context**: Commits `96f5db0`, `68bf99d`, `5535400`, `4876b05` hardened the tier-replan system in the TS goal-runner. Every benchmark run today takes the TS path because of a pre-existing bug on the Rust side.

## The current state

Two implementations of the goal-runner exist:

1. **TS** — [agent/src/goal-runner.ts](../agent/src/goal-runner.ts), marked `@deprecated Use the Rust goal-runner (cel-goal-runner) via NAPI for new work.`
2. **Rust** — [cel/cel-goal-runner](../cel/cel-goal-runner), ~2,200 lines, actively refactored (30+ uncommitted files touching it in the current branch).

Production calls go through `runGoalWithRustFallback` in [adapters/browser/src/cel-run.ts](../adapters/browser/src/cel-run.ts) which tries Rust first and falls back to TS on error.

**Until this PR, every benchmark call failed the Rust try** due to a double-JSON-stringify bug in the TS bindings (`cel.runGoalRust` stringifies its argument, but `cel-run.ts` was also pre-stringifying). The Rust parser saw a string-of-a-string and rejected it. The fallback to TS hid the issue.

With the bug fixed (commit in progress), the Rust path can be exercised again. This makes the TS-vs-Rust decision urgent.

## Forks

### Fork 1 — TS-primary
Remove the `@deprecated` tag. Delete the Rust runner (or mark it abandoned). All improvements — tier-replan, state scaffolds, semantic stall, pre-flight consolidation, metrics — stay where they are and continue to land.

**Pros**:
- Work done in this PR keeps delivering value.
- Single orchestration path is easier to reason about.
- TS has the richer abstractions (typed `GoalState`, `CognitiveTrail` event envelope, tier-replan).
- Fast iteration — no Rust compile cycles.

**Cons**:
- Slower than Rust for CPU-bound work (but the hot path is I/O / LLM latency, not CPU).
- Rust Cortex integration is already wired; TS path uses the TS `Cortex` class.
- Someone already invested significant work in the Rust runner refactor.

### Fork 2 — Rust-primary
Port the TS tier-replan work to Rust. Files to replicate:
- `triggerReplan` / `replanRouter` / `getReplanTier` → `cel-goal-runner/src/failure_recovery.rs` (new)
- `StrategyTracker.resetGlobalCounter` → existing `strategy_tracker.rs`
- Semantic-stall logic → `runner.rs` GATE phase (new)
- Pre-flight reorder + pre_step execution → `runner.rs` (new)
- Tier 4 cap + milestone re-decomposition → `runner.rs` (new)
- Feature flags → `config.rs` (extend GoalConfig)
- Cognitive-trail `ns` + subscribe → `cognitive_trail.rs`
- `GoalState` scaffold → already exists as `state.rs` in Rust

**Pros**:
- Single runtime, no fallback complexity.
- Matches the existing refactor direction.
- Deterministic performance profile.

**Cons**:
- **Weeks of work.** Every TS change needs a Rust port with matching semantics.
- **Test matrix doubles.** Each tier test needs a Rust equivalent.
- During transition, TS path must stay alive as fallback (otherwise bugs in Rust crash prod).
- Lose the LangGraph scaffolds (`state.ts`, reducers, `TrailEvent`) unless re-implemented in Rust.

### Fork 3 — Shim, don't dup
Keep the TS goal-runner as the orchestration layer. Have the Rust `cel-goal-runner` become a *thin wrapper* that calls into the TS runner via N-API or a child process. Rust Cortex continues to own perception; Rust Cortex dispatches to TS runner for planning/loop control.

**Pros**:
- Avoids forking effort.
- Rust benefits (Cortex, adapter system) kept; TS benefits (rapid iteration, rich abstractions) kept.

**Cons**:
- Architecture smells — inversion of the stated direction.
- IPC / FFI overhead on the hot path.
- Unclear who owns what.

## Decision criteria

| Criterion | TS-primary | Rust-primary | Shim |
|---|---|---|---|
| Perf (latency on hot path) | ≈ (mostly I/O-bound) | ✓ slight edge | ✗ FFI cost |
| Dev velocity | ✓ | ✗ | ≈ |
| Rollback safety | ✓ (flag-gated) | ✗ needs Rust rebuild | ≈ |
| Matches stated roadmap | ✗ contradicts deprecation | ✓ | ≈ |
| Effort to land | 1-2 days | 2-3 weeks | 1 week |
| Rust Cortex integration | ✗ redundant TS Cortex | ✓ native | ✓ native |
| Bench pass-rate | unchanged | risk during port | mostly unchanged |
| Test-suite complexity | 1 suite | 2 suites + integration | hybrid (messy) |

## Recommendation (superseded by decision above)

**~~Fork 1 — TS-primary~~** was recommended in the initial draft for reasons of
migration cost and battle-testing. The project owner chose Fork 2 — Rust-primary
— on 2026-04-19 for architectural coherence (the rest of the planning/perception
stack is Rust).

### Original Fork 1 reasoning (preserved for audit)

1. The Rust path has been broken in prod since the double-stringify bug. Real traffic has been on TS for months.
2. The tier-replan system this session built has 5 integration-test scenarios and unit coverage across 11 files. Porting all of that to Rust is a multi-week endeavor with high risk.
3. The stated "Rust-primary" architectural direction was a goal, not a realized state. Reversing course then was cheap; after 3 more months of Rust investment it won't be.
4. The one thing Rust does uniquely well — Cortex perception — can stay in Rust and be consumed by the TS runner (via the existing `CortexProxy` pattern in `config.ts`).

## Required for Fork 2 (now the chosen path)

See [docs/rust-port-plan.md](rust-port-plan.md) for the detailed port plan.
Summary:

- [ ] Port plan: estimate task-by-task — **done, see rust-port-plan.md**
- [ ] Parity-test suite: identical input → identical output across TS and Rust runners
- [ ] Feature-flag Rust path independently from tier-replan flags
- [ ] Extended fallback window: keep TS alive for 6+ months post-cutover
- [ ] Keep `CortexProxy` as the TS/Rust seam for any remaining TS code

## Resolved questions

1. **Who owns the Rust refactor?** — dimpagk, the project owner
2. **Cortex integration** — stays Rust-native; `CortexProxy` remains available for legacy TS callers during transition
3. **Timeline pressure** — no hard deadline; port proceeds while TS fallback keeps working
