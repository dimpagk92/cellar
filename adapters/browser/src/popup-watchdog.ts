/**
 * Popup Watchdog — auto-handles JavaScript dialogs (alert, confirm, prompt).
 *
 * JS dialogs block ALL browser automation until dismissed. Without this,
 * any page that triggers alert() will stall the agent indefinitely.
 * Browser-use added this as a dedicated watchdog for the same reason.
 *
 * Supports both Playwright (page.on('dialog')) and CDP (Page.javascriptDialogOpening).
 *
 * License: MIT
 */

import type { Page, Dialog } from "playwright";
import type { CdpChannel } from "./cdp-channel.js";

export interface PopupEvent {
  type: "alert" | "confirm" | "prompt" | "beforeunload";
  message: string;
  action: "accepted" | "dismissed";
  timestamp_ms: number;
}

export interface PopupWatchdogConfig {
  /** Auto-accept confirm() dialogs (default: true). */
  autoAcceptConfirm?: boolean;
  /** Default text for prompt() dialogs (default: ""). */
  defaultPromptText?: string;
  /** Auto-dismiss beforeunload dialogs (default: true). */
  autoDismissBeforeUnload?: boolean;
  /** Maximum events to buffer. */
  maxEvents?: number;
}

export class PopupWatchdog {
  private events: PopupEvent[] = [];
  private config: PopupWatchdogConfig;
  private attached = false;
  private maxEvents: number;

  constructor(config: PopupWatchdogConfig = {}) {
    this.config = config;
    this.maxEvents = config.maxEvents ?? 20;
  }

  /** Attach to a Playwright Page. */
  attach(page: Page): void {
    if (this.attached) return;

    page.on("dialog", async (dialog: Dialog) => {
      await this.handleDialog(
        dialog.type() as PopupEvent["type"],
        dialog.message(),
        (accept, text) => accept ? dialog.accept(text) : dialog.dismiss(),
      );
    });

    this.attached = true;
  }

  /** Attach to a CDP channel. */
  async attachCdp(cdp: CdpChannel): Promise<void> {
    if (this.attached) return;

    // Register listener BEFORE enabling domain to avoid missing events
    cdp.on("Page.javascriptDialogOpening", async (params) => {
      const type = params.type as PopupEvent["type"];
      const message = (params.message as string) ?? "";

      await this.handleDialog(type, message, async (accept, text) => {
        await cdp.send("Page.handleJavaScriptDialog", {
          accept,
          promptText: text,
        });
      });
    });

    await cdp.enableDomain("Page");
    this.attached = true;
  }

  /** Handle a dialog regardless of source. */
  private async handleDialog(
    type: PopupEvent["type"],
    message: string,
    respond: (accept: boolean, text?: string) => Promise<void>,
  ): Promise<void> {
    let action: "accepted" | "dismissed";

    switch (type) {
      case "alert":
        // Alerts only have an OK button — always accept
        await respond(true);
        action = "accepted";
        break;

      case "confirm":
        if (this.config.autoAcceptConfirm ?? true) {
          await respond(true);
          action = "accepted";
        } else {
          await respond(false);
          action = "dismissed";
        }
        break;

      case "prompt":
        await respond(true, this.config.defaultPromptText ?? "");
        action = "accepted";
        break;

      case "beforeunload":
        if (this.config.autoDismissBeforeUnload ?? true) {
          // Stay on page — dismiss the "are you sure" dialog
          await respond(false);
          action = "dismissed";
        } else {
          await respond(true);
          action = "accepted";
        }
        break;

      default:
        await respond(true);
        action = "accepted";
    }

    this.addEvent({ type, message, action, timestamp_ms: Date.now() });
  }

  /** Get recent popup events. */
  getEvents(): PopupEvent[] {
    return [...this.events];
  }

  /** Whether any dialogs have been handled since last clear. */
  get hasActivity(): boolean {
    return this.events.length > 0;
  }

  /** Clear buffered events. */
  clear(): void {
    this.events = [];
  }

  private addEvent(event: PopupEvent): void {
    this.events.push(event);
    if (this.events.length > this.maxEvents) {
      this.events = this.events.slice(-this.maxEvents);
    }
  }
}
