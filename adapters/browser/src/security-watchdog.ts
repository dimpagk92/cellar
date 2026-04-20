/**
 * Security Watchdog — domain allowlist/blocklist enforcement.
 *
 * Prevents the agent from navigating to unintended domains (phishing, ads,
 * malicious sites). Browser-use has a SecurityWatchdog that enforces domain
 * restrictions. This is essential for production deployments where the agent
 * operates on real user accounts.
 *
 * Works at the navigation level — checks URLs before they execute.
 * Can also intercept in-page navigations via CDP Page.frameNavigated events.
 *
 * License: MIT
 */

import type { CdpChannel } from "./cdp-channel.js";
import type { Page } from "playwright";

export interface SecurityEvent {
  type: "blocked" | "allowed" | "warning";
  url: string;
  reason: string;
  timestamp_ms: number;
}

export interface SecurityWatchdogConfig {
  /** Allowed domains (if set, only these domains can be navigated to). */
  allowedDomains?: string[];
  /** Blocked domains (always blocked, even if in allowedDomains). */
  blockedDomains?: string[];
  /** Block data: and javascript: URLs (default: true). */
  blockDangerousSchemes?: boolean;
  /** Block navigation to about:blank (default: false). */
  blockAboutBlank?: boolean;
  /** Maximum events to buffer. */
  maxEvents?: number;
}

export class SecurityWatchdog {
  private events: SecurityEvent[] = [];
  private config: SecurityWatchdogConfig;
  private attached = false;
  private maxEvents: number;

  constructor(config: SecurityWatchdogConfig = {}) {
    this.config = config;
    this.maxEvents = config.maxEvents ?? 50;
  }

  /**
   * Check if a URL is allowed before navigating.
   * Returns null if allowed, or a reason string if blocked.
   */
  checkUrl(url: string): string | null {
    // Block dangerous schemes
    if (this.config.blockDangerousSchemes ?? true) {
      if (url.startsWith("javascript:")) {
        return "javascript: URLs are blocked for security";
      }
      if (url.startsWith("data:text/html")) {
        return "data:text/html URLs are blocked for security";
      }
    }

    // about:blank check
    if (url === "about:blank" && this.config.blockAboutBlank) {
      return "about:blank navigation blocked";
    }

    let hostname: string;
    try {
      hostname = new URL(url).hostname;
    } catch {
      return `Invalid URL: ${url.slice(0, 100)}`;
    }

    // Check blocklist first (takes priority)
    if (this.config.blockedDomains?.length) {
      for (const blocked of this.config.blockedDomains) {
        if (this.domainMatches(hostname, blocked)) {
          return `Domain ${hostname} is in blocklist`;
        }
      }
    }

    // Check allowlist (if set, only allowed domains pass)
    if (this.config.allowedDomains?.length) {
      const isAllowed = this.config.allowedDomains.some((allowed) =>
        this.domainMatches(hostname, allowed),
      );
      if (!isAllowed) {
        return `Domain ${hostname} is not in allowlist`;
      }
    }

    return null; // Allowed
  }

  /**
   * Validate and record a navigation attempt.
   * Returns true if allowed, false if blocked.
   */
  validateNavigation(url: string): boolean {
    const reason = this.checkUrl(url);
    if (reason) {
      this.addEvent({
        type: "blocked",
        url: url.slice(0, 200),
        reason,
        timestamp_ms: Date.now(),
      });
      return false;
    }
    this.addEvent({
      type: "allowed",
      url: url.slice(0, 200),
      reason: "passed security check",
      timestamp_ms: Date.now(),
    });
    return true;
  }

  /** Attach to CDP to monitor in-page navigations. */
  async attachCdp(cdp: CdpChannel): Promise<void> {
    if (this.attached) return;

    // Register listener BEFORE enabling domain to avoid missing events
    cdp.on("Page.frameNavigated", (params) => {
      const frame = params.frame as { url?: string } | undefined;
      if (frame?.url) {
        const reason = this.checkUrl(frame.url);
        if (reason) {
          this.addEvent({
            type: "warning",
            url: frame.url.slice(0, 200),
            reason: `In-page navigation to blocked domain: ${reason}`,
            timestamp_ms: Date.now(),
          });
          // Note: we can't block in-page navigations retroactively via CDP,
          // but we log them as warnings so the agent knows
        }
      }
    });

    await cdp.enableDomain("Page");
    this.attached = true;
  }

  /** Attach to a Playwright Page to monitor navigations. */
  attach(page: Page): void {
    if (this.attached) return;

    page.on("framenavigated", (frame) => {
      const url = frame.url();
      if (url && url !== "about:blank") {
        const reason = this.checkUrl(url);
        if (reason) {
          this.addEvent({
            type: "warning",
            url: url.slice(0, 200),
            reason: `In-page navigation to blocked domain: ${reason}`,
            timestamp_ms: Date.now(),
          });
        }
      }
    });

    this.attached = true;
  }

  /** Get recent security events. */
  getEvents(): SecurityEvent[] {
    return [...this.events];
  }

  /** Get blocked navigation attempts. */
  getBlocked(): SecurityEvent[] {
    return this.events.filter((e) => e.type === "blocked");
  }

  /** Clear buffered events. */
  clear(): void {
    this.events = [];
  }

  /** Check if a hostname matches a domain pattern (supports wildcards). */
  private domainMatches(hostname: string, pattern: string): boolean {
    // Exact match
    if (hostname === pattern) return true;
    // Subdomain match: "example.com" matches "sub.example.com"
    if (hostname.endsWith(`.${pattern}`)) return true;
    // Wildcard: "*.example.com" matches "sub.example.com"
    if (pattern.startsWith("*.") && hostname.endsWith(pattern.slice(1))) return true;
    return false;
  }

  private addEvent(event: SecurityEvent): void {
    this.events.push(event);
    if (this.events.length > this.maxEvents) {
      this.events = this.events.slice(-this.maxEvents);
    }
  }
}
