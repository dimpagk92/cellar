#!/usr/bin/env node
/**
 * Browser Adapter Process Driver — wraps BrowserAdapter for the Cortex stdio protocol.
 *
 * Speaks the Cortex adapter JSON-line protocol over stdin/stdout so the Rust
 * goal-runner can dispatch browser actions through the Cortex adapter system.
 *
 * Protocol:
 *   ← {"method":"activate","params":{"cdp_url":"ws://..."}}
 *   → {"ok":true}
 *   ← {"method":"get_context"}
 *   → {"elements":[...]}
 *   ← {"method":"execute","action":"click","params":{"target_id":"dom:btn1",...}}
 *   → {"success":true}
 *   ← {"method":"deactivate"}
 *   → {"ok":true}
 */

import * as readline from "readline";
import { BrowserAdapter } from "./index.js";
import { executeBrowserAction } from "./callback-builder.js";
import {
  Cel,
  discoverCanonicalCdpTargets,
  getPreferredCelCdpPort,
  selectPreferredCdpTarget,
  type ScreenContext,
  type PlannedAction,
  type ContextElement,
} from "@cellar/agent";

let adapter: BrowserAdapter | null = null;
let lastContext: ScreenContext | null = null;

function respond(data: unknown) {
  process.stdout.write(JSON.stringify(data) + "\n");
}

function normalizeAppName(name: string | undefined): string {
  return (name ?? "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, " ")
    .trim();
}

async function discoverCdpUrlFromCel(): Promise<string | undefined> {
  const cel = new Cel();
  const quick = cel.getQuickContext();
  const quickApp = normalizeAppName(quick.app);
  const targets = await discoverCanonicalCdpTargets(cel);

  if (targets.length === 0) return undefined;

  const preferred = selectPreferredCdpTarget(targets, quickApp, getPreferredCelCdpPort());
  if (preferred?.ws_url) return preferred.ws_url;

  return targets[0]?.ws_url;
}

async function handleRequest(req: { method: string; action?: string; params?: any }) {
  try {
    switch (req.method) {
      case "activate": {
        let cdpUrl = req.params?.cdp_url ?? req.params?.cdpUrl ?? process.env.CEL_CDP_URL;

        // Prefer CEL's richer native CDP discovery so we can attach to browsers
        // that are exposing a non-default debugging port.
        if (!cdpUrl) {
          cdpUrl = await discoverCdpUrlFromCel();
        }

        // Fallback to the historical direct port probes.
        if (!cdpUrl) {
          for (const port of [9333, 9222]) {
            try {
              const resp = await fetch(`http://127.0.0.1:${port}/json`);
              const targets = await resp.json() as Array<{ webSocketDebuggerUrl?: string; type?: string; url?: string }>;
              const page = targets.find(
                (t) => t.webSocketDebuggerUrl && t.type === "page" && !t.url?.startsWith("devtools://"),
              );
              if (page?.webSocketDebuggerUrl) { cdpUrl = page.webSocketDebuggerUrl; break; }
            } catch { /* try next port */ }
          }
        }

        if (!cdpUrl) {
          respond({ ok: false, error: "No CDP target found. Enable CEL CDP setup or launch a browser with remote debugging." });
          return;
        }

        adapter = new BrowserAdapter({ browser: "chromium", useCdp: true, cdpUrl });
        await adapter.connect();
        try { await adapter.dismissCookieConsent(); } catch {}
        respond({ ok: true });
        break;
      }

      case "get_context": {
        if (!adapter) {
          respond({ elements: [] });
          return;
        }
        try {
          const ctx = await Promise.race([
            adapter.getContextFast(),
            new Promise<ScreenContext>((_, reject) =>
              setTimeout(() => reject(new Error("timeout")), 5000),
            ),
          ]);
          lastContext = ctx;

          // Map to ContextElement[] (strip ScreenContext wrapper)
          const elements: ContextElement[] = ctx.elements.map((el) => ({
            ...el,
            // Ensure source is set for Cortex adapter index
            source: "native_api" as any,
          }));
          respond({ elements });
        } catch {
          respond({ elements: [] });
        }
        break;
      }

      case "execute": {
        if (!adapter) {
          respond({ success: false, error: "Not activated" });
          return;
        }

        const action = req.action ?? req.params?.action;
        const params = req.params ?? {};

        // Map to PlannedAction format
        const ctx = lastContext ?? {
          app: "Browser", window: "", elements: [], timestamp_ms: Date.now(),
          network_events: [], http_events: [],
        } as ScreenContext;

        try {
          let success: boolean;

          // Custom browser actions (navigate, evaluate, etc.)
          if (action === "navigate") {
            await adapter.navigate(params.url);
            await adapter.waitForStable({ timeout: 3000, idleTime: 150 });
            try { await adapter.dismissCookieConsent(); } catch {}
            success = true;
          } else if (action === "evaluate") {
            await adapter.evaluate(params.expression ?? params.code);
            success = true;
          } else if (action === "screenshot") {
            const buf = await adapter.screenshot();
            respond({ success: true, data: buf.toString("base64") });
            return;
          } else if (action === "dismiss_cookie_consent") {
            await adapter.dismissCookieConsent();
            success = true;
          } else if (action === "wait_for_stable") {
            await adapter.waitForStable({ timeout: params.timeout ?? 3000 });
            success = true;
          } else {
            // Structured actions (click, type, set_value, etc.) — route through executeBrowserAction
            const plannedAction = buildPlannedAction(action, params);
            if (plannedAction) {
              success = await executeBrowserAction(adapter, plannedAction, ctx);
            } else {
              // Fall back to adapter.executeAction for unknown action types
              success = await adapter.executeAction(action, params);
            }
          }

          respond({ success });
        } catch (e) {
          respond({ success: false, error: String(e) });
        }
        break;
      }

      case "deactivate": {
        if (adapter) {
          try { await adapter.disconnect(); } catch {}
          adapter = null;
        }
        lastContext = null;
        respond({ ok: true });
        break;
      }

      case "probe": {
        if (!adapter) {
          respond({ available: false });
          return;
        }
        try {
          await adapter.evaluate("document.title");
          respond({ available: true });
        } catch {
          respond({ available: false });
        }
        break;
      }

      default:
        respond({ error: `Unknown method: ${req.method}` });
    }
  } catch (e) {
    respond({ error: String(e) });
  }
}

/** Build a PlannedAction from the process driver protocol's action+params. */
function buildPlannedAction(action: string, params: any): PlannedAction | null {
  switch (action) {
    case "click":
      return { type: "click", target_id: params.target_id } as PlannedAction;
    case "type":
      return { type: "type", target_id: params.target_id, text: params.text } as PlannedAction;
    case "set_value":
      return { type: "set_value", target_id: params.target_id, value: params.value } as PlannedAction;
    case "key":
      return { type: "key", key: params.key } as PlannedAction;
    case "key_combo":
      return { type: "key_combo", keys: params.keys } as PlannedAction;
    case "scroll":
      return { type: "scroll", dx: params.dx ?? 0, dy: params.dy ?? -3 } as PlannedAction;
    case "select_option":
      return { type: "custom", adapter: "browser", action: "select_option", params } as PlannedAction;
    default:
      return null;
  }
}

// ── Main: read stdin JSON lines ────────────────────────────────────────────

const rl = readline.createInterface({ input: process.stdin, terminal: false });

rl.on("line", (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  try {
    const req = JSON.parse(trimmed);
    handleRequest(req).catch((e) => {
      respond({ error: String(e) });
    });
  } catch (e) {
    respond({ error: `Invalid JSON: ${e}` });
  }
});

rl.on("close", () => {
  if (adapter) {
    adapter.disconnect().catch(() => {}).finally(() => process.exit(0));
  } else {
    process.exit(0);
  }
});

// Handle signals gracefully
process.on("SIGTERM", () => {
  if (adapter) adapter.disconnect().catch(() => {}).finally(() => process.exit(0));
  else process.exit(0);
});
