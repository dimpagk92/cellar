/**
 * Storage Watchdog — saves and restores cookies + localStorage across sessions.
 *
 * Critical for staying logged in between workflow runs. Without this,
 * every automation session starts fresh (logged out). Browser-use added
 * StorageStateWatchdog + export_storage_state for the same reason.
 *
 * Two storage modes:
 * 1. Playwright: context.storageState() / newContext({ storageState })
 * 2. CDP: Network.getAllCookies / Network.setCookies + Runtime.evaluate for localStorage
 *
 * License: MIT
 */

import type { Page, BrowserContext } from "playwright";
import type { CdpChannel } from "./cdp-channel.js";
import * as fs from "fs/promises";

export interface StorageState {
  cookies: CookieData[];
  localStorage: LocalStorageEntry[];
  /** When this state was captured. */
  captured_at: string;
  /** URL the state was captured from. */
  origin?: string;
}

export interface CookieData {
  name: string;
  value: string;
  domain: string;
  path: string;
  expires: number;
  httpOnly: boolean;
  secure: boolean;
  sameSite: "Strict" | "Lax" | "None";
}

export interface LocalStorageEntry {
  origin: string;
  key: string;
  value: string;
}

export interface StorageWatchdogConfig {
  /** File path to persist storage state. */
  statePath?: string;
  /** Auto-save on disconnect (default: true). */
  autoSave?: boolean;
  /** Auto-restore on connect (default: true). */
  autoRestore?: boolean;
}

export class StorageWatchdog {
  private config: StorageWatchdogConfig;
  private lastState: StorageState | null = null;

  constructor(config: StorageWatchdogConfig = {}) {
    this.config = config;
  }

  // --- Playwright Mode ---

  /**
   * Capture storage state from a Playwright BrowserContext.
   * Includes cookies + localStorage for all origins.
   */
  async captureFromPlaywright(context: BrowserContext): Promise<StorageState> {
    const pwState = await context.storageState();

    const cookies: CookieData[] = pwState.cookies.map((c) => ({
      name: c.name,
      value: c.value,
      domain: c.domain,
      path: c.path,
      expires: c.expires,
      httpOnly: c.httpOnly,
      secure: c.secure,
      sameSite: c.sameSite as CookieData["sameSite"],
    }));

    const localStorage: LocalStorageEntry[] = [];
    for (const origin of pwState.origins) {
      for (const item of origin.localStorage) {
        localStorage.push({
          origin: origin.origin,
          key: item.name,
          value: item.value,
        });
      }
    }

    this.lastState = {
      cookies,
      localStorage,
      captured_at: new Date().toISOString(),
    };

    if ((this.config.autoSave ?? true) && this.config.statePath) {
      await this.saveToFile(this.config.statePath);
    }

    return this.lastState;
  }

  // --- CDP Mode ---

  /**
   * Capture storage state via CDP.
   * Gets cookies via Network.getAllCookies and localStorage via Runtime.evaluate.
   */
  async captureFromCdp(cdp: CdpChannel): Promise<StorageState> {
    await cdp.enableDomain("Network");

    // Get cookies
    const cookieResult = await cdp.send<{ cookies: CdpCookie[] }>("Network.getAllCookies");
    const cookies: CookieData[] = (cookieResult.cookies ?? []).map((c) => ({
      name: c.name,
      value: c.value,
      domain: c.domain,
      path: c.path,
      expires: c.expires,
      httpOnly: c.httpOnly,
      secure: c.secure,
      sameSite: (c.sameSite ?? "None") as CookieData["sameSite"],
    }));

    // Get localStorage for current origin
    let localStorage: LocalStorageEntry[] = [];
    try {
      const lsData = await cdp.evaluate<Record<string, string>>(
        `(() => {
          const data = {};
          for (let i = 0; i < localStorage.length; i++) {
            const key = localStorage.key(i);
            if (key) data[key] = localStorage.getItem(key) ?? "";
          }
          return data;
        })()`,
      );
      const origin = await cdp.evaluate<string>("location.origin");
      localStorage = Object.entries(lsData).map(([key, value]) => ({
        origin,
        key,
        value,
      }));
    } catch {
      // localStorage may not be accessible (e.g., about:blank)
    }

    this.lastState = {
      cookies,
      localStorage,
      captured_at: new Date().toISOString(),
    };

    if ((this.config.autoSave ?? true) && this.config.statePath) {
      await this.saveToFile(this.config.statePath);
    }

    return this.lastState;
  }

  /**
   * Restore storage state via CDP.
   * Sets cookies via Network.setCookies and localStorage via Runtime.evaluate.
   */
  async restoreToCdp(cdp: CdpChannel, state?: StorageState): Promise<void> {
    const toRestore = state ?? this.lastState;
    if (!toRestore) return;

    await cdp.enableDomain("Network");

    // Restore cookies
    if (toRestore.cookies.length > 0) {
      await cdp.send("Network.setCookies", {
        cookies: toRestore.cookies.map((c) => ({
          name: c.name,
          value: c.value,
          domain: c.domain,
          path: c.path,
          expires: c.expires > 0 ? c.expires : undefined,
          httpOnly: c.httpOnly,
          secure: c.secure,
          sameSite: c.sameSite,
        })),
      });
    }

    // Restore localStorage (grouped by origin)
    const byOrigin = new Map<string, LocalStorageEntry[]>();
    for (const entry of toRestore.localStorage) {
      const list = byOrigin.get(entry.origin) ?? [];
      list.push(entry);
      byOrigin.set(entry.origin, list);
    }

    // Can only restore localStorage for the current origin via evaluate
    try {
      const currentOrigin = await cdp.evaluate<string>("location.origin");
      const entries = byOrigin.get(currentOrigin);
      if (entries && entries.length > 0) {
        const serialized = JSON.stringify(
          Object.fromEntries(entries.map((e) => [e.key, e.value])),
        );
        await cdp.evaluate(
          `(() => {
            const data = ${serialized};
            for (const [k, v] of Object.entries(data)) {
              localStorage.setItem(k, v);
            }
          })()`,
        );
      }
    } catch {
      // May fail on about:blank or restricted pages
    }
  }

  // --- File persistence ---

  /** Save current state to a JSON file. */
  async saveToFile(filePath: string): Promise<void> {
    if (!this.lastState) return;
    await fs.writeFile(filePath, JSON.stringify(this.lastState, null, 2), "utf-8");
  }

  /** Load state from a JSON file. */
  async loadFromFile(filePath: string): Promise<StorageState | null> {
    try {
      const data = await fs.readFile(filePath, "utf-8");
      this.lastState = JSON.parse(data);
      return this.lastState;
    } catch {
      return null;
    }
  }

  /** Auto-restore from file if configured. */
  async autoRestore(cdp: CdpChannel): Promise<boolean> {
    if (!(this.config.autoRestore ?? true) || !this.config.statePath) return false;
    const state = await this.loadFromFile(this.config.statePath);
    if (!state) return false;
    await this.restoreToCdp(cdp, state);
    return true;
  }

  /** Get the last captured state. */
  get state(): StorageState | null {
    return this.lastState;
  }

  /** Number of cookies in the current state. */
  get cookieCount(): number {
    return this.lastState?.cookies.length ?? 0;
  }
}

/** CDP cookie format. */
interface CdpCookie {
  name: string;
  value: string;
  domain: string;
  path: string;
  expires: number;
  httpOnly: boolean;
  secure: boolean;
  sameSite?: string;
}
