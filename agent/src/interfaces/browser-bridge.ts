/**
 * BrowserBridge — Chrome DevTools Protocol operations.
 *
 * Abstracts all CDP operations from the Cel god class.
 * Consumers that need browser automation should depend on this
 * interface, not the full Cel class.
 */

import type { PageContent, HttpEvent } from "../types.js";

export interface BrowserBridge {
  /** Get page content from CDP if available. */
  getCdpPageContent(): Promise<PageContent | null>;

  /** Navigate the focused browser tab to a URL. */
  cdpNavigate(url: string): Promise<void>;

  /** Execute JavaScript in the focused browser tab via CDP. */
  cdpEvaluate(expression: string): Promise<unknown>;

  /** Get all cookies from the focused browser tab. */
  cdpGetCookies(): Promise<unknown[]>;

  /** Get a localStorage value from the focused browser tab. */
  cdpGetLocalStorage(key: string): Promise<string | null>;

  /** Get recent HTTP requests from the focused browser tab. */
  cdpGetNetworkRequests(limit?: number): Promise<HttpEvent[]>;

  /** Discover CDP targets on this machine. */
  discoverCdpTargets(): Array<{
    app_name: string;
    pid: number;
    port: number;
    ws_url: string;
  }>;

  /** Check if CDP setup (LaunchAgent) is installed. */
  isCdpSetup(): boolean;
}
