/**
 * Network Tap — captures HTTP request/response events.
 *
 * Supports two modes:
 * 1. Playwright: page.on("request"/"response") events
 * 2. CDP: Network.requestWillBeSent / Network.responseReceived events
 *
 * Provides the network context stream that DOM and vision can't:
 * "POST /api/submit returned 422", "XHR in-flight", etc.
 *
 * License: MIT
 */

import type { Page, Request, Response } from "playwright";
import type { CdpChannel } from "./cdp-channel.js";
// NetworkEvent is ConnectionEvent (TCP-level) but we capture HTTP-level events.
// Using a local type to avoid the mismatch until agent types are updated.
interface NetworkEvent {
  url: string;
  method: string;
  status: number;
  content_type?: string;
  timestamp_ms: number;
}

/** Maximum buffered events. */
const MAX_EVENTS = 50;

/** URL patterns to filter out (noise). */
const NOISE_PATTERNS = [
  /^data:/,
  /^chrome-extension:/,
  /^moz-extension:/,
  /^about:/,
  /^blob:/,
  /\.(js|css|woff2?|ttf|eot|ico|svg|png|jpg|jpeg|gif|webp)(\?|$)/,
];

export class NetworkTap {
  private events: NetworkEvent[] = [];
  private pendingRequests: Map<string, { url: string; method: string; timestamp: number }> = new Map();
  private attached = false;
  private nextRequestId = 0;
  /** Map Playwright request → synthetic ID for correlation. */
  private playwrightRequestIds = new WeakMap<Request, string>();

  /** Start capturing via Playwright page events. */
  attach(page: Page): void {
    if (this.attached) return;

    page.on("request", (request: Request) => {
      const id = `pw-${this.nextRequestId++}`;
      this.playwrightRequestIds.set(request, id);
      this.onRequest(id, request.url(), request.method());
    });

    page.on("response", (response: Response) => {
      const id = this.playwrightRequestIds.get(response.request()) ?? `pw-${this.nextRequestId++}`;
      this.onResponse(
        id,
        response.url(),
        response.request().method(),
        response.status(),
        response.headers()["content-type"],
      );
    });

    page.on("requestfailed", (request: Request) => {
      const id = this.playwrightRequestIds.get(request) ?? `pw-${this.nextRequestId++}`;
      this.onRequestFailed(id, request.url(), request.method());
    });

    this.attached = true;
  }

  /** Start capturing via CDP Network domain events (no Playwright needed). */
  async attachCdp(cdp: CdpChannel): Promise<void> {
    if (this.attached) return;

    // Register listeners BEFORE enabling the domain to avoid missing events
    cdp.on("Network.requestWillBeSent", (params) => {
      const requestId = params.requestId as string;
      const request = params.request as { url: string; method: string } | undefined;
      if (request && requestId) {
        this.onRequest(requestId, request.url, request.method);
      }
    });

    cdp.on("Network.responseReceived", (params) => {
      const requestId = params.requestId as string;
      const response = params.response as {
        url: string;
        status: number;
        headers: Record<string, string>;
      } | undefined;
      if (response) {
        const pending = this.pendingRequests.get(requestId);
        this.onResponse(
          requestId,
          response.url,
          pending?.method ?? "GET",
          response.status,
          response.headers?.["content-type"] ?? response.headers?.["Content-Type"],
        );
      }
    });

    cdp.on("Network.loadingFailed", (params) => {
      const requestId = params.requestId as string;
      const pending = this.pendingRequests.get(requestId);
      if (pending) {
        this.onRequestFailed(requestId, pending.url, pending.method);
      }
    });

    await cdp.enableDomain("Network");
    this.attached = true;
  }

  /** Get buffered network events. */
  getEvents(): NetworkEvent[] {
    return [...this.events];
  }

  /** Clear buffered events. */
  clear(): void {
    this.events = [];
    this.pendingRequests.clear();
  }

  /** Get the number of currently pending (in-flight) requests. */
  getPendingCount(): number {
    return this.pendingRequests.size;
  }

  private onRequest(requestId: string, url: string, method: string): void {
    if (this.isNoise(url)) return;
    this.pendingRequests.set(requestId, {
      url,
      method,
      timestamp: Date.now(),
    });
  }

  private onResponse(
    requestId: string,
    url: string,
    method: string,
    status: number,
    contentType?: string,
  ): void {
    if (this.isNoise(url) && status < 400) return;

    this.pendingRequests.delete(requestId);

    this.addEvent({
      url: this.truncateUrl(url),
      method,
      status,
      content_type: contentType,
      timestamp_ms: Date.now(),
    });
  }

  private onRequestFailed(requestId: string, url: string, method: string): void {
    this.pendingRequests.delete(requestId);

    this.addEvent({
      url: this.truncateUrl(url),
      method,
      status: 0,
      content_type: undefined,
      timestamp_ms: Date.now(),
    });
  }

  private addEvent(event: NetworkEvent): void {
    this.events.push(event);
    if (this.events.length > MAX_EVENTS) {
      this.events = this.events.slice(-MAX_EVENTS);
    }
  }

  private isNoise(url: string): boolean {
    return NOISE_PATTERNS.some((pattern) => pattern.test(url));
  }

  private truncateUrl(url: string): string {
    try {
      const parsed = new URL(url);
      const path = parsed.pathname;
      const search = parsed.search.length > 100
        ? parsed.search.slice(0, 100) + "..."
        : parsed.search;
      return `${parsed.origin}${path}${search}`;
    } catch {
      return url.slice(0, 200);
    }
  }
}
