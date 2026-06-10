/**
 * Daemon IPC client — drives the daemon-hosted Cortex over the UDS socket.
 *
 * Phase C of `cellar-daemon-cortex.md`: when the running daemon hosts the
 * single live Cortex (`CELLAR_DAEMON_CORTEX=1`, surfaced as
 * `daemon.status.cortex_running`), the MCP server proxies cortex-backed
 * operations (`cortex.see` / `cortex.act` / `cortex.perceive.*`) to it
 * instead of booting its own napi Cortex — two Cortexes would fight over
 * one AX tree and input focus. With no daemon (or `CELLAR_MCP_DAEMON=0`),
 * everything falls back to the existing in-process napi path.
 *
 * Wire protocol: newline-delimited JSON-RPC 2.0 over
 * `$CELLAR_DAEMON_SOCK` (default `~/.cellar/daemon.sock`) — the same
 * framing `cellar-ipc` speaks.
 */

import { createConnection, type Socket } from "node:net";
import { homedir } from "node:os";
import { join } from "node:path";

const CONNECT_TIMEOUT_MS = 500;
const PROBE_CALL_TIMEOUT_MS = 2_000;
const DEFAULT_CALL_TIMEOUT_MS = 15_000;
/** `cortex.act` can legitimately take a while (navigate waits, effect polls). */
export const ACT_CALL_TIMEOUT_MS = 120_000;
/** How long a failed probe is cached before the daemon is re-probed. */
const NEGATIVE_PROBE_TTL_MS = 5_000;

function socketPath(): string {
  return process.env.CELLAR_DAEMON_SOCK ?? join(homedir(), ".cellar", "daemon.sock");
}

type Pending = {
  resolve: (value: unknown) => void;
  reject: (err: Error) => void;
  timer: NodeJS.Timeout;
};

/**
 * Minimal persistent JSON-RPC 2.0 client over the daemon's UDS socket.
 * One in-flight map keyed by request id; line-buffered reads; a dead socket
 * rejects everything in flight and the next call reconnects.
 */
export class DaemonClient {
  private socket: Socket | null = null;
  private buffer = "";
  private nextId = 1;
  private pending = new Map<number, Pending>();

  private async connect(): Promise<Socket> {
    if (this.socket && !this.socket.destroyed) return this.socket;
    const socket = await new Promise<Socket>((resolve, reject) => {
      const s = createConnection(socketPath());
      const onError = (err: Error) => {
        s.destroy();
        reject(err);
      };
      s.setTimeout(CONNECT_TIMEOUT_MS, () => onError(new Error("daemon connect timeout")));
      s.once("error", onError);
      s.once("connect", () => {
        s.setTimeout(0);
        s.removeListener("error", onError);
        resolve(s);
      });
    });
    socket.on("data", (chunk) => this.onData(chunk));
    const fail = (why: string) => () => this.failAll(new Error(why));
    socket.on("error", fail("daemon socket error"));
    socket.on("close", fail("daemon socket closed"));
    this.socket = socket;
    return socket;
  }

  private onData(chunk: Buffer | string): void {
    this.buffer += typeof chunk === "string" ? chunk : chunk.toString("utf8");
    let nl: number;
    while ((nl = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, nl).trim();
      this.buffer = this.buffer.slice(nl + 1);
      if (!line) continue;
      let msg: { id?: unknown; result?: unknown; error?: { code?: number; message?: string } };
      try {
        msg = JSON.parse(line);
      } catch {
        continue; // not addressed to us / malformed — skip
      }
      if (typeof msg.id !== "number") continue; // stream notification — ignore
      const pending = this.pending.get(msg.id);
      if (!pending) continue;
      this.pending.delete(msg.id);
      clearTimeout(pending.timer);
      if (msg.error) {
        pending.reject(
          new Error(`daemon rpc error ${msg.error.code ?? "?"}: ${msg.error.message ?? "unknown"}`),
        );
      } else {
        pending.resolve(msg.result);
      }
    }
  }

  private failAll(err: Error): void {
    this.socket?.destroy();
    this.socket = null;
    this.buffer = "";
    for (const [, p] of this.pending) {
      clearTimeout(p.timer);
      p.reject(err);
    }
    this.pending.clear();
  }

  async call<T>(method: string, params?: unknown, timeoutMs = DEFAULT_CALL_TIMEOUT_MS): Promise<T> {
    const socket = await this.connect();
    const id = this.nextId++;
    const line = `${JSON.stringify({ jsonrpc: "2.0", id, method, ...(params !== undefined ? { params } : {}) })}\n`;
    return await new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`daemon rpc timeout: ${method} after ${timeoutMs}ms`));
      }, timeoutMs);
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject, timer });
      socket.write(line, (err) => {
        if (err) {
          this.pending.delete(id);
          clearTimeout(timer);
          reject(err);
        }
      });
    });
  }
}

let stickyClient: DaemonClient | null = null;
let lastNegativeProbeAt = 0;
let probeInFlight: Promise<DaemonClient | null> | null = null;

/**
 * The daemon-hosted-Cortex client, when one is available.
 *
 * Probes the daemon socket + `daemon.status.cortex_running` on first use.
 * A positive result is sticky for the MCP server's lifetime (the daemon owns
 * the single Cortex; flip-flopping transports mid-session would be worse than
 * failing loudly if the daemon goes away). A negative result is re-probed
 * after a short TTL so starting the daemon later is picked up.
 */
export async function daemonCortex(): Promise<DaemonClient | null> {
  if (process.env.CELLAR_MCP_DAEMON === "0") return null;
  if (stickyClient) return stickyClient;
  if (Date.now() - lastNegativeProbeAt < NEGATIVE_PROBE_TTL_MS) return null;
  if (probeInFlight) return probeInFlight;
  probeInFlight = (async () => {
    const client = new DaemonClient();
    try {
      const status = await client.call<{ cortex_running?: boolean }>(
        "daemon.status",
        undefined,
        PROBE_CALL_TIMEOUT_MS,
      );
      if (status?.cortex_running === true) {
        stickyClient = client;
        return client;
      }
    } catch {
      // No daemon / no socket — fall through to napi.
    }
    lastNegativeProbeAt = Date.now();
    return null;
  })();
  try {
    return await probeInFlight;
  } finally {
    probeInFlight = null;
  }
}

/** Sticky sync accessor — non-null once a probe has succeeded. */
export function daemonCortexKnown(): DaemonClient | null {
  return stickyClient;
}

/** Mirror of the IPC `CortexActResult` (engine `ActionResult`). */
export type DaemonActResult = {
  success: boolean;
  error?: string | null;
  data?: unknown;
};

/** Execute one canonical action on the daemon-hosted Cortex. */
export async function daemonAct(client: DaemonClient, action: unknown): Promise<DaemonActResult> {
  return client.call<DaemonActResult>("cortex.act", { action }, ACT_CALL_TIMEOUT_MS);
}

/** Read the daemon Cortex's mental-model snapshot (raw JSON object). */
export async function daemonModel(client: DaemonClient): Promise<unknown> {
  const res = await client.call<{ model: unknown }>("cortex.perceive.read");
  return res.model;
}
