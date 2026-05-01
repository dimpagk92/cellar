# Merge Audit — Tier-Replan Hardening vs. In-Flight Rust Refactor

**Date**: 2026-04-17
**My commits landed in this session**: `96f5db0`, `68bf99d`, `5535400`, `4876b05` (+ pending profile instrumentation)

## Scope

When this session started, the worktree had 30+ uncommitted files from a parallel in-flight refactor (primarily Rust-side). My commits land on top. This document catalogs the overlap so a reviewer can merge confidently.

## File categories

### Category 1 — Mine only (no conflict expected)

| File | What I changed |
|---|---|
| `agent/src/goal-runner.ts` | Tier-replan orchestration, semantic stall, feature flags, profile hooks |
| `agent/src/goal-runner/cognitive-trail.ts` | `ns` field + `TrailEvent` + `subscribe()` |
| `agent/src/goal-runner/failure-recovery.ts` | `triggerReplan`, `replanRouter`, proactive-signal tier floor |
| `agent/src/goal-runner/strategy-tracker.ts` | `resetGlobalCounter()` |
| `agent/src/goal-runner/state.ts` | NEW — typed `GoalState` scaffold |
| `agent/src/goal-runner/config.ts` | Feature flags added |
| `agent/src/goal-runner/planner.ts` | Screenshot buffer reuse, static import |
| `agent/src/goal-runner/index.ts` | Re-exports updated |
| `agent/src/goal-runner/phase-profiler.ts` | NEW — opt-in timing profiler |
| `agent/src/types.ts` | 4 metric fields added |
| `agent/src/cognitive-trail.test.ts` | +4 tests |
| `agent/src/failure-recovery.test.ts` | NEW |
| `agent/src/state-reducers.test.ts` | NEW |
| `agent/src/integration/tier-replan.integration.test.ts` | NEW |
| `docs/CHANGELOG-replan-hardening.md` | NEW |
| `docs/security-replan-hardening.md` | NEW |
| `docs/ADR-replan-ts-vs-rust.md` | NEW |
| `docs/merge-audit-replan-hardening.md` | NEW (this file) |

### Category 2 — Overlap with pre-existing work

| File | My change | Pre-existing change | Conflict risk |
|---|---|---|---|
| `adapters/browser/src/cel-run.ts` | Fixed double-stringify bug; added profile hooks | Modified (unknown scope — not mine) | **High** — both touch `runGoalWithRustFallback` |
| `benchmarks/src/runners/cellar.ts` | Added profile hooks in `runTask()` | Modified (not mine) | **Medium** — same function modified |
| `agent/src/config.ts` | *No change by me* | Pre-existing: reads `~/.cellar/config.toml` | None (I did not touch) |

### Category 3 — Pre-existing only (not mine, included for reference)

Roughly 25 files I did not modify:
- `Cargo.lock`, `Cargo.toml`, root-level config
- `cel/cel-accessibility/**` (4 files) — a11y refactor
- `cel/cel-context/**` (7 files) — context merge/resolve work
- `cel/cel-cortex/**` (6 files) — cortex refactor
- `cel/cel-goal-runner/**` (6 files) — **the Rust runner refactor referenced in ADR**
- `cel/cel-llm/**` (5 files) — LLM provider rework
- `cel/cel-napi/src/goal_runner.rs` — **Rust-side `parse_config` fallback added here pre-existing (explains ADR Fork 1 reasoning)**
- `cel/cel-planner/**` (2 files)
- `cli/src/**` (2 files) — MCP command rework
- `docs/**` (3 files) — api-reference + quickstart updates
- `registry/src/index.ts`

## Conflicts requiring attention

### C1 — `adapters/browser/src/cel-run.ts` (HIGH)

I edited this file in **both** my work (profile hooks + double-stringify fix) and the pre-existing diff touches it too. The diff I produced is staged as an uncommitted change. Reviewer should:

1. Before rebasing, pull both diffs side by side: `git diff HEAD~4 -- adapters/browser/src/cel-run.ts` (mine) and `git stash show -p` (theirs, if stashed).
2. My double-stringify fix (switching `cel.runGoalRust(JSON.stringify(...))` → `cel.runGoalRust({...})`) is **essential** — keep it regardless of which side wins the surrounding diff.
3. My profile hooks (`CELLAR_PROFILE=1` guarded) are non-essential — drop them if they conflict with the pre-existing changes.

### C2 — `benchmarks/src/runners/cellar.ts` (MEDIUM)

Only ~4 lines added by me (profile hooks). Low semantic risk. Keep if clean, drop if ugly.

### C3 — None in Rust

I explicitly did not touch Rust. Any Rust changes in the worktree are pre-existing and orthogonal.

## Rebase order recommendation

1. Commit the pre-existing work first (check with the author what they intended).
2. Rebase my 4 session commits on top.
3. Resolve `cel-run.ts` by preserving the double-stringify fix + any pre-existing logic changes, discarding profile hooks if needed.
4. Run full test suite: `cd agent && npx vitest run` — expect 264 passing.
5. Run Rust suite: `cargo test --workspace --lib` — expect 440+ passing.
6. Run e2e: `cd e2e && npx playwright test --project=agent-engine --project=adversarial --project=context-pipeline` — expect 69 passing.
7. Smoke bench: `BENCH_LLM_MODEL=claude-haiku-4-5-20251001 npm run bench:cellar -- --task hn-top-stories --runs 1`

## What the pre-existing Rust work appears to include

Based on file names (I did not read these diffs in depth):

- **`cel-goal-runner/src/runner.rs`** — the Rust goal-runner orchestration. The ADR concludes TS should remain primary, so this work may be either:
  - Archived with a note
  - Kept as a skeleton for future Rust port (no urgency)
  - Partially merged for its Cortex integration (good parts)

- **`cel-napi/src/goal_runner.rs`** — the `parse_config` double-stringify fallback is in this diff. **This is redundant with my TS-side fix** but harmless. If both land, the Rust fallback is belt-and-suspenders.

- **`cel-context`, `cel-accessibility`, `cel-cortex`** — perception-layer improvements. Orthogonal to my work. Should merge cleanly.

- **`cel-llm`** — provider rework. Touches how LLM calls happen. My code calls `cel.llmComplete` and `cel.planStep`; if the pre-existing work renames or restructures these, the TS side might need corresponding updates. **Verify at merge time.**

## Required reviewer action

1. **Own the decision on `adapters/browser/src/cel-run.ts`**: double-stringify fix MUST land; profile hooks are optional.
2. **Answer the ADR's three open questions** (docs/ADR-replan-ts-vs-rust.md).
3. **Confirm the pre-existing `cel-llm` rework doesn't rename `llmComplete`** or my pre-flight LLM calls break.
4. **Run the full smoke test** in the order above before pushing.
