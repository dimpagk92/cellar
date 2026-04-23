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
  cel?: Pick<Cel, "discoverCdpTargets" | "cdpNavigate">;
  port?: number;
  url?: string;
  timeoutMs?: number;
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
    if (options.url && options.cel?.cdpNavigate) {
      await options.cel.cdpNavigate(options.url);
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

  const browser = await chooseChromiumBrowser();
  if (!browser) {
    return {
      ok: false,
      launched: false,
      browser: null,
      status: initialStatus,
      message: "No Chromium-family browser found. Install Chrome, Chromium, Brave, Edge, or Arc.",
    };
  }

  const profileDir = path.join(getCelCdpProfileRoot(), browser.profileDirName);
  mkdirSync(profileDir, { recursive: true });

  const initialUrl = options.url ?? "about:blank";
  launchDedicatedBrowser(browser, requestedPort, profileDir, initialUrl);

  const timeoutMs = options.timeoutMs ?? 10_000;
  const deadline = Date.now() + timeoutMs;

  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 500));
    const status = await getDedicatedCdpBrowserStatus(options.cel, requestedPort);
    if (status.ready) {
      return {
        ok: true,
        launched: true,
        browser,
        status,
        message: `CEL browser (${browser.appName}) ready on port ${requestedPort}.`,
      };
    }
  }

  const finalStatus = await getDedicatedCdpBrowserStatus(options.cel, requestedPort);
  return {
    ok: false,
    launched: true,
    browser,
    status: finalStatus,
    message: `${browser.appName} launched, but CDP was not ready on port ${requestedPort} after ${timeoutMs}ms.`,
  };
}
