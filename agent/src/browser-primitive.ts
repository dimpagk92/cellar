/**
 * Browser primitive — the single source of truth for browser launch in CEL.
 *
 * Per ADR-unify-browser-ownership: both BrowserAdapter and the Cortex
 * dedicated-Chrome path should call `cel.ensureBrowser()` instead of
 * launching their own Chromium instances. This file owns the actual
 * launch + CDP URL extraction; the Cel class is a thin delegation layer
 * over it.
 *
 * Implementation note: we spawn Playwright's bundled Chromium binary
 * directly with `--remote-debugging-port=0`. That gives us a real CDP
 * endpoint (compatible with `chromium.connectOverCDP(url)`), unlike
 * `chromium.launchServer()` which returns a Playwright-protocol endpoint
 * that's NOT compatible with `connectOverCDP`.
 */

import { spawn, type ChildProcess } from "node:child_process";
import { mkdirSync } from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

export interface EnsureBrowserOptions {
  /**
   * Default true. Pass false for agent-assistant use cases that want a
   * visible window (e.g. the MCP `ensureDedicatedCdpBrowser` path).
   */
  headless?: boolean;

  /** Default { width: 1280, height: 800 } */
  viewport?: { width: number; height: number };

  /** Default true — adds Chrome stealth flags. */
  stealth?: boolean;

  /**
   * Specific CDP port to bind. When omitted, an auto-assigned free port
   * is used (`--remote-debugging-port=0`).
   */
  port?: number;

  /** Profile directory. When omitted, a fresh ephemeral dir is created. */
  profileDir?: string;
}

export interface BrowserHandle {
  /** ws://127.0.0.1:PORT/devtools/browser/UUID — standard CDP endpoint. */
  cdpUrl: string;
  /** The CDP port the browser is listening on. */
  port: number;
  /** True if the underlying browser process is still running. */
  isAlive(): Promise<boolean>;
  /** Tear down the browser. Idempotent. */
  close(): Promise<void>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Stealth flags — mirror adapters/browser/src/cdp-client.ts.
// ─────────────────────────────────────────────────────────────────────────────
const STEALTH_ARGS = [
  "--disable-blink-features=AutomationControlled",
  "--disable-features=IsolateOrigins,site-per-process",
  "--disable-site-isolation-trials",
  "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows",
  "--disable-renderer-backgrounding",
  "--disable-popup-blocking",
  "--disable-ipc-flooding-protection",
  "--password-store=basic",
  "--use-mock-keychain",
];

// Default Chrome args we always pass for a sane benchmark / agent profile.
//
// `--no-sandbox` is included whenever we detect we're running as root
// (UID 0) OR the caller explicitly opted in via CEL_BROWSER_NO_SANDBOX=1.
// Chromium refuses to launch as root without it, and the Hetzner benchmark
// server (`root@204.168.232.124:/opt/cellar`) is always root. Without this,
// every server-side webvoyager / mind2web / browsergym run dies before
// task 1 with "Running as root without --no-sandbox is not supported".
// See feedback_cellar_server_benchmarks.md for the full story.
const BASE_ARGS: string[] = [
  "--no-first-run",
  "--no-default-browser-check",
  "--disable-sync",
  ...(process.getuid?.() === 0 || process.env.CEL_BROWSER_NO_SANDBOX === "1"
    ? ["--no-sandbox"]
    : []),
];

interface SingletonState {
  proc: ChildProcess;
  cdpUrl: string;
  port: number;
  profileDir: string;
  options: Required<Pick<EnsureBrowserOptions, "headless" | "viewport" | "stealth">>;
}

let singleton: SingletonState | null = null;
let inflightLaunch: Promise<BrowserHandle> | null = null;

/**
 * Launch (or reuse) the CEL-managed browser. Returns a CDP URL clients
 * can attach to with `chromium.connectOverCDP(url)` (or any CDP client).
 *
 * Idempotent: calling twice with compatible options returns the same handle.
 * Calling with incompatible options throws — caller must `closeBrowser()`
 * first.
 *
 * Uses Playwright's bundled Chromium binary, independent of any
 * system-installed browser.
 */
export async function ensureBrowserInternal(
  options: EnsureBrowserOptions = {},
): Promise<BrowserHandle> {
  const effective = {
    headless: options.headless ?? true,
    viewport: options.viewport ?? { width: 1280, height: 800 },
    stealth: options.stealth ?? true,
    port: options.port,
    profileDir: options.profileDir,
  };

  if (singleton) {
    const alive = !singleton.proc.killed && singleton.proc.exitCode === null;
    if (alive) {
      if (!areOptionsCompatible(singleton.options, effective)) {
        throw new Error(
          "cel.ensureBrowser called with incompatible options for the currently-running browser. " +
            "Call cel.closeBrowser() first if you need a different configuration.",
        );
      }
      return makeHandle(singleton);
    }
    singleton = null;
  }

  if (inflightLaunch) return inflightLaunch;

  inflightLaunch = (async () => {
    try {
      return await launchAndAdopt(effective);
    } finally {
      inflightLaunch = null;
    }
  })();

  return inflightLaunch;
}

/** Close the CEL-managed browser. Idempotent. */
export async function closeBrowserInternal(): Promise<void> {
  if (!singleton) return;
  const s = singleton;
  singleton = null;
  try {
    s.proc.kill("SIGTERM");
    // Force-kill after grace period.
    const killed = await Promise.race<boolean>([
      new Promise<boolean>((resolve) => s.proc.once("exit", () => resolve(true))),
      new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 3000)),
    ]);
    if (!killed) {
      try { s.proc.kill("SIGKILL"); } catch {}
    }
  } catch {
    // Process may already be dead.
  }
}

/** Test-only escape hatch. */
export function _hasSingletonForTests(): boolean {
  return singleton !== null;
}

// ─────────────────────────────────────────────────────────────────────────────
// Internals
// ─────────────────────────────────────────────────────────────────────────────

async function launchAndAdopt(
  effective: Required<Pick<EnsureBrowserOptions, "headless" | "viewport" | "stealth">> &
    Pick<EnsureBrowserOptions, "port" | "profileDir">,
): Promise<BrowserHandle> {
  // Dynamic import so Cel works without Playwright until ensureBrowser is called.
  const { chromium } = await import("playwright");
  const binaryPath = chromium.executablePath();
  if (!binaryPath) {
    throw new Error(
      "Playwright bundled Chromium not found. Run `pnpm exec playwright install chromium` in the workspace.",
    );
  }

  // Default under ~/.cellar/cdp-profiles/ so isCelOwnedUserDataDir() in
  // agent/src/cdp-browser.ts recognises this browser as a CEL-owned dedicated
  // CDP browser. The Rust cortex's adapter-binding code relies on that match
  // to attach a CDP client to the browser; if the profile is anywhere else
  // (e.g. /tmp), discovery succeeds but binding silently does not, and
  // downstream operations fall back to vision+screen-coordinate input on the
  // wrong window. See ADR-unify-browser-ownership Phase 3.
  const profileDir = effective.profileDir ??
    path.join(
      os.homedir(),
      ".cellar",
      "cdp-profiles",
      `playwright-${process.pid}-${Date.now()}`,
    );
  mkdirSync(profileDir, { recursive: true });

  const port = effective.port ?? 0; // 0 = auto-assign free port
  const args: string[] = [
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profileDir}`,
    ...BASE_ARGS,
  ];

  if (effective.headless) {
    args.push("--headless=new");
  }
  if (effective.stealth) {
    args.push(...STEALTH_ARGS);
  }

  // The viewport is applied per-context, not at launch, so it's recorded
  // on the singleton for compatibility checks rather than passed as flags.

  const proc = spawn(binaryPath, args, {
    stdio: ["ignore", "pipe", "pipe"],
    detached: false,
  });

  // Discover the actual port from stderr's "DevTools listening on ws://..." line.
  // (Required when port=0 because Chromium picks a free port; useful even with
  // explicit port for validating that it bound successfully.)
  const { actualPort, cdpUrl } = await waitForDevToolsEndpoint(proc, 15_000);

  singleton = {
    proc,
    cdpUrl,
    port: actualPort,
    profileDir,
    options: {
      headless: effective.headless,
      viewport: effective.viewport,
      stealth: effective.stealth,
    },
  };

  // Auto-cleanup if the process dies on its own.
  proc.once("exit", () => {
    if (singleton?.proc === proc) {
      singleton = null;
    }
  });

  return makeHandle(singleton);
}

function waitForDevToolsEndpoint(
  proc: ChildProcess,
  timeoutMs: number,
): Promise<{ actualPort: number; cdpUrl: string }> {
  return new Promise((resolve, reject) => {
    let resolved = false;
    let stderrBuf = "";

    const timer = setTimeout(() => {
      if (!resolved) {
        resolved = true;
        try { proc.kill("SIGTERM"); } catch {}
        reject(
          new Error(
            `Chromium did not announce DevTools endpoint within ${timeoutMs}ms. ` +
              `Last stderr: ${stderrBuf.slice(-500)}`,
          ),
        );
      }
    }, timeoutMs);

    proc.stderr?.on("data", (chunk: Buffer) => {
      if (resolved) return;
      stderrBuf += chunk.toString();
      // Chromium prints:  DevTools listening on ws://127.0.0.1:PORT/devtools/browser/UUID
      const match = stderrBuf.match(/DevTools listening on (ws:\/\/[^\s]+)/);
      if (match) {
        resolved = true;
        clearTimeout(timer);
        const cdpUrl = match[1];
        const portMatch = cdpUrl.match(/:(\d+)\//);
        const actualPort = portMatch ? parseInt(portMatch[1], 10) : 0;
        resolve({ actualPort, cdpUrl });
      }
    });

    proc.once("error", (err) => {
      if (!resolved) {
        resolved = true;
        clearTimeout(timer);
        reject(err);
      }
    });

    proc.once("exit", (code, signal) => {
      if (!resolved) {
        resolved = true;
        clearTimeout(timer);
        reject(
          new Error(
            `Chromium exited before announcing DevTools endpoint (code=${code}, signal=${signal}). ` +
              `stderr: ${stderrBuf.slice(-500)}`,
          ),
        );
      }
    });
  });
}

function makeHandle(s: SingletonState): BrowserHandle {
  return {
    cdpUrl: s.cdpUrl,
    port: s.port,
    isAlive: async () => {
      const cur = singleton;
      if (!cur) return false;
      return !cur.proc.killed && cur.proc.exitCode === null;
    },
    close: closeBrowserInternal,
  };
}

function areOptionsCompatible(
  existing: SingletonState["options"],
  requested: SingletonState["options"],
): boolean {
  return (
    existing.headless === requested.headless &&
    existing.viewport.width === requested.viewport.width &&
    existing.viewport.height === requested.viewport.height &&
    existing.stealth === requested.stealth
  );
}
