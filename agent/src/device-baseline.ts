/**
 * Device Baseline Scanner
 *
 * Scans the system ONCE to detect OS, keyboard layout, shortcuts,
 * installed apps, and screen resolution. Cached for the session.
 * Used by the blind planner so it knows the correct keyboard shortcuts
 * without needing to walk the accessibility tree.
 */

import { execSync } from "child_process";
import * as os from "os";
import type { ContextProvider } from "./interfaces/context-provider.js";

/** Max bytes for shell command output to prevent memory overflow. */
const MAX_OUTPUT_BYTES = 100 * 1024; // 100KB

/** Safe execSync wrapper that bounds output size and catches errors. */
function safeExec(cmd: string, timeoutMs = 2000): string {
  try {
    const result = execSync(cmd, {
      encoding: "utf-8",
      timeout: timeoutMs,
      maxBuffer: MAX_OUTPUT_BYTES,
    });
    return result;
  } catch {
    return "";
  }
}

/** Minimal CEL capability set needed by the baseline scanner. */
type BaselineDeps = Pick<ContextProvider, "listMonitors">;

export interface DeviceBaseline {
  os: "macos" | "windows" | "linux";
  os_version: string;
  keyboard_layout: string;
  locale: string;
  screen_resolution: { width: number; height: number };
  shortcuts: Record<string, string[]>;
  installed_apps: string[];
  dock_apps: string[];
}

let cachedBaseline: DeviceBaseline | null = null;

/** Get or scan the device baseline (cached after first call). */
export function getOrScanBaseline(cel: BaselineDeps): DeviceBaseline {
  if (cachedBaseline) return cachedBaseline;
  cachedBaseline = scanBaseline(cel);
  return cachedBaseline;
}

/** Force a fresh scan (e.g., after system settings change). */
export function resetBaseline(): void {
  cachedBaseline = null;
}

function scanBaseline(cel: BaselineDeps): DeviceBaseline {
  const platform = process.platform;

  if (platform === "darwin") {
    return scanMacOS(cel);
  }

  // Fallback for non-macOS
  return {
    os: platform === "win32" ? "windows" : "linux",
    os_version: os.release(),
    keyboard_layout: "unknown",
    locale: Intl.DateTimeFormat().resolvedOptions().locale,
    screen_resolution: { width: 1920, height: 1080 },
    shortcuts: getDefaultShortcuts(platform === "win32" ? "windows" : "linux"),
    installed_apps: [],
    dock_apps: [],
  };
}

function scanMacOS(cel: BaselineDeps): DeviceBaseline {
  // Screen resolution from CEL
  const monitors = cel.listMonitors();
  const primary = monitors.find((m) => m.is_primary) ?? monitors[0];
  const resolution = primary
    ? { width: primary.width, height: primary.height }
    : { width: 1920, height: 1080 };

  // Keyboard layout
  let keyboardLayout = "US";
  {
    const raw = safeExec("defaults read ~/Library/Preferences/com.apple.HIToolbox AppleCurrentKeyboardLayoutInputSource 2>/dev/null");
    const match = raw.match(/"KeyboardLayout Name"\s*=\s*"?([^";]+)"?/);
    if (match) keyboardLayout = match[1].trim();
  }

  // Locale
  const locale = Intl.DateTimeFormat().resolvedOptions().locale;

  // Keyboard shortcuts from symbolichotkeys
  const shortcuts = detectMacShortcuts();

  // Installed apps
  let installedApps: string[] = [];
  {
    const raw = safeExec("ls /Applications 2>/dev/null");
    installedApps = raw
      .split("\n")
      .filter((l) => l.endsWith(".app"))
      .map((l) => l.replace(".app", ""))
      .slice(0, 200); // Cap at 200 apps
  }

  // Dock apps
  let dockApps: string[] = [];
  {
    const raw = safeExec("defaults read com.apple.dock persistent-apps 2>/dev/null");
    const matches = raw.matchAll(/"file-label"\s*=\s*"([^"]+)"/g);
    dockApps = [...matches].map((m) => m[1]).slice(0, 50); // Cap at 50
  }

  return {
    os: "macos",
    os_version: os.release(),
    keyboard_layout: keyboardLayout,
    locale,
    screen_resolution: resolution,
    shortcuts,
    installed_apps: installedApps,
    dock_apps: dockApps,
  };
}

// Modifier bitmask from Apple symbolichotkeys
const MOD_CMD = 1048576;
const MOD_SHIFT = 131072;
const MOD_OPTION = 524288;
const MOD_CONTROL = 262144;

// Common keycodes
const KEYCODE_MAP: Record<number, string> = {
  49: "Space",
  36: "Return",
  48: "Tab",
  51: "Delete",
  53: "Escape",
  123: "Left",
  124: "Right",
  125: "Down",
  126: "Up",
  // F keys
  122: "F1", 120: "F2", 99: "F3", 118: "F4", 96: "F5",
  97: "F6", 98: "F7", 100: "F8", 101: "F9", 109: "F10",
  103: "F11", 111: "F12",
  // Number keys
  18: "1", 19: "2", 20: "3", 21: "4", 23: "5",
  22: "6", 26: "7", 28: "8", 25: "9", 29: "0",
};

// Symbolic hotkey IDs
const HOTKEY_MAP: Record<number, string> = {
  64: "spotlight",
  60: "mission_control",
  61: "app_windows",
  32: "show_desktop",
  51: "screenshot_full",
  54: "screenshot_selection",
  // 160: "show_launchpad",
};

function detectMacShortcuts(): Record<string, string[]> {
  const shortcuts: Record<string, string[]> = {};

  {
    const raw = safeExec("defaults read com.apple.symbolichotkeys AppleSymbolicHotKeys 2>/dev/null", 3000);
    if (!raw) return getDefaultShortcutsMac();

    // Parse plist text format
    for (const [keyId, name] of Object.entries(HOTKEY_MAP)) {
      // Use [\s\S] instead of [^}] to cross nested braces in plist format
      // Anchor to line boundary to avoid 64 matching 164
      const regex = new RegExp(
        `(?:^|\\n)\\s*${keyId}\\s*=\\s*\\{[\\s\\S]*?enabled\\s*=\\s*(\\d)[\\s\\S]*?parameters\\s*=\\s*\\(\\s*([^)]+)\\)`,
        "m",
      );
      const match = raw.match(regex);
      if (match) {
        const enabled = match[1] === "1";
        if (!enabled) continue;

        const params = match[2].split(",").map((s) => parseInt(s.trim(), 10));
        if (params.length >= 3) {
          const [_charcode, keycode, modifiers] = params;
          const keys: string[] = [];
          if (modifiers & MOD_CMD) keys.push("Cmd");
          if (modifiers & MOD_SHIFT) keys.push("Shift");
          if (modifiers & MOD_OPTION) keys.push("Option");
          if (modifiers & MOD_CONTROL) keys.push("Control");

          const keyName = KEYCODE_MAP[keycode] ?? `key${keycode}`;
          keys.push(keyName);

          shortcuts[name] = keys;
        }
      }
    }
  }

  // Fill in defaults for any missing shortcuts
  return getDefaultShortcutsMac(shortcuts);
}

function getDefaultShortcutsMac(shortcuts: Record<string, string[]> = {}): Record<string, string[]> {
  return {
    spotlight: shortcuts.spotlight ?? ["Cmd", "Space"],
    mission_control: shortcuts.mission_control ?? ["Control", "Up"],
    app_windows: shortcuts.app_windows ?? ["Control", "Down"],
    show_desktop: shortcuts.show_desktop ?? ["F11"],
    screenshot_full: shortcuts.screenshot_full ?? ["Cmd", "Shift", "3"],
    screenshot_selection: shortcuts.screenshot_selection ?? ["Cmd", "Shift", "4"],
    // Standard shortcuts (not in symbolichotkeys — always the same)
    close_window: ["Cmd", "W"],
    quit_app: ["Cmd", "Q"],
    new_window: ["Cmd", "N"],
    save: ["Cmd", "S"],
    undo: ["Cmd", "Z"],
    copy: ["Cmd", "C"],
    paste: ["Cmd", "V"],
    select_all: ["Cmd", "A"],
    find: ["Cmd", "F"],
    app_switcher: ["Cmd", "Tab"],
    ...shortcuts,
  };
}

function getDefaultShortcuts(
  osType: "windows" | "linux",
): Record<string, string[]> {
  if (osType === "windows") {
    return {
      search: ["Win"],
      close_window: ["Alt", "F4"],
      app_switcher: ["Alt", "Tab"],
      find: ["Ctrl", "F"],
      copy: ["Ctrl", "C"],
      paste: ["Ctrl", "V"],
      save: ["Ctrl", "S"],
      undo: ["Ctrl", "Z"],
      select_all: ["Ctrl", "A"],
    };
  }
  return {
    search: ["Super"],
    close_window: ["Alt", "F4"],
    app_switcher: ["Alt", "Tab"],
    find: ["Ctrl", "F"],
    copy: ["Ctrl", "C"],
    paste: ["Ctrl", "V"],
    save: ["Ctrl", "S"],
    undo: ["Ctrl", "Z"],
    select_all: ["Ctrl", "A"],
  };
}
