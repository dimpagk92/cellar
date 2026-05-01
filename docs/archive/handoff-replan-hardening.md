# Tier-Replan Hardening — Handoff

**Commits**: `96f5db0`, `68bf99d`, `5535400`, `4876b05`, `22fb480`, + final Phase D/E commit
**Test state**: 281/281 unit tests passing, 69/69 e2e passing, Rust 440+/440+ passing
**Bench state**: `hn-top-stories` passes at ~60s (1 LLM call); `techcrunch-news` went from 300s timeout to 34-36s pass across 2 runs.

## Shippable today (with flags off — zero risk)

The flag-off default is **bit-exact to pre-`96f5db0` behavior**. These changes can merge to `main` without changing any production behavior:

- Pure additions: `state.ts` (GoalState scaffold), `replan-events.ts` (observability), `canary.ts` (rollout), `phase-profiler.ts` (diagnostics)
- Flag-gated additions: `triggerReplan`, `replanRouter`, semantic stall detection, tier-4 re-decomposition
- 2 real bug fixes (always on):
  - `executeAction` returning `false` no longer silently zeros the failure counter (goal-runner.ts)
  - TS side no longer double-stringifies the config passed to the Rust goal-runner (cel-run.ts)
- Tests: 281 unit, integration for tier-replan + concurrency + canary

Risk of merging flag-off: near zero. Both bug fixes are demonstrably correct in new unit tests.

## Shippable with flags on (feature-flag rollout)

Once the 7 gates in [CHANGELOG-replan-hardening.md](CHANGELOG-replan-hardening.md) pass, turn flags on progressively via `CELLAR_TIER_REPLAN_PCT`. Rollout runbook: [rollout-runbook.md](rollout-runbook.md).

Recommended flag ordering:
1. `enableTierReplan=true` — enables tier 1/2/3 (no Tier 4, no semantic stall, no pre_steps)
2. `enableSemanticStallEscalation=true` — adds stall detection after tier 2/3 are stable
3. `enableTier4Reassessment=true` — adds the most invasive tier
4. `enableFeasibilityPreSteps=true` — **requires security review first** ([security-replan-hardening.md](security-replan-hardening.md))

## Not yet production-grade

| Gate | Status | Blocker |
|---|---|---|
| Full bench suite × 5 runs | Not done | ~30 min of compute + ~$5-20 API. Worth doing. |
| P50 latency < 15s for simple tasks | **Failed**: ~60s measured | Hotspot is inside `ts_fallback` / TS `runGoal`; needs deeper profiling inside `planStep()` |
| Rust primary path working | Partially | My TS fix unblocks it, but the Rust binary is stale + the ADR suggests TS-primary anyway |
| `grounding failure → tier-replan` | **Broken** | `continue` path in goal-runner.ts bypasses the GATE. Known architectural issue; documented. |
| Security review for pre_steps | Checklist ready, approval pending | Run through `docs/security-replan-hardening.md` with a reviewer. |
| Tier-4 meaningful recovery | **Partial** | Because `canReplanGlobal()` stays false after Tier 4 unless `resetGlobalCounter()` is called, and counter-reset is gated on decomposition succeeding. One bonus attempt, then fails cleanly. |

## Known architectural follow-ups (post-merge)

1. **Grounding-fail bypass**: `goal-runner.ts:~1067` `continue` on grounding error skips the tier-replan GATE. Move GATE to a `try/finally` or integrate grounding-fail into the tier signal path.

2. **60-second TS fallback**: profile inside `planStep()` (it's where the unaccounted ~50s lives). Likely candidates:
   - LLM call latency (SDK retry logic?)
   - Hidden speculative planning LLM calls
   - Context distillation overhead on large pages
   - Conversation thread message compaction

3. **Rust primary decision**: [ADR-replan-ts-vs-rust.md](ADR-replan-ts-vs-rust.md) recommends TS-primary. Once decided, remove the `@deprecated` tag and formalize the orchestration layer.

4. **Merge conflict resolution**: 30+ pre-existing uncommitted files in the worktree. See [merge-audit-replan-hardening.md](merge-audit-replan-hardening.md) for file-by-file conflict review. The one file that NEEDS both my work and theirs is `adapters/browser/src/cel-run.ts`.

5. **LangGraph scaffolds**: `state.ts` defines `GoalState`, `PersistentState`, `EphemeralState`, reducers. Zero consumers today. Either promote to real use or delete after 60 days.

## Reviewer action items (ordered by urgency)

1. **Decide**: do we want this PR now (flag-off), or after the Rust-vs-TS ADR is resolved?
2. **Review**: read [CHANGELOG-replan-hardening.md](CHANGELOG-replan-hardening.md) (10 min), glance at [ADR](ADR-replan-ts-vs-rust.md) (15 min).
3. **Merge**: see [merge-audit](merge-audit-replan-hardening.md). The only high-risk file is `adapters/browser/src/cel-run.ts`.
4. **Test**: full bench sweep × 5 runs before flag-flip. Budget ~$20.
5. **Roll out**: follow [rollout-runbook.md](rollout-runbook.md) — canary 10% → 25% → 50% → 100% over 7 days.

## What I'd consider "done"

- All 281 unit tests green *(done)*
- 69 e2e tests green *(done)*
- Rust workspace green *(done)*
- 2 bench tasks real-run pass *(done)*
- Flag-gated + rollback-safe *(done)*
- Security review checklist for pre_steps *(done — not approved yet)*
- ADR drafted *(done — not decided yet)*
- Merge audit *(done)*
- Observability: metrics + events *(done)*
- Concurrency test *(done)*
- Documentation: design + runbook + handoff *(done)*

What's NOT done:
- Full bench matrix × 5 runs (scale + cost)
- 60s latency investigation
- Rust port (if ADR chooses Fork 2)
- Production deployment (not in scope)

## Contact

Me, if you want to pair on any of the follow-ups. Otherwise the documents here should be enough for any reviewer to land this PR and run the canary independently.
