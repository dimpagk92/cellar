# Adapter Catalog

The official list of adapters known to CEL, with maturity, maintainer, and license. This is the answer to "what can CEL plug into today, and how much can I trust each of those paths?"

Adapters are Layer 1 (see [adapters-cel-agents.md](./adapters-cel-agents.md)). Maturity labels are defined at the bottom.

## Maturity Labels

- **Stub** — trait skeleton and manifest exist; no real operations implemented.
- **Alpha** — the basic read/write loop works end-to-end on a dev machine. Expect rough edges, missing actions, no eval coverage.
- **Beta** — most advertised actions work. Known bugs are documented. At least one eval scenario passes.
- **Production** — stable surface. Covered by multiple evals. Versioned releases. Used by a real workflow in-house or in a pilot.
- **Planned** — on the roadmap, not yet started. See [adapter-roadmap.md](./adapter-roadmap.md).

Promotions between tiers require the acceptance bar listed in the roadmap doc.

## First-Party Adapters

| Name | Scope | Status | Maintainer | License | Docs | Since |
| --- | --- | --- | --- | --- | --- | --- |
| `browser` | Chrome, Chromium, Brave, Arc, Edge, Firefox, Safari via CDP/Playwright | **Production** | CEL core | Apache-2.0 | [adapters/browser/adapter.json](../adapters/browser/adapter.json), [browser-context-fusion.md](./browser-context-fusion.md) | v0.1.0 |
| `excel` | Microsoft Excel via COM | **Stub** (interface designed) | CEL core | Apache-2.0 | [adapters/excel](../adapters/excel) | v0.1.0 |
| `sap-gui` | SAP GUI for Windows via SAP Scripting API | **Stub** | CEL core | Apache-2.0 | [adapters/sap-gui](../adapters/sap-gui) | v0.1.0 |
| `bloomberg` | Bloomberg Terminal | **Stub** | CEL core | Apache-2.0 | [adapters/bloomberg](../adapters/bloomberg) | v0.1.0 |
| `metatrader` | MetaTrader 5 via MQL5 / Manager API | **Stub** | CEL core | Apache-2.0 | [adapters/metatrader](../adapters/metatrader) | v0.1.0 |
| `numbers` | Apple Numbers via AppleScript | **Beta** (in-tree at `cel-cortex/src/native_adapters.rs`, registered via NAPI; awaiting graduation to standalone `adapters/numbers/` crate) | CEL core | Apache-2.0 | `cel/cel-cortex/src/native_adapters.rs` | unreleased |

`adapter-common`, the shared trait crate, is MIT-licensed to invite contributions.

Notes:

- `browser` is the only Production-tier adapter today. It is the reference for what a stable adapter looks like and how to structure a process-runtime driver.
- `numbers` is in **Beta** — `NumbersAdapter` lives in `cel-cortex/src/native_adapters.rs`, implements the `AdapterDriver` trait, and is registered into the runtime in `cel-napi/src/cortex.rs`. Pending: extract into a standalone `adapters/numbers/` crate so it follows the same packaging convention as `browser`. Tracked in [adapter-roadmap.md](./adapter-roadmap.md) P0.
- The four stub adapters (`excel`, `sap-gui`, `bloomberg`, `metatrader`) are intentional — they shape the surface by showing the trait applies to COM, scripting APIs, and trading terminals. They are not expected to ship to Beta without a dedicated customer commitment. See [adapter-roadmap.md](./adapter-roadmap.md) for the "deferred unless demand" note.

## Community Adapters

_None yet._

When the first community adapter lands, it goes here with the same columns plus a **Maintainer** link pointing to the external repo or owner. Community adapters must pass the bar in [adapter-security.md](./adapter-security.md#first-party-review-requirements) even if they're hosted externally — CEL only vouches for what's in this table.

## Planned

Rows below are tracked as future work. The ordering is intentional and is explained in [adapter-roadmap.md](./adapter-roadmap.md). These are **proposals** — inclusion here does not commit the project to building them in order.

| Name | Scope | Rationale | Design status |
| --- | --- | --- | --- |
| `numbers` (extract) | Apple Numbers | Beta lives in `cel-cortex` — graduate to a standalone `adapters/numbers/` crate so it follows `browser`'s packaging. | Implementation done; packaging pending. |
| `slack` | Slack desktop + web | High-leverage workflow automation. Needs Slack API adapter design doc first. | Not started. |
| `figma` | Figma desktop | Design-ops demos, strong visual story. Open question: is this a dedicated adapter or a browser-fusion layer? | Decision pending. |
| `cursor` / `vscode` | IDE automation | Strong agent-platform narrative — "Cursor + CEL" lets agents drive an editor deterministically. | Not started. |
| `docker-desktop` | Docker Desktop | DevOps workflows. Uses Docker's REST / CLI. | Not started. |
| `slides` / `powerpoint` | Presentation apps | High TAM, may be better as browser-fusion for Slides and COM for PowerPoint. | Not started. |
| `notion` | Notion desktop + web | Mostly web — likely browser-fusion over dedicated adapter. | Decision pending. |
| `google-sheets` | Google Sheets via web | Web-based, almost certainly browser-fusion rather than a dedicated adapter. | Decision pending. |

## Deferred

Adapters we are intentionally not prioritizing. They stay in the catalog as stubs because they demonstrate the extensibility surface to enterprise customers.

- `bloomberg`, `metatrader`, `sap-gui`. Niche verticals with strong per-customer pull but small TAM. Moved from planned → deferred pending a specific customer commitment.

## Contribution Path

Want to add your adapter?

1. Read [adapters-cel-agents.md](./adapters-cel-agents.md) for the three-layer context.
2. Read [adapter-sdk.md](./adapter-sdk.md) for the stable contract and forward-compat policy.
3. Read [adapter-security.md](./adapter-security.md) for the trust model — especially before submitting as first-party.
4. Read [building-adapters.md](./building-adapters.md) for the tutorial and code skeleton.
5. Decide tier:
   - **First-party**: PR into `adapters/` under Apache-2.0. Requires review.
   - **Community**: fork or external crate consuming `adapter-common`. Process-runtime or WASM recommended.
   - **User-local / dev**: drop into your own `adapter.json` directory, no submission needed.
6. Land with a manifest, probe, get_context, and at least one execute action.

When you're ready to be listed here, open a PR adding a row to the appropriate table.

## How This Catalog Is Maintained

- Updates happen alongside the code change that introduces or promotes an adapter.
- Maturity label changes require the bar in [adapter-roadmap.md](./adapter-roadmap.md) to be met.
- Removed or deprecated adapters move to a "Deprecated" section (to be added when first deprecation happens) rather than being deleted — users need to see what stopped working.

## Also See

- [adapters-cel-agents.md](./adapters-cel-agents.md) — the three-layer north star.
- [building-adapters.md](./building-adapters.md) — how-to / tutorial.
- [adapter-sdk.md](./adapter-sdk.md) — public contract.
- [adapter-security.md](./adapter-security.md) — trust model and review bar.
- [adapter-lifecycle.md](./adapter-lifecycle.md) — loading, activation, deprecation flow.
- [adapter-roadmap.md](./adapter-roadmap.md) — what's next and why.
