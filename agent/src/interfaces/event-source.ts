/**
 * EventSource — watchdog event streaming.
 *
 * Abstracts the event polling mechanism from the Cel god class.
 * Only the Cortex needs this — other consumers should use
 * ContextProvider or InputController instead.
 */

import type { CelEvent } from "../types.js";

export interface EventSource {
  /** Start the context watchdog for change detection. */
  startWatchdog(): void;

  /** Poll for watchdog events. Returns events that occurred since last poll. */
  pollEvents(): CelEvent[];

  /** Stop and reset the watchdog. */
  stopWatchdog(): void;
}
