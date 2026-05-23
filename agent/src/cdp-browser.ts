import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import type { Cel } from "./cel-bindings.js";

export const DEFAULT_CEL_CDP_PORT = 9333;
const CEL_CDP_PROFILE_SEGMENT = path.join(".cellar", "cdp-profiles");

export type CdpTargetLike = {
  app_name?: string;
  appName?: string;
  pid?: number;
  port: number;
  ws_url?: string;
  wsUrl?: string;
};

export type ChromiumCandidate = {
  appName: string;
  bundleId: string;
  binaryPath: string;
  profileDirName: string;
};

export type DedicatedCdpBrowserStatus = {
  port: number;
  running: boolean;
  ready: boolean;
  ownedByCel: boolean;
  conflict: boolean;
  browserApp: string | null;
  browserVersion: string | null;
  userDataDir: string | null;
  webSocketDebuggerUrl: string | null;
  targetCount: number;
  profileRoot: string;
  processPid: number | null;
};

export type CanonicalCdpTarget = {
  app_name: string;
  pid: number;
  port: number;
  ws_url: string;
  title?: string;
  url?: string;
  type?: string;
  source: "native" | "http" | "merged";
};

export type CanonicalCdpState = {
  status: DedicatedCdpBrowserStatus;
  targets: CanonicalCdpTarget[];
  preferredTarget: CanonicalCdpTarget | null;
  rawTargetCount: number;
  mismatch: boolean;
};

export type EnsureDedicatedCdpBrowserOptions = {
  cel?: Pick<Cel, "discoverCdpTargets" | "cdpNavigate" | "ensureBrowser" | "closeBrowser" | "bindBrowserCdpUrl" | "cortexRefreshNow">;
  port?: number;
  url?: string;
  timeoutMs?: number;
  cleanupBlanksAfter?: boolean;
};

export type EnsureDedicatedCdpBrowserResult = {
  ok: boolean;
  launched: boolean;
  browser: ChromiumCandidate | null;
  status: DedicatedCdpBrowserStatus;
  message: string;
};

const CHROMIUM_CANDIDATES: ChromiumCandidate[] = [
  {
    appName: "Google Chrome",
    bundleId: "com.google.Chrome",
    binaryPath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    profileDirName: "google-chrome",
  },
  {
    appName: "Chromium",
    bundleId: "org.chromium.Chromium",
    binaryPath: "/Applications/Chromium.app/Contents/MacOS/Chromium",
    profileDirName: "chromium",
  },
  {
    appName: "Brave Browser",
    bundleId: "com.brave.Browser",
    binaryPath: "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    profileDirName: "brave-browser",
  },
  {
    appName: "Microsoft Edge",
    bundleId: "com.microsoft.edgemac",
    binaryPath: "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    profileDirName: "microsoft-edge",
  },
  {
    appName: "Arc",
    bundleId: "company.thebrowser.Browser",
    binaryPath: "/Applications/Arc.app/Contents/MacOS/Arc",
    profileDirName: "arc",
  },
];

function normalizeBrowserAppName(name: string | undefined): string {
  return (name ?? "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, " ")
    .trim();
}

function scoreTarget(target: CdpTargetLike, frontmostAppName: string | undefined, preferredPort: number): number {
  const targetName = normalizeBrowserAppName(target.app_name ?? target.appName);
  const frontmost = normalizeBrowserAppName(frontmostAppName);

  let score = 0;
  if (target.port !== preferredPort) score += 20;

  if (frontmost) {
    if (targetName === frontmost) {
      score += 0;
    } else if (targetName.includes(frontmost) || frontmost.includes(targetName)) {
      score += 4;
    } else {
      score += 8;
    }
  }

  if (!(target.ws_url ?? target.wsUrl)) {
    score += 100;
  }

  return score;
}

export function getPreferredCelCdpPort(rawValue = process.env.CEL_CDP_PORT): number {
  const parsed = Number.parseInt(rawValue ?? "", 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : DEFAULT_CEL_CDP_PORT;
}

export function getCelCdpProfileRoot(): string {
  return path.join(os.homedir(), CEL_CDP_PROFILE_SEGMENT);
}

export function isCelOwnedUserDataDir(userDataDir: string | null | undefined): boolean {
  if (!userDataDir) return false;
  const normalized = userDataDir.replace(/\\/g, "/");
  return normalized.includes(CEL_CDP_PROFILE_SEGMENT.replace(/\\/g, "/"));
}

function readProcessCommandLine(pid: number | undefined): string | null {
  if (!pid || pid <= 0) return null;
  try {
    const commandLine = execFileSync("ps", ["-o", "command=", "-p", String(pid)], {
      encoding: "utf-8",
    }).trim();
    return commandLine || null;
  } catch {
    return null;
  }
}

function isCelOwnedCommandLine(commandLine: string | null | undefined): boolean {
  if (!commandLine) return false;
  const normalized = commandLine.replace(/\\/g, "/");
  return normalized.includes(CEL_CDP_PROFILE_SEGMENT.replace(/\\/g, "/"));
}

function findCelBrowserProcess(port: number): { pid: number; commandLine: string } | null {
  try {
    const output = execFileSync("ps", ["-o", "pid=,command=", "-ax"], {
      encoding: "utf-8",
    });
    const portFlag = `--remote-debugging-port=${port}`;
    for (const rawLine of output.split("\n")) {
      const line = rawLine.trim();
      if (!line || !line.includes(portFlag)) continue;
      if (!isCelOwnedCommandLine(line)) continue;
      const match = line.match(/^(\d+)\s+(.*)$/);
      if (!match) continue;
      const pid = Number.parseInt(match[1] ?? "", 10);
      const commandLine = match[2] ?? "";
      if (Number.isFinite(pid) && commandLine) {
        return { pid, commandLine };
      }
    }
  } catch {
    // Best effort only.
  }
  return null;
}

export function selectPreferredCdpTarget<T extends CdpTargetLike>(
  targets: T[],
  frontmostAppName?: string,
  preferredPort = getPreferredCelCdpPort(),
): T | undefined {
  return [...targets]
    .filter((target) => Boolean(target.ws_url ?? target.wsUrl))
    .sort((left, right) => {
      const leftScore = scoreTarget(left, frontmostAppName, preferredPort);
      const rightScore = scoreTarget(right, frontmostAppName, preferredPort);
      if (leftScore !== rightScore) return leftScore - rightScore;
      return left.port - right.port;
    })[0];
}

type JsonObject = Record<string, unknown>;

async function fetchJson(url: string): Promise<JsonObject | JsonObject[] | null> {
  try {
    const response = await fetch(url);
    if (!response.ok) return null;
    return await response.json() as JsonObject | JsonObject[];
  } catch {
    return null;
  }
}

async function queryVersion(port: number): Promise<JsonObject | null> {
  const payload = await fetchJson(`http://127.0.0.1:${port}/json/version`);
  return payload && !Array.isArray(payload) ? payload : null;
}

async function queryTargetCount(port: number): Promise<number> {
  const payload = await fetchJson(`http://127.0.0.1:${port}/json/list`);
  return Array.isArray(payload) ? payload.length : 0;
}

async function fetchHttpOk(url: string): Promise<boolean> {
  try {
    const response = await fetch(url);
    return response.ok;
  } catch {
    return false;
  }
}

/**
 * Close every page target on the given CEL CDP port whose URL is `about:blank`
 * or empty. Used to suppress popunder/exit blanks that ad-heavy sites spawn as
 * a side effect of navigation.
 *
 * Targets are enumerated via `/json/list` and closed via `/json/close/<id>`.
 * Failures are swallowed; the returned `closed` count reflects only the tabs
 * that the browser acknowledged closing.
 */
export async function cleanupBlankCdpTabs(port?: number): Promise<{ closed: number }> {
  const resolvedPort = port ?? getPreferredCelCdpPort();
  const payload = await fetchJson(`http://127.0.0.1:${resolvedPort}/json/list`);
  if (!Array.isArray(payload)) return { closed: 0 };

  let closed = 0;
  for (const entry of payload) {
    if (!entry || typeof entry !== "object") continue;
    const page = entry as JsonObject;
    if (page.type !== "page") continue;
    const url = typeof page.url === "string" ? page.url : "";
    if (url !== "" && url !== "about:blank") continue;
    const id = typeof page.id === "string" ? page.id : null;
    if (!id) continue;
    if (await fetchHttpOk(`http://127.0.0.1:${resolvedPort}/json/close/${id}`)) {
      closed += 1;
    }
  }
  return { closed };
}

async function queryTargets(port: number): Promise<CanonicalCdpTarget[]> {
  const payload = await fetchJson(`http://127.0.0.1:${port}/json/list`);
  if (!Array.isArray(payload)) return [];

  const version = await queryVersion(port);
  const browserApp = typeof version?.Browser === "string"
    ? version.Browser.split("/")[0] ?? "Browser"
    : "Browser";

  const targets: CanonicalCdpTarget[] = [];
  for (const entry of payload) {
    if (!entry || typeof entry !== "object") continue;
    const page = entry as JsonObject;
    const wsUrl = typeof page.webSocketDebuggerUrl === "string" ? page.webSocketDebuggerUrl : null;
    const type = typeof page.type === "string" ? page.type : undefined;
    const url = typeof page.url === "string" ? page.url : undefined;
    if (!wsUrl || type !== "page" || (url ?? "").startsWith("devtools://")) {
      continue;
    }
    targets.push({
      app_name: browserApp,
      pid: 0,
      port,
      ws_url: wsUrl,
      title: typeof page.title === "string" ? page.title : undefined,
      url,
      type,
      source: "http",
    });
  }
  return targets;
}

async function detectDefaultBrowserBundleId(): Promise<string | null> {
  try {
    const { execFileSync } = await import("node:child_process");
    const output = execFileSync(
      "defaults",
      ["read", "com.apple.LaunchServices/com.apple.launchservices.secure", "LSHandlers"],
      { encoding: "utf-8" },
    );
    const lines = output.split("\n");
    for (let i = 0; i < lines.length; i += 1) {
      if (!lines[i]?.includes("LSHandlerURLScheme = http;")) continue;
      for (let j = i; j < Math.min(i + 8, lines.length); j += 1) {
        const match = lines[j]?.match(/LSHandlerRoleAll = \"([^\"]+)\";/);
        if (match?.[1]) return match[1];
      }
    }
  } catch {
    // Best effort only.
  }

  return null;
}

function getCandidateByBundleId(bundleId: string): ChromiumCandidate | null {
  return CHROMIUM_CANDIDATES.find((candidate) => candidate.bundleId === bundleId) ?? null;
}

export async function chooseChromiumBrowser(): Promise<ChromiumCandidate | null> {
  const defaultBundleId = await detectDefaultBrowserBundleId();
  const defaultCandidate = defaultBundleId ? getCandidateByBundleId(defaultBundleId) : null;
  if (defaultCandidate && existsSync(defaultCandidate.binaryPath)) {
    return defaultCandidate;
  }

  return CHROMIUM_CANDIDATES.find((candidate) => existsSync(candidate.binaryPath)) ?? null;
}

export async function getDedicatedCdpBrowserStatus(
  cel?: Pick<Cel, "discoverCdpTargets">,
  requestedPort = getPreferredCelCdpPort(),
): Promise<DedicatedCdpBrowserStatus> {
  const version = await queryVersion(requestedPort);
  const userDataDir = typeof version?.["User-Data-Dir"] === "string"
    ? version["User-Data-Dir"]
    : null;
  const browserVersion = typeof version?.Browser === "string" ? version.Browser : null;
  const webSocketDebuggerUrl = typeof version?.webSocketDebuggerUrl === "string"
    ? version.webSocketDebuggerUrl
    : null;

  const discoveredTargets = await discoverCanonicalCdpTargets(cel, requestedPort);
  const targetCount = discoveredTargets.length > 0
    ? discoveredTargets.length
    : await queryTargetCount(requestedPort);
  const running = Boolean(version) || targetCount > 0;
  const processMatch = findCelBrowserProcess(requestedPort);

  const ownedByCel = isCelOwnedUserDataDir(userDataDir)
    || discoveredTargets.some((target) => isCelOwnedCommandLine(readProcessCommandLine(target.pid)))
    || Boolean(processMatch);
  const ready = running && ownedByCel && targetCount > 0;

  return {
    port: requestedPort,
    running,
    ready,
    ownedByCel,
    conflict: running && !ownedByCel,
    browserApp: browserVersion?.split("/")[0] ?? discoveredTargets[0]?.app_name ?? null,
    browserVersion,
    userDataDir,
    webSocketDebuggerUrl,
    targetCount,
    profileRoot: getCelCdpProfileRoot(),
    processPid: processMatch?.pid ?? null,
  };
}

export async function discoverCanonicalCdpTargets(
  cel?: Pick<Cel, "discoverCdpTargets">,
  requestedPort = getPreferredCelCdpPort(),
): Promise<CanonicalCdpTarget[]> {
  const nativeTargets = (cel?.discoverCdpTargets() ?? [])
    .filter((target) => target.port === requestedPort)
    .map((target) => ({
      app_name: target.app_name,
      pid: target.pid,
      port: target.port,
      ws_url: target.ws_url,
      source: "native" as const,
    }));
  const httpTargets = await queryTargets(requestedPort);
  const merged = new Map<string, CanonicalCdpTarget>();

  for (const target of nativeTargets) {
    merged.set(target.ws_url, target);
  }
  for (const target of httpTargets) {
    const existing = merged.get(target.ws_url);
    if (existing) {
      merged.set(target.ws_url, {
        ...target,
        app_name: existing.app_name || target.app_name,
        pid: existing.pid || target.pid,
        source: "merged",
      });
    } else {
      merged.set(target.ws_url, target);
    }
  }

  return [...merged.values()].sort((left, right) => {
    if (left.port !== right.port) return left.port - right.port;
    return left.ws_url.localeCompare(right.ws_url);
  });
}

export async function getCanonicalCdpState(
  cel?: Pick<Cel, "discoverCdpTargets" | "getQuickContext">,
  requestedPort = getPreferredCelCdpPort(),
): Promise<CanonicalCdpState> {
  const status = await getDedicatedCdpBrowserStatus(cel, requestedPort);
  const targets = await discoverCanonicalCdpTargets(cel, requestedPort);
  const rawTargets = cel?.discoverCdpTargets().filter((target) => target.port === requestedPort) ?? [];
  const preferredTarget = selectPreferredCdpTarget(
    targets,
    cel?.getQuickContext?.().app,
    requestedPort,
  ) ?? null;

  return {
    status,
    targets,
    preferredTarget,
    rawTargetCount: rawTargets.length,
    mismatch: rawTargets.length !== targets.length,
  };
}

function launchDedicatedBrowser(
  browser: ChromiumCandidate,
  port: number,
  profileDir: string,
  initialUrl: string,
): void {
  const child = spawn(browser.binaryPath, [
    `--remote-debugging-port=${port}`,
    "--remote-allow-origins=*",
    `--user-data-dir=${profileDir}`,
    "--new-window",
    initialUrl,
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-sync",
  ], {
    detached: true,
    stdio: "ignore",
  });
  child.unref();
}

export async function ensureDedicatedCdpBrowser(
  options: EnsureDedicatedCdpBrowserOptions = {},
): Promise<EnsureDedicatedCdpBrowserResult> {
  const requestedPort = options.port ?? getPreferredCelCdpPort();
  const initialStatus = await getDedicatedCdpBrowserStatus(options.cel, requestedPort);

  if (initialStatus.ready) {
    // ① Clean up any blank/popup tabs from prior runs BEFORE binding.
    //    bind_browser_cdp_url picks the first entry from /json/list, which
    //    is the most-recently-opened target. If a prior run's window.open
    //    popup (about:blank) is still alive it would be picked over the
    //    actual fixture page, causing the cortex to read the wrong DOM.
    //    Closing blanks here ensures we always bind to the real fixture tab.
    const shouldCleanup = options.cleanupBlanksAfter ?? true;
    if (shouldCleanup) {
      await cleanupBlankCdpTabs(requestedPort);
    }

    // Re-bind the cortex BrowserAdapter to the existing browser. The Rust
    // adapter starts with cdp_client = None on every cortex boot, so it must
    // be re-attached whenever a browser was left running from a previous MCP
    // session. bindBrowserCdpUrl internally resolves a page-level WebSocket
    // URL via /json/list, so passing the browser-level URL is fine.
    // We await here so the adapter is bound before the caller navigates or
    // starts the goal runner — firing-and-forgetting leaves a window where
    // the runner tries to use an unbound adapter.
    if (options.cel?.bindBrowserCdpUrl) {
      const browserUrl =
        initialStatus.webSocketDebuggerUrl ??
        `ws://127.0.0.1:${requestedPort}/devtools/browser/existing`;
      try {
        await options.cel.bindBrowserCdpUrl(browserUrl);
      } catch {
        // Non-fatal: probe() will attempt ambient discovery as fallback.
      }
    }
    if (options.url && options.cel?.cdpNavigate) {
      // ② Force a full JS-state reset by bouncing through about:blank first.
      //    A direct navigate to the same URL is a no-op in Chrome (the page
      //    isn't reloaded, so acknowledged=true / stale result-text persist).
      //    Navigating to about:blank first destroys the page's JS context,
      //    then navigating to the fixture URL loads it clean.
      await options.cel.cdpNavigate("about:blank");
      await new Promise((resolve) => setTimeout(resolve, 300));
      await options.cel.cdpNavigate(options.url);
      // Give the page JS a moment to initialise before the cortex reads DOM.
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
    // Force a cortex tick now so the BrowserAdapter activates (sets
    // connected=true) and DOM elements land in the mental model before
    // the first LLM planning call reads context. Without this, the runner's
    // first context snapshot would be stale (no DOM elements) because the
    // background tick fires every 200 ms and might not have run yet.
    if (options.cel?.cortexRefreshNow) {
      try {
        await options.cel.cortexRefreshNow(3000);
      } catch {
        // Non-fatal — the background tick loop will catch up.
      }
    }
    return {
      ok: true,
      launched: false,
      browser: null,
      status: initialStatus,
      message: `CEL browser already running on port ${requestedPort}.`,
    };
  }

  if (initialStatus.conflict) {
    return {
      ok: false,
      launched: false,
      browser: null,
      status: initialStatus,
      message: `Port ${requestedPort} is already occupied by a non-CEL browser instance.`,
    };
  }

  // Phase 3 of ADR-unify-browser-ownership: delegate to cel.ensureBrowser
  // instead of spawning a system-installed Chrome/Brave/Edge/Arc binary.
  // The CEL-managed browser uses Playwright's bundled Chromium, so no
  // system browser install is required. cel.ensureBrowser also calls
  // cel.bindBrowserCdpUrl() internally so the cortex's BrowserAdapter
  // attaches to the CDP URL directly — no reliance on
  // connect_to_focused_app discovery (which fails for headless browsers).
  if (!options.cel?.ensureBrowser) {
    return {
      ok: false,
      launched: false,
      browser: null,
      status: initialStatus,
      message:
        "ensureDedicatedCdpBrowser requires options.cel with ensureBrowser support " +
        "(see ADR-unify-browser-ownership).",
    };
  }

  try {
    // Visible by default — preserves MCP agent-assistant behavior where the
    // user wants to see what the agent is doing.
    const handle = await options.cel.ensureBrowser({
      headless: false,
      port: requestedPort,
    });

    if (options.url && options.cel.cdpNavigate) {
      await options.cel.cdpNavigate(options.url);
      const shouldCleanup = options.cleanupBlanksAfter ?? true;
      if (shouldCleanup) {
        await new Promise((resolve) => setTimeout(resolve, 500));
        await cleanupBlankCdpTabs(handle.port || requestedPort);
      }
    }
    // Force a cortex tick so BrowserAdapter activates and DOM lands in the
    // mental model before the first LLM planning call.
    if (options.cel.cortexRefreshNow) {
      try {
        await options.cel.cortexRefreshNow(3000);
      } catch {
        // Non-fatal.
      }
    }

    return {
      ok: true,
      launched: true,
      // `browser` (ChromiumCandidate) is null in Phase 3+ because we no longer
      // launch a system browser. Callers that need browser metadata should
      // read status.browserApp / status.webSocketDebuggerUrl instead.
      browser: null,
      status: buildStatusFromHandle(handle, requestedPort),
      message: `CEL-managed browser ready on port ${handle.port || requestedPort}.`,
    };
  } catch (err) {
    return {
      ok: false,
      launched: false,
      browser: null,
      status: initialStatus,
      message: `cel.ensureBrowser failed: ${err instanceof Error ? err.message : String(err)}`,
    };
  }
}

/**
 * Build a DedicatedCdpBrowserStatus from a CEL BrowserHandle for return-shape
 * compatibility with the pre-Phase-3 ensureDedicatedCdpBrowser API.
 *
 * The only known caller (mcp-server's ensureCdpChrome) only consumes
 * `message` — the status fields are best-effort.
 */
function buildStatusFromHandle(
  handle: { cdpUrl: string; port: number },
  fallbackPort: number,
): DedicatedCdpBrowserStatus {
  return {
    port: handle.port || fallbackPort,
    running: true,
    ready: true,
    ownedByCel: true,
    conflict: false,
    browserApp: "Playwright Chromium",
    browserVersion: null,
    userDataDir: null,
    webSocketDebuggerUrl: handle.cdpUrl,
    targetCount: 1,
    profileRoot: getCelCdpProfileRoot(),
    processPid: null,
  };
}
