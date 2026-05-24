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
