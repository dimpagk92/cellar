import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

/**
 * Result of an `ensureFrontmost` call. Lets keystroke-based callers audit
 * whether the system focus was already where they wanted it (no-op) or had
 * to be moved (`activated: true`), and how long the activation cost.
 */
export interface EnsureFrontmostResult {
  /** App name reported by `System Events` after the call. */
  frontmost: string;
  /** Whether `frontmost` matches the requested target after the call. */
  matchesTarget: boolean;
  /** True if we had to call `activate` (i.e. the target was not already frontmost). */
  activated: boolean;
  /** Wall-clock time spent inside ensureFrontmost in milliseconds. */
  elapsedMs: number;
  /** App that was frontmost when we entered (before any activation). */
  previousFrontmost: string;
}

/**
 * macOS only. Returns the app name of the currently-frontmost process.
 * Exposed so other tools (e.g. cel_perceive feed) can audit where an event
 * actually landed when the cortex's diff says nothing visibly changed.
 */
export async function getFrontmost(): Promise<string> {
  const { stdout } = await execFileAsync("osascript", [
    "-e",
    'tell application "System Events" to get name of first application process whose frontmost is true',
  ]);
  return stdout.trim();
}

/**
 * Activate `targetApp` and return immediately; the OS schedules the focus
 * switch asynchronously, so callers must poll with `getFrontmost` to know
 * when it has actually landed.
 */
async function activate(targetApp: string): Promise<void> {
  // `osascript -e 'tell application "X" to activate'` is the canonical
  // foreground-an-app primitive on macOS — works for both running and
  // not-yet-launched apps. We use this rather than NSWorkspace bindings to
  // avoid pulling in a native helper from a TS-only layer.
  await execFileAsync("osascript", [
    "-e",
    `tell application "${targetApp.replace(/"/g, '\\"')}" to activate`,
  ]);
}

/**
 * Ensure `targetApp` is macOS-frontmost before the caller fires a
 * focus-sensitive action (typically a keystroke via CGEventPost, which
 * routes to whichever app has system focus).
 *
 * `cel_act type` / `key_press` / `key_combo` use the global keyboard event
 * path (enigo → CGEventPost), so any focus oscillation between when the
 * caller queried the screen and when the keystroke fires can land
 * characters in the wrong app. This helper closes that race.
 *
 * The contract:
 * - If `targetApp` is already frontmost, return immediately (no-op).
 * - Otherwise activate it and poll until `System Events` reports it as
 *   frontmost or the timeout expires.
 * - On timeout, return with `matchesTarget: false` rather than throwing —
 *   the caller decides whether to abort (recommended) or proceed.
 *
 * Polling cadence: 25 ms — short enough that a typical sub-100 ms focus
 * shift on a hot system completes in 2–4 polls, long enough that we don't
 * burn CPU when the OS is busy.
 *
 * @param targetApp App name (e.g. `"Finder"`, `"Numbers"`, `"Google Chrome"`)
 *   as reported by `System Events`. Bundle IDs are NOT supported — pass the
 *   user-visible name.
 * @param timeoutMs Maximum time to wait for activation to land. Default
 *   1500 ms, which is generous for cold launches; pass a smaller value
 *   (e.g. 250 ms) when you know the app is already running.
 */
export async function ensureFrontmost(
  targetApp: string,
  timeoutMs: number = 1500,
): Promise<EnsureFrontmostResult> {
  const start = Date.now();
  const previousFrontmost = await getFrontmost();

  if (previousFrontmost === targetApp) {
    return {
      frontmost: previousFrontmost,
      matchesTarget: true,
      activated: false,
      elapsedMs: Date.now() - start,
      previousFrontmost,
    };
  }

  await activate(targetApp);

  const deadline = start + timeoutMs;
  let frontmost = previousFrontmost;
  while (Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 25));
    frontmost = await getFrontmost();
    if (frontmost === targetApp) {
      return {
        frontmost,
        matchesTarget: true,
        activated: true,
        elapsedMs: Date.now() - start,
        previousFrontmost,
      };
    }
  }

  return {
    frontmost,
    matchesTarget: false,
    activated: true,
    elapsedMs: Date.now() - start,
    previousFrontmost,
  };
}
