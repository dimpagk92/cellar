# ADR: Unify Browser Ownership Under CEL

**Status:** Implementing on `unify-browser-ownership` branch (2026-05-19)
**Author:** dimpagk92 (with Claude assistance)
**Supersedes:** —
**Superseded by:** —

## Problem

CEL has two parallel browser-launching paths, owned by two different layers, that don't share a Chromium instance.

| | `BrowserAdapter` path | `ensureDedicatedCdpBrowser` path |
|---|---|---|
| File | `adapters/browser/src/index.ts` | `agent/src/cdp-browser.ts` |
| Launcher | Playwright internal | `child_process.spawn(browser.binaryPath, ...)` |
| Chromium source | Playwright bundled | User-installed Chrome / Chromium / Brave / Edge / Arc |
| Visibility | Headless | Visible window |
| Lifecycle owner | Adapter | Cortex (transitively) |
| Used by | Direct `celRun(adapter)` calls | MCP `run_goal` path |

Symptoms of the split:
1. **Parallel browsers.** Code that uses both paths (e.g. a hypothetical runner that calls `celRun` and also `bootCortex`) launches two Chromiums per session.
2. **Focus stealing.** When `bootCortex()` is called without `ensureDedicatedCdpBrowser()`, Cortex defaults to whatever browser is frontmost (Safari, user's main Chrome). Observed during 2026-05-19 benchmark debugging.
3. **Inconsistent dependencies.** The `BrowserAdapter` path works on any machine; the `ensureDedicatedCdpBrowser` path silently requires the user to have a Chromium-family browser installed.
4. **Inconsistent headedness.** Direct path is headless by default; MCP path is visible by default. Benchmarks running in batch see Chrome windows pop up.

Per the existing `CLAUDE.md` positioning: *"CEL should own context fusion, stream normalization, execution, and adapter routing."* The browser is execution infrastructure. It belongs to CEL, not to one of N adapters.

## Goal

One CEL-owned browser primitive. Both the `BrowserAdapter` and the existing `ensureDedicatedCdpBrowser` consume it. No parallel browsers. No focus stealing. Bundled Chromium so it works without a system browser install. Headless by default, visible opt-in.

## Non-goals

- Not changing the planner / agent loop / strategy router.
- Not changing benchmark task definitions.
- Not eliminating `ensureDedicatedCdpBrowser`'s public API (callers stay compatible).
- Not adding support for non-Chromium browsers (Safari, Firefox) — out of scope.

## Proposed API

A single primitive on the `Cel` class:

```typescript
// agent/src/cel.ts (or wherever Cel is defined)

export interface EnsureBrowserOptions {
  /** Default true. Pass false for agent-assistant use cases that want a visible window. */
  headless?: boolean;
  /** Default { width: 1280, height: 800 } */
  viewport?: { width: number; height: number };
  /** Default true */
  stealth?: boolean;
  /** Default auto. If specified, binds to this port. */
  port?: number;
  /**
   * Default a temp dir under `~/.cellar/cdp-profiles/`. Pass for persistent profiles
   * (e.g. agent-assistant flows that want to remember login state).
   */
  profileDir?: string;
  /** Initial URL. Default "about:blank". */
  initialUrl?: string;
}

export interface BrowserHandle {
  /** ws://localhost:PORT/devtools/browser/UUID — clients attach to this. */
  cdpUrl: string;
  /** The CDP port the browser is listening on. */
  port: number;
  /** True if the launched browser is currently reachable. */
  isAlive(): Promise<boolean>;
  /** Tear down the browser. Idempotent. */
  close(): Promise<void>;
}

class Cel {
  // ...existing methods...

  /**
   * Ensure a Chromium browser is running and return a handle.
   *
   * Idempotent: if a browser was already launched with compatible options
   * (same headedness, same viewport, etc.), returns the existing handle.
   * If a browser was launched with INCOMPATIBLE options (e.g. caller wants
   * headless: false but existing is headless: true), throws — callers must
   * close the existing browser first.
   *
   * Uses Playwright's bundled Chromium binary, independent of any
   * system-installed browser.
   */
  async ensureBrowser(options?: EnsureBrowserOptions): Promise<BrowserHandle>;
}
```

### Why this shape

- **`Cel` is the right owner.** Per CLAUDE.md ("CEL should own context fusion, stream normalization, execution, and adapter routing"), the browser primitive belongs on the `Cel` class itself, not the adapter or the cortex.
- **Idempotent.** Multiple callers (BrowserAdapter, Cortex, future direct cdp clients) call `ensureBrowser()` and get the same handle. No parallel browsers.
- **Bundled Chromium.** Removes the "needs Chrome installed" requirement of the current `ensureDedicatedCdpBrowser`. Works on any Mac, Linux, Windows that runs Playwright.
- **CDP URL return.** Clients attach via standard CDP. Both Playwright clients and raw CDP clients can use it. Agnostic.
- **Headless default.** Right for benchmarks, CI, batch use. Agent-assistant flows opt into `headless: false`.

## Implementation strategy

### Phase 1: Add `cel.ensureBrowser()` primitive (no migrations)

**Files touched:**
- `agent/src/cel.ts` (add method)
- `agent/src/browser-primitive.ts` (new file — internal implementation)
- `agent/test/browser-primitive.test.ts` (new file — unit tests)
- `agent/package.json` (ensure playwright dep is in `dependencies` not `devDependencies`)

**Implementation outline (browser-primitive.ts):**

```typescript
import { chromium, type Browser } from "playwright";
import path from "node:path";
import os from "node:os";
import { mkdirSync } from "node:fs";

let singletonBrowser: Browser | null = null;
let singletonOptions: EnsureBrowserOptions | null = null;
let singletonCdpUrl: string | null = null;

export async function ensureBrowserInternal(
  options: EnsureBrowserOptions = {}
): Promise<BrowserHandle> {
  const effective = {
    headless: options.headless ?? true,
    viewport: options.viewport ?? { width: 1280, height: 800 },
    stealth: options.stealth ?? true,
    initialUrl: options.initialUrl ?? "about:blank",
    ...options,
  };

  // If an existing browser is alive, reuse it (only if options are compatible)
  if (singletonBrowser && singletonBrowser.isConnected()) {
    if (!areCompatible(singletonOptions, effective)) {
      throw new Error(
        "ensureBrowser called with incompatible options; close existing browser first"
      );
    }
    return makeHandle(singletonBrowser, singletonCdpUrl!);
  }

  // Otherwise launch fresh
  const userDataDir = options.profileDir ??
    path.join(os.homedir(), ".cellar", "cdp-profiles", `temp-${Date.now()}`);
  mkdirSync(userDataDir, { recursive: true });

  const context = await chromium.launchPersistentContext(userDataDir, {
    headless: effective.headless,
    viewport: effective.viewport,
    args: [
      // Stealth flags if requested
      ...(effective.stealth ? STEALTH_ARGS : []),
    ],
  });
  const browser = context.browser();
  if (!browser) throw new Error("Failed to obtain browser from context");

  // Get the CDP endpoint. Playwright exposes this via wsEndpoint.
  // For launchPersistentContext we need a different path — use the
  // browser instance's connection details.
  const cdpUrl = browser.wsEndpoint?.() ?? await derivecdpUrl(browser);

  singletonBrowser = browser;
  singletonOptions = effective;
  singletonCdpUrl = cdpUrl;

  return makeHandle(browser, cdpUrl);
}
```

(The exact Playwright API for getting a wsEndpoint on a launched browser is tricky — may need to use `chromium.launch()` with explicit `--remote-debugging-port` instead of `launchPersistentContext`. Will resolve during implementation.)

**Phase 1 success criteria:**
- `cel.ensureBrowser()` returns a handle
- `handle.cdpUrl` is reachable (can curl `/json/version`)
- `handle.close()` shuts down
- Second call to `ensureBrowser()` returns the same handle (idempotent)
- **Hybrid suite still passes at 100%** (`pnpm bench:hybrid:h2h` or equivalent) — this primitive isn't called by anything yet, so this should be a no-op verification

### Phase 2: Migrate `BrowserAdapter`

**Files touched:**
- `adapters/browser/src/index.ts` (rewire `connect()`)
- `adapters/browser/src/cdp-client.ts` (accept external CDP URL)

**Change:**
```typescript
// adapters/browser/src/index.ts
async connect() {
  // OLD: this.cdpClient = new CdpClient({ headless: ..., useCdp: ... });
  //      await this.cdpClient.connect();

  // NEW:
  const handle = await this.cel.ensureBrowser({
    headless: this.opts.headless,
    viewport: this.opts.viewport,
    stealth: this.opts.stealth,
  });
  this.cdpClient = new CdpClient({ cdpUrl: handle.cdpUrl });
  await this.cdpClient.connect();
}
```

**Phase 2 success criteria:**
- `BrowserAdapter` no longer launches its own Chromium
- Hybrid suite still passes at 100%
- 5-task WebVoyager smoke runs without crashing (success rate doesn't matter — just verifying the path works)

### Phase 3: Migrate `ensureDedicatedCdpBrowser`

**Files touched:**
- `agent/src/cdp-browser.ts` (rewrite `ensureDedicatedCdpBrowser` to delegate to `cel.ensureBrowser`)

**Change:** Same public API, internal implementation now calls `cel.ensureBrowser({ headless: false })` (preserves visible-browser default for MCP `agent-assistant` use cases). Removes the `chooseChromiumBrowser()` system-Chrome-finding logic.

**Phase 3 success criteria:**
- `cellar-mcp.ts` hybrid suite still passes at 100%
- MCP server flows from external consumers (Claude Code, OpenClaw, etc.) still work
- No regression in the MCP `run_goal` path

### Phase 4: Cleanup

**Files touched:**
- Delete `chooseChromiumBrowser()` and related (now unused)
- Remove duplicate Playwright dep config if any
- Update inline docs / module headers
- Update README / `docs/architecture.md` to reflect single browser primitive

**Phase 4 success criteria:**
- Hybrid suite still 100%
- Clean TypeScript build with no warnings
- No dead code grep'able for the removed APIs

## Validation strategy

At every phase, the hybrid suite at 100% is the gate. Specifically:

```bash
cd /Users/dimitriospagkratis/cellar/cellar/benchmarks
pnpm bench:hybrid               # CEL on hybrid suite
pnpm bench:cellar-mcp:hybrid    # CEL via MCP on hybrid suite (Phase 3 onward)
```

Both must show 5/5 PASS. If either drops below 100%, the phase is broken and gets reverted before proceeding.

Additionally, after Phase 2 a 5-task WebVoyager smoke confirms the standard runners still work:

```bash
pnpm bench:webvoyager -- --limit 5
```

Pass rate doesn't need to be high (the planner is what determines that, not the browser). Just needs to complete without errors.

## Risks

| Risk | Mitigation |
|---|---|
| Playwright's CDP URL extraction is fiddly for `launchPersistentContext` | Try `chromium.launch()` with explicit `--remote-debugging-port` if the persistent-context path doesn't expose wsEndpoint cleanly. Document in Phase 1 implementation. |
| Singleton browser state causes test interference (one test's browser leaks into another) | Add `cel.closeBrowser()` for explicit teardown. Wire benchmark `printSummary()` / cleanup to call it. |
| `ensureDedicatedCdpBrowser` callers expect visible Chrome | Phase 3 keeps `headless: false` default for that path, so callers see no behavior change. |
| Hybrid suite regression mid-migration | Each phase has explicit gate; we revert and investigate before continuing. Branch is feature-isolated. |
| Bundled Chromium too old for some test sites | Pin to current Playwright version (1.59.x per `adapters/browser/package.json`). Already verified chromium-1217 was downloaded earlier today. |

## Open questions

1. **Profile lifecycle.** Singleton browser uses one profile dir for its lifetime. Benchmarks want a fresh profile per run for isolation. Options: (a) `cel.ensureBrowser()` always uses fresh profile + benchmark calls `cel.closeBrowser()` between runs; (b) add `cel.resetBrowserProfile()`; (c) accept that benchmark runs reuse profile state. **Tentative:** (a). Decide during Phase 1.

2. **Multi-display / multi-browser.** Future: should `ensureBrowser` accept a `tag` parameter so callers can request "the dedicated browser for adapter X" vs "the dedicated browser for assistant mode"? **Out of scope for this ADR**, but the API shape should not preclude.

3. **Headless mode for MCP "agent assistant" use cases.** Phase 3 keeps `headless: false` as the default for `ensureDedicatedCdpBrowser`. Should we expose a config flag (CLI / env) to flip this for power users who want headless MCP? **Yes, but Phase 4 follow-up — don't block Phase 3 on it.**

## Decision log

- **2026-05-19** — User authorized the ~6-9 day investment on `unify-browser-ownership` branch. Hybrid suite at 100% as the gate at each phase. Design doc + implementation in the same flow.

## Addendum: CDP client ownership on the Rust side (Cortex) — 2026-06-02

The body of this ADR concerns the **TypeScript** browser-*launching* paths
(`cel.ensureBrowser`, `BrowserAdapter`, `ensureDedicatedCdpBrowser`). This
addendum covers the symmetric question one layer down, on the **Rust** side:
who owns the `CdpClient` *connection* inside `cel-cortex`, and how it is bound.
This was item #4 of the June-2026 architecture pass; it was deferred while
`cortex.rs` was being decomposed into submodules (commit `1ba48bb`,
*"decompose cortex.rs and planning_view.rs into submodules"*) and is applied on
top of that split.

### Problem (Rust side)

CDP-client state lived in **three** independent slots, written by **five**
different paths, with no single authority:

| Slot | Written by | Read by |
|---|---|---|
| `Cortex::cdp_client` | `with_cdp_client`, `set_cdp_client`, `bind_browser_cdp_url`, tick-loop auto-bind, per-action fallbacks | `execute()` dispatch, `cdp_eval`/navigate/screenshot, `url_changed` bridge |
| `BrowserAdapter::cdp_client` (browser-rs) | pushed via `set_cdp_client`, **plus its own `probe()` ambient discovery** | DOM perception |
| TS `BrowserAdapter` (separate process) | its own `connect()` | its own perception |

Only `bind_browser_cdp_url` propagated a client from the cortex to the adapters.
The other writers (`with_cdp_client`, the tick-loop auto-bind, and ~6 per-action
`connect_to_focused_app()` fallbacks) set the cortex slot alone — and each
per-action fallback opened its own throwaway connection. In ambient mode the
cortex and the browser-rs adapter discovered clients via two *separate*
`connect_to_focused_app()` calls with no synchronization, so "which client is
bound?" had no single answer.

### Decision: Cortex-internal consolidation

Make the **Cortex the single runtime owner** of the CDP client, with exactly one
runtime writer that always propagates to adapters. Scope deliberately kept
inside `cel-cortex` — the lower-risk option, chosen because the CDP subsystem
was being actively redesigned in a parallel stream.

**Invariant:** at runtime there is exactly one mutator —
`install_cdp_client(slot, adapters, client)` in the parent `cortex` module —
which sets `Cortex::cdp_client` *and* pushes the same client to every registered
adapter. Everything funnels through it:

- `Cortex::bind_cdp_client(client)` — thin method wrapper (new).
- `bind_browser_cdp_url(url)` — resolve page-level URL → connect → `bind_cdp_client`.
- The tick-loop ambient auto-bind (still **gated on `daemon_bridge`**) now calls
  `install_cdp_client` instead of writing the slot directly, so the app's
  in-process browser adapter perceives the cortex's exact client.
- `cdp_client_or_ambient()` (new) — returns the bound client, or does **one**
  ambient `connect_to_focused_app()` discovery and binds it (propagating) for
  reuse. The six per-action fallbacks — `cdp_navigate_page`, `cdp_page_content`,
  `cdp_eval_via_shared_or_focused`, the `execute()` dom path, the
  `extract_with_fallback` loop, and `dispatch_navigate` — now route through this
  single helper instead of each opening a throwaway connection.

The sync builders `with_cdp_client` / `set_cdp_client` stay slot-only **by
design**: they run at construction time (by value / `&mut self`, before the
cortex is shared behind an `Arc` and booted), when no adapters are registered
and `self` is owned exclusively. Once booted, `self` is shared immutably, so
`bind_cdp_client` (and the tick loop) is the only reachable writer — which is
what makes the invariant hold in practice.

### Explicitly NOT done (deferred)

The stronger "full single-owner" option was considered and **declined for now**
to keep the blast radius inside `cel-cortex`:

- The browser-rs adapter keeps its own slot and its lazy `probe()`
  `connect_to_focused_app()` self-discovery — it remains a second owner that
  self-syncs by convention (both prefer port 9333). The cortex's propagation
  overwrites the adapter slot on every bind, so the cortex is authoritative
  whenever it binds; but in pure-MCP mode (no `daemon_bridge`, no explicit bind)
  the adapter can still self-discover independently until the first CDP action
  funnels through `cdp_client_or_ambient` and converges them.
- The ambient auto-bind stays gated on `daemon_bridge` (unchanged).
- No mutex-type unification to literally share one slot object across the crate
  boundary (the cortex uses `std::sync::Mutex`; the adapter uses
  `tokio::sync::Mutex`).

The TS-adapter regime (separate process) is untouched: it shares a *target*
(page-level CDP URL) with the cortex via `bind_browser_cdp_url`, as established
by the `642b1aa` page-handle fix.
