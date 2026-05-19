/**
 * CDP Client — hybrid Playwright + raw CDP browser access.
 *
 * Uses Playwright for lifecycle management (launch, connect, contexts)
 * and CdpChannel for the hot path (evaluate, input, network).
 *
 * Two connection modes:
 * 1. Playwright-managed: launch/connect browser, extract CDP session
 * 2. Direct CDP: connect via raw WebSocket (no Playwright dependency)
 *
 * License: MIT
 */

import { chromium, firefox, webkit } from "playwright";
import type {
  Browser,
  BrowserContext,
  Page,
  BrowserType,
} from "playwright";
import { CdpChannel } from "./cdp-channel.js";
import { CLOSED_SHADOW_PATCH } from "./dom-extractor.js";

export interface CdpClientConfig {
  browser: "chromium" | "firefox" | "webkit";
  /** WebSocket endpoint to connect via Playwright's connectOverCDP. */
  wsEndpoint?: string;
  /** Raw CDP WebSocket URL — bypasses Playwright entirely. */
  cdpUrl?: string;
  headless?: boolean;
  viewport?: { width: number; height: number };
  /** Extra Chromium launch arguments. */
  args?: string[];
  /** User data directory — enables persistent context (required for extensions). */
  userDataDir?: string;
  /** Playwright channel — use "chrome" for system Chrome instead of bundled Chromium. */
  channel?: string;
  /** Custom user agent string. */
  userAgent?: string;
  /** Enable stealth/anti-detection mode. */
  stealth?: boolean;
  /** Callback invoked after a successful session recovery. */
  onRecovery?: (reason: string) => void;
}

export class CdpClient {
  private browser: Browser | null = null;
  private context: BrowserContext | null = null;
  private _page: Page | null = null;
  private _cdp: CdpChannel;
  private config: CdpClientConfig;
  private _directCdp = false;
  private _recovering = false;

  constructor(config: CdpClientConfig) {
    this.config = config;
    this._cdp = new CdpChannel();
  }

  /**
   * The Playwright Page — available when using Playwright-managed connection.
   * Throws in direct CDP mode; use `cdp` instead.
   */
  get page(): Page {
    if (!this._page) throw new Error("CdpClient not connected (or in direct CDP mode — use cdp channel)");
    return this._page;
  }

  /** The raw CDP channel — available in ALL connection modes. */
  get cdp(): CdpChannel {
    return this._cdp;
  }

  /** Whether the browser adapter has a Playwright Page (for backwards compat). */
  get hasPage(): boolean {
    return this._page !== null && !this._page.isClosed();
  }

  get isConnected(): boolean {
    if (this._directCdp) return this._cdp.connected;
    return this._page !== null && !this._page.isClosed();
  }

  /** Whether we're in direct CDP mode (no Playwright). */
  get isDirectCdp(): boolean {
    return this._directCdp;
  }

  /** Connect to an existing browser or launch a new one. */
  async connect(): Promise<void> {
    // Mode 1: Direct CDP — no Playwright at all
    if (this.config.cdpUrl) {
      await this._cdp.connectViaWebSocket(this.config.cdpUrl);
      // Track URL via frameNavigated events (no JS evaluation needed)
      this._cdp.on("Page.frameNavigated", (params) => {
        const frame = params.frame as { url?: string; parentId?: string } | undefined;
        // Only track top-level frame (no parentId)
        if (frame?.url && !frame.parentId) {
          this._lastKnownUrl = frame.url;
        }
      });
      await this._cdp.enableDomain("Page");
      await this._cdp.enableDomain("Runtime");
      this._directCdp = true;
      return;
    }

    // Mode 2: Playwright-managed lifecycle + CDP channel for hot path
    const browserType = this.getBrowserType();

    // Stealth: merge anti-detection args and user agent
    const stealth = this.config.stealth ?? false;
    const stealthArgs = stealth ? [
      "--headless=new",
      "--disable-blink-features=AutomationControlled",
      "--disable-dev-shm-usage",
      "--no-sandbox",
      "--disable-infobars",
      "--disable-background-timer-throttling",
      "--disable-backgrounding-occluded-windows",
      "--disable-renderer-backgrounding",
      "--disable-popup-blocking",
      "--disable-ipc-flooding-protection",
      "--password-store=basic",
      "--use-mock-keychain",
    ] : [];
    const mergedArgs = [...stealthArgs, ...(this.config.args ?? [])];
    // Stealth: headless=false tells Playwright not to add its own --headless flag
    // (we use --headless=new via args instead for proper Chrome UA)
    const headless = stealth ? false : (this.config.headless ?? false);
    const isMac = process.platform === "darwin";
    const userAgent = this.config.userAgent ?? (stealth
      ? isMac
        ? "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36"
        : "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36"
      : undefined);

    if (this.config.wsEndpoint) {
      this.browser = await browserType.connectOverCDP(
        this.config.wsEndpoint,
      );
      const contexts = this.browser.contexts();
      this.context = contexts[0] ?? (await this.browser.newContext());
      const pages = this.context.pages();
      this._page = pages[0] ?? (await this.context.newPage());
    } else if (this.config.userDataDir) {
      // Persistent context mode — required for browser extensions.
      this.context = await browserType.launchPersistentContext(
        this.config.userDataDir,
        {
          headless,
          viewport: this.config.viewport ?? { width: 1280, height: 800 },
          args: mergedArgs.length > 0 ? mergedArgs : undefined,
          ...(this.config.channel ? { channel: this.config.channel } : {}),
          ...(userAgent ? { userAgent } : {}),
        },
      );
      this.browser = null;
      const pages = this.context.pages();
      this._page = pages[0] ?? (await this.context.newPage());
    } else {
      this.browser = await browserType.launch({
        headless,
        args: mergedArgs.length > 0 ? mergedArgs : undefined,
      });
      this.context = await this.browser.newContext({
        viewport: this.config.viewport ?? { width: 1280, height: 800 },
        ...(userAgent ? { userAgent } : {}),
      });
      this._page = await this.context.newPage();
    }

    // Extract CDP session from Playwright (Chromium only)
    if (this.config.browser === "chromium" && this._page) {
      const session = await this._page.context().newCDPSession(this._page);
      this._cdp.connectViaSession(session);
    }
  }

  /** Disconnect and close browser. */
  async disconnect(): Promise<void> {
    await this._cdp.disconnect();

    if (this.browser) {
      await this.browser.close().catch(() => {});
      this.browser = null;
    } else if (this.context) {
      // Persistent context mode — close the context directly
      await this.context.close().catch(() => {});
    }
    this.context = null;
    this._page = null;
    this._directCdp = false;
  }

  /**
   * Attempt to recover the browser session after a disconnection.
   *
   * Recovery strategy (in order of escalation):
   * 1. If page is closed but context exists → open a new page from context
   * 2. If context is gone but browser exists → create new context + page
   * 3. Re-attach CDP session after recovery
   * 4. Re-inject the closed shadow DOM patch
   */
  private async recoverSession(reason: string): Promise<void> {
    if (this._recovering || this._directCdp) throw new Error("Cannot recover: " + reason);
    this._recovering = true;

    try {
      // Case 1: page closed but context still alive
      if (this.context && (!this._page || this._page.isClosed())) {
        this._page = await this.context.newPage();
      }
      // Case 2: context gone but browser alive
      else if (this.browser && !this.context) {
        this.context = await this.browser.newContext({
          viewport: this.config.viewport ?? { width: 1280, height: 800 },
        });
        this._page = await this.context.newPage();
      }
      // Nothing to recover from
      else if (!this._page || this._page.isClosed()) {
        throw new Error("No browser or context available for recovery");
      }

      // Re-attach CDP session (Chromium only)
      if (this.config.browser === "chromium" && this._page) {
        await this._cdp.disconnect();
        const session = await this._page.context().newCDPSession(this._page);
        this._cdp.connectViaSession(session);
      }

      // Re-inject closed shadow DOM patch
      try {
        if (this._cdp.connected) {
          await this._cdp.send("Page.addScriptToEvaluateOnNewDocument", {
            source: CLOSED_SHADOW_PATCH,
          });
          await this._cdp.evaluate(CLOSED_SHADOW_PATCH);
        } else if (this._page) {
          await this._page.addInitScript(CLOSED_SHADOW_PATCH);
          await this._page.evaluate(CLOSED_SHADOW_PATCH);
        }
      } catch {
        // Best-effort — patch is an optimization
      }

      this.config.onRecovery?.(reason);
    } finally {
      this._recovering = false;
    }
  }

  /** Check if an error indicates a disconnected/crashed session. */
  private isDisconnectionError(error: unknown): boolean {
    const msg = String(error);
    return (
      msg.includes("Target closed") ||
      msg.includes("Session closed") ||
      msg.includes("Protocol error") ||
      msg.includes("Target crashed") ||
      msg.includes("Page closed") ||
      msg.includes("Navigation interrupted") ||
      msg.includes("Execution context was destroyed")
    );
  }

  /** Errors that commonly happen mid-navigation and are worth retrying. */
  private isTransientNavigationError(error: unknown): boolean {
    const msg = String(error);
    return (
      msg.includes("Execution context was destroyed") ||
      msg.includes("Target page, context or browser has been closed") ||
      msg.includes("Cannot find context with specified id") ||
      msg.includes("Navigation interrupted")
    );
  }

  private async retryDuringNavigation<T>(
    fn: () => Promise<T>,
    attempts = 3,
    delayMs = 1500,
  ): Promise<T> {
    let lastError: unknown;
    for (let attempt = 0; attempt < attempts; attempt++) {
      try {
        return await fn();
      } catch (error) {
        lastError = error;
        if (!this.isTransientNavigationError(error) || attempt === attempts - 1) {
          throw error;
        }
        await new Promise((resolve) => setTimeout(resolve, delayMs));
      }
    }
    throw lastError;
  }

  /**
   * Evaluate JavaScript in the page context.
   * Uses CDP Runtime.evaluate when available (Chromium), falls back to Playwright.
   * Attempts session recovery once on disconnection errors.
   */
  async evaluate<T>(expression: string): Promise<T> {
    try {
      if (this._cdp.connected) {
        return await this._cdp.evaluate<T>(expression);
      }
      return await (this.page.evaluate(expression) as Promise<T>);
    } catch (error) {
      // Attempt recovery once on disconnection errors
      if (!this._recovering && !this._directCdp && this.isDisconnectionError(error)) {
        await this.recoverSession(String(error));
        // Retry after recovery
        if (this._cdp.connected) {
          return this._cdp.evaluate<T>(expression);
        }
        return this.page.evaluate(expression) as Promise<T>;
      }
      throw error;
    }
  }

  /** Evaluate a function in the page context (Playwright only). */
  async evaluateHandle<T>(
    fn: (...args: unknown[]) => T,
    ...args: unknown[]
  ): Promise<T> {
    return this.page.evaluate(fn, ...args) as Promise<T>;
  }

  /** Get current page title. */
  async getPageTitle(): Promise<string> {
    if (this._directCdp) {
      return this.retryDuringNavigation(() => this._cdp.evaluate<string>("document.title"));
    }
    return this.retryDuringNavigation(() => this.page.title());
  }

  private _lastKnownUrl = "";

  /** Get current page URL (returns cached URL in direct CDP mode). */
  getPageUrl(): string {
    if (this._directCdp) {
      return this._lastKnownUrl;
    }
    return this.page.url();
  }

  /** Async page URL — works in all modes. Caches result for sync access. */
  async getPageUrlAsync(): Promise<string> {
    if (this._directCdp) {
      this._lastKnownUrl = await this.retryDuringNavigation(
        () => this._cdp.evaluate<string>("location.href"),
      );
      return this._lastKnownUrl;
    }
    return this.page.url();
  }

  /**
   * Navigate to a URL. Optional `waitUntil` / `timeout` flow through to
   * Playwright's `page.goto`. The direct-CDP branch only honours `timeout`
   * (it always waits on `Page.loadEventFired`, which corresponds to
   * `waitUntil: "load"`).
   */
  async navigate(
    url: string,
    options?: {
      waitUntil?: "load" | "domcontentloaded" | "networkidle" | "commit";
      timeout?: number;
    },
  ): Promise<void> {
    const timeout = options?.timeout ?? 10_000;
    if (this._directCdp) {
      await this._cdp.navigate(url);
      // Wait for the page load event (or timeout)
      await new Promise<void>((resolve) => {
        const handler = () => {
          this._cdp.off("Page.loadEventFired", handler);
          resolve();
        };
        this._cdp.on("Page.loadEventFired", handler);
        setTimeout(() => {
          this._cdp.off("Page.loadEventFired", handler);
          resolve();
        }, timeout);
      });
      return;
    }
    await this.page.goto(url, {
      waitUntil: options?.waitUntil ?? "domcontentloaded",
      timeout,
    });
  }

  /** Take a screenshot as a Buffer. */
  async screenshot(): Promise<Buffer> {
    if (this._cdp.connected) {
      const base64 = await this._cdp.screenshot("png");
      return Buffer.from(base64, "base64");
    }
    return this.page.screenshot({ type: "png" }) as Promise<Buffer>;
  }

  /** Get all iframe pages for cross-origin extraction. */
  async getIframePages(): Promise<Array<{ page: Page; origin: string }>> {
    if (this._directCdp) return []; // Cross-origin iframes need Playwright's frame API
    const frames = this.page.frames();
    const results: Array<{ page: Page; origin: string }> = [];
    for (const frame of frames) {
      if (frame === this.page.mainFrame()) continue;
      try {
        const url = frame.url();
        if (url && url !== "about:blank") {
          results.push({
            page: this._page!,
            origin: new URL(url).origin,
          });
        }
      } catch {
        // Skip inaccessible frames
      }
    }
    return results;
  }

  private getBrowserType(): BrowserType {
    switch (this.config.browser) {
      case "chromium":
        return chromium;
      case "firefox":
        return firefox;
      case "webkit":
        return webkit;
      default:
        return chromium;
    }
  }
}
