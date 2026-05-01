# Adapter Roadmap

This doc answers "which adapter should we build next, and why?" It is a **proposal** — the ranking reflects my read of the platform bets implied by [adapters-cel-agents.md](./adapters-cel-agents.md) and the current shipping state in [adapter-catalog.md](./adapter-catalog.md). The user (product / maintainers) may re-rank. It should be treated as a living document.

The right set of adapters depends on the ICP. When `docs/gtm-icp.md` lands (TODO), link it here and let it override this ordering.

## Ranking Rubric

Each candidate is scored on:

1. **Validates the pattern** — does it prove the adapter contract works beyond `browser`?
2. **Platform pull** — does it make the agent-agnostic infrastructure story stronger?
3. **Customer demand** — is there a concrete workflow being asked for?
4. **Build cost** — Rust effort, native API complexity, macOS/Windows/Linux scope.
5. **Graduation path** — does it already exist in some form (e.g., AppleScript helpers) that we can package?

## Current State

- **Shipped**: `browser` (Production).
- **Stubs**: `excel`, `sap-gui`, `bloomberg`, `metatrader`.
- **In the core, should become an adapter**: Numbers (via `cel-input` AppleScript helpers).

## Proposed Order

### P0 — Numbers (graduate)

- **Status**: functionality exists in `cel-input` today (`read_numbers_cells` / `write_numbers_cells` in `cel/cel-input/src/applescript.rs`); needs to be repackaged as `adapters/numbers`.
- **Why now**: it's the lowest-cost way to prove the adapter pattern works for a second app. Success here unblocks everything below.
- **Estimated effort**: 1-2 weeks.
- **Deliverables**:
  - New `adapters/numbers` crate implementing `AdapterDriver`.
  - `adapter.json` matching `(?i)numbers`.
  - `execute()` supporting `write_cell`, `read_cell`, `write_cells`, `read_cells`.
  - Existing AppleScript callers in Cortex deprecated behind the adapter.
  - At least 2 eval scenarios (simple write, round-trip read).

### P1 — Excel (complete stubs)

- **Status**: COM interface designed, no real operations yet.
- **Why**: highest enterprise pull. Complements Numbers; proves the pattern on Windows COM as well as macOS AppleScript. Validates cross-platform adapter story.
- **Estimated effort**: 3-4 weeks for COM integration on Windows; longer if macOS Excel (AppleScript-based) is also in scope.
- **Deliverables**:
  - `excel` adapter moves from Stub → Beta.
  - Core actions: read_cell, write_cell, read_range, write_range, insert_row, delete_row, switch_sheet.
  - Windows COM path verified. macOS path either AppleScript or deferred.
  - ≥2 eval scenarios.

### P1 — Slack (new build)

- **Status**: not started.
- **Why**: Slack is the single highest-leverage workflow automation target for the "agent-agnostic infrastructure" narrative. Reading channels, posting messages, reacting to threads — all things customers will ask their agent to do.
- **Estimated effort**: 3-5 weeks. Needs a design doc first: Slack's scopes, OAuth, web vs desktop, how it fits alongside the browser adapter.
- **Deliverables**:
  - Design doc at `docs/adapters/slack-design.md` (TODO).
  - `adapters/slack` crate targeting Beta.
  - Actions: post_message, read_channel, search, react.
  - ≥2 eval scenarios covering read + write.

### P2 — Figma (design decision first)

- **Status**: not started.
- **Decision needed**: is this a dedicated adapter targeting Figma's REST API, a browser-fusion layer that enriches what we already see in the browser adapter, or both?
- **Why**: design-ops customers and strong demo material ("agent edits a frame in Figma"). But Figma's desktop app is essentially a browser wrapper — the browser adapter may already cover most of it with the addition of REST calls for deterministic node access.
- **Estimated effort**: 2-4 weeks after decision.
- **Deliverables**:
  - Decision doc.
  - Alpha adapter (if standalone) or design-ops example recipe (if browser-fusion).

### P2 — Cursor / VS Code

- **Status**: not started.
- **Why**: strongest "agent platform" story. Agents that can drive an IDE deterministically — open a file, edit a region, run a task — are the most visible demo of the three-layer model. Target for Cursor first; VS Code extension comes later.
- **Estimated effort**: 3-5 weeks for Cursor (Electron-based, use CDP + file I/O). VS Code via its extension API is a separate track.
- **Deliverables**:
  - Design decision: custom protocol vs browser-adapter-plus-CDP, given Cursor's Electron architecture.
  - Alpha adapter with open_file, edit_range, run_command.
  - One example recipe showing an agent driving Cursor.

### P3 — Docker Desktop

- **Status**: not started.
- **Why**: DevOps workflows (start container, inspect logs, manage volumes). Smaller user base but a very clean native-API surface via Docker's REST / CLI.
- **Estimated effort**: 2-3 weeks.
- **Deliverables**:
  - Design doc only at this stage.

### P3 — Notion, Google Sheets, Slides (likely browser-fusion)

- **Status**: not started.
- **Why**: high TAM but mostly web. The right answer is almost certainly "enhance the browser adapter with site-specific helpers" rather than building dedicated adapters. Documenting the decision here prevents someone re-opening the question later.
- **Estimated effort**: design doc.
- **Deliverables**:
  - One joint design doc covering the "when is it browser-fusion vs dedicated" question with these three as case studies.

### Deferred — Bloomberg, MetaTrader, SAP GUI

- Stay as Stubs in the catalog. They demonstrate the extensibility surface to enterprise prospects.
- Only promoted to Alpha+ when a specific customer commits to the integration.

## Per-Tier Acceptance Bar

To be promoted into its tier, an adapter must pass the corresponding bar. These reuse the maturity labels from [adapter-catalog.md](./adapter-catalog.md#maturity-labels).

| Tier | Requirement | Matches maturity |
| --- | --- | --- |
| **P0** | Production-ready. Covered by ≥2 evals. Docs in the adapter README. Deprecation path for any existing core code it replaces. | Production |
| **P1** | Beta. Core advertised actions work. ≥2 eval scenarios. Known-issues list in README. | Beta |
| **P2** | Alpha. Read and at least one write action working end-to-end. One example recipe. | Alpha |
| **P3** | Design doc only. No implementation required to be on the list. | Planned |

## What Drives Re-Ranking

Reasons to re-rank the list above:

- A specific enterprise customer commits to a vertical adapter (Bloomberg, SAP) — promote to P0/P1 for that customer.
- The ICP doc lands and disagrees with this ordering. The ICP doc wins.
- A community adapter ships something on this list, lowering our need to build it first-party.
- A platform partnership (e.g., official Slack integration) changes the build/buy calculus.

## Questions to Resolve

- TODO: land `docs/gtm-icp.md` and reconcile this ordering.
- TODO: decide Figma dedicated-vs-browser-fusion before investing build time.
- TODO: decide Cursor's protocol strategy — custom vs CDP-through-browser-adapter.
- TODO: publish the design doc for Slack before starting implementation.
- TODO: confirm that the stubs (`bloomberg`, `metatrader`, `sap-gui`) stay in-tree as surface demonstrations or move to a `community/` tier once we open submissions.

## Also See

- [adapters-cel-agents.md](./adapters-cel-agents.md) — the three-layer north star.
- [building-adapters.md](./building-adapters.md) — how-to / tutorial.
- [adapter-sdk.md](./adapter-sdk.md) — the stable contract each of these adapters must meet.
- [adapter-catalog.md](./adapter-catalog.md) — the authoritative list these land into.
- [adapter-lifecycle.md](./adapter-lifecycle.md) — the graduation/deprecation mechanics referenced above.
- [adapter-security.md](./adapter-security.md) — review bar for first-party promotions.
