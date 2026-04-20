/**
 * Download Watchdog — detects and manages file downloads.
 *
 * Without this, when a click triggers a file download instead of navigation,
 * the agent waits for a page load that never comes and eventually times out.
 * Browser-use added this as a dedicated watchdog (DownloadsWatchdog) with
 * save_as_pdf support.
 *
 * Supports both Playwright (page.on('download')) and CDP (Page.downloadWillBegin).
 *
 * License: MIT
 */

import type { Page, Download } from "playwright";
import type { CdpChannel } from "./cdp-channel.js";
import * as path from "path";

export interface DownloadEvent {
  /** Suggested filename from the server. */
  filename: string;
  /** Full URL that triggered the download. */
  url: string;
  /** Where the file was saved (after completion). */
  savedPath?: string;
  /** Download status. */
  status: "started" | "completed" | "failed";
  timestamp_ms: number;
}

export interface DownloadWatchdogConfig {
  /** Directory to save downloads to. Default: OS temp dir. */
  downloadDir?: string;
  /** Auto-accept downloads (default: true). If false, downloads are cancelled. */
  autoAccept?: boolean;
  /** Maximum events to buffer. */
  maxEvents?: number;
}

export class DownloadWatchdog {
  private events: DownloadEvent[] = [];
  private config: DownloadWatchdogConfig;
  private attached = false;
  private maxEvents: number;
  private _pendingCount = 0;

  constructor(config: DownloadWatchdogConfig = {}) {
    this.config = config;
    this.maxEvents = config.maxEvents ?? 50;
  }

  /** Attach to a Playwright Page. */
  attach(page: Page): void {
    if (this.attached) return;

    page.on("download", async (download: Download) => {
      const filename = download.suggestedFilename();
      const url = download.url();
      this._pendingCount++;

      this.addEvent({
        filename,
        url,
        status: "started",
        timestamp_ms: Date.now(),
      });

      if (this.config.autoAccept ?? true) {
        try {
          const savePath = this.config.downloadDir
            ? path.join(this.config.downloadDir, filename)
            : undefined;

          if (savePath) {
            await download.saveAs(savePath);
          }

          const finalPath = savePath ?? (await download.path());
          this._pendingCount--;

          this.addEvent({
            filename,
            url,
            savedPath: finalPath ?? undefined,
            status: "completed",
            timestamp_ms: Date.now(),
          });
        } catch (e) {
          this._pendingCount--;
          this.addEvent({
            filename,
            url,
            status: "failed",
            timestamp_ms: Date.now(),
          });
        }
      } else {
        await download.cancel();
        this._pendingCount--;
        this.addEvent({
          filename,
          url,
          status: "failed",
          timestamp_ms: Date.now(),
        });
      }
    });

    this.attached = true;
  }

  /** Attach to a CDP channel. */
  async attachCdp(cdp: CdpChannel): Promise<void> {
    if (this.attached) return;

    // Register listeners BEFORE enabling domains to avoid missing events
    cdp.on("Page.downloadWillBegin", (params) => {
      const url = (params.url as string) ?? "";
      const filename = (params.suggestedFilename as string) ?? "download";
      this._pendingCount++;

      this.addEvent({
        filename,
        url,
        status: "started",
        timestamp_ms: Date.now(),
      });

      // In direct CDP mode, we can only detect downloads, not control save path.
      // The download completes in the browser's default download directory.
    });

    cdp.on("Page.downloadProgress", (params) => {
      const state = params.state as string;
      if (state === "completed" || state === "canceled") {
        this._pendingCount = Math.max(0, this._pendingCount - 1);
        // Update the most recent matching event
        const lastStarted = [...this.events]
          .reverse()
          .find((e) => e.status === "started");
        if (lastStarted) {
          this.addEvent({
            filename: lastStarted.filename,
            url: lastStarted.url,
            status: state === "completed" ? "completed" : "failed",
            timestamp_ms: Date.now(),
          });
        }
      }
    });

    await cdp.enableDomain("Page");
    try {
      await cdp.enableDomain("Browser");
    } catch {
      // Browser domain not available in all CDP contexts (e.g. Playwright's CDP session)
    }

    this.attached = true;
  }

  /** Get recent download events. */
  getEvents(): DownloadEvent[] {
    return [...this.events];
  }

  /** Number of downloads currently in progress. */
  get pendingCount(): number {
    return this._pendingCount;
  }

  /** Whether any downloads are in progress. */
  get hasPendingDownloads(): boolean {
    return this._pendingCount > 0;
  }

  /** Get the most recent completed download path (if any). */
  get lastCompletedPath(): string | undefined {
    return [...this.events]
      .reverse()
      .find((e) => e.status === "completed")?.savedPath;
  }

  /** Clear buffered events. */
  clear(): void {
    this.events = [];
  }

  private addEvent(event: DownloadEvent): void {
    this.events.push(event);
    if (this.events.length > this.maxEvents) {
      this.events = this.events.slice(-this.maxEvents);
    }
  }
}
