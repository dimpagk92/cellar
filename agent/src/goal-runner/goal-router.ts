/**
 * Goal Router — LLM-powered classification that routes simple goals to
 * deterministic execution, complex goals to the full planner.
 *
 * Uses Gemini Flash (~100ms, ~$0.0001) for classification.
 */

import type { Planner } from "../interfaces/planner.js";
import type { PlannedAction } from "../types.js";
import type { DeviceBaseline } from "../device-baseline.js";

/** Route types returned by the LLM goal router. */
export interface GoalRoute {
  route: string;
  app?: string;
  query?: string;
  url?: string;
  text?: string;
  keys?: string[][];
  action?: string;
  reason?: string;
  extraction?: {
    mode: "extract_data" | "find_elements" | "full_context";
    target: string;
    css_hints?: string[];
    max_items?: number;
    element_keywords?: string[];
  };
  steps?: GoalRoute[];
}

/** Browser app names — use Cmd+L (address bar) for search, not Cmd+F */
const BROWSER_APPS = new Set(["chrome", "google chrome", "brave", "firefox", "safari", "arc", "edge", "chromium", "opera", "vivaldi"]);

/** Build the "open/activate app" action sequence.
 * Uses activate_app (open -a) which is the most reliable method on macOS.
 * Falls back to Spotlight if activate_app is not available.
 */
export function openAppActions(appName: string, _spotlightKeys?: string[]): PlannedAction[] {
  return [
    { type: "activate_app", app_name: appName } as PlannedAction,
    { type: "wait", ms: 1000 } as PlannedAction,
  ];
}

/**
 * LLM-powered goal router — classifies the goal and extracts parameters.
 * Routes simple goals to deterministic execution, complex goals to the full LLM planner.
 */
export async function routeGoal(
  cel: Pick<Planner, "llmComplete">,
  goal: string,
  baseline: DeviceBaseline | null,
): Promise<{ route: GoalRoute; actions: PlannedAction[] | null }> {
  const spotlightKeys = baseline?.shortcuts?.spotlight ?? ["Cmd", "Space"];
  const shortcuts = baseline?.shortcuts;

  try {
    const shortcutList = shortcuts ? `
Available shortcuts on this device:
- Spotlight/Search: ${JSON.stringify(shortcuts.spotlight)}
- Close window: ${JSON.stringify(shortcuts.close_window)}
- Quit app: ${JSON.stringify(shortcuts.quit_app)}
- New window: ${JSON.stringify(shortcuts.new_window)}
- Save: ${JSON.stringify(shortcuts.save)}
- Copy: ${JSON.stringify(shortcuts.copy)}
- Paste: ${JSON.stringify(shortcuts.paste)}
- Undo: ${JSON.stringify(shortcuts.undo)}
- Screenshot (full): ${JSON.stringify(shortcuts.screenshot_full)}
- Screenshot (selection): ${JSON.stringify(shortcuts.screenshot_selection)}` : "";

    const prompt = `Classify this desktop automation goal. Return ONLY valid JSON.

Goal: "${goal}"
${shortcutList}

Routes (pick the BEST match):

1. "open_app" — Launch/open an application
   {"route": "open_app", "app": "AppName"}

2. "open_and_search" — Open an app AND search within it
   {"route": "open_and_search", "app": "AppName", "query": "search term"}
   NOTE: For browsers (Chrome, Safari, etc.), search means address bar. For Finder, it means file search. For other apps, it means in-app search (Cmd+F).

3. "navigate_url" — Go to a URL in a browser
   {"route": "navigate_url", "url": "https://..."}

4. "search_web" — Search the web for something (not a specific URL)
   {"route": "search_web", "query": "search term"}

5. "keyboard_sequence" — One or more keyboard shortcuts
   {"route": "keyboard_sequence", "keys": [["Cmd", "C"]]}
   Use the EXACT shortcuts from the device list above.

6. "type_text" — Type some text into the current app
   {"route": "type_text", "text": "the text to type"}

7. "close_app" — Close/quit an application or its windows
   {"route": "close_app", "app": "AppName", "action": "close_windows" | "quit"}

8. "open_and_type" — Open an app and type text into it
   {"route": "open_and_type", "app": "AppName", "text": "text to type"}

9. "switch_to_app" — Bring an already-running app to front
   {"route": "switch_to_app", "app": "AppName"}

10. "read_data" — Goal is to READ/EXTRACT data from a web page (prices, headlines, text content). May need navigation first.
    {"route": "read_data", "url": "https://...", "extraction": {"mode": "extract_data", "target": "description of data to extract", "css_hints": ["table tr", ".price"], "max_items": 5}}

11. "multi_step" — Goal requires MULTIPLE sequential operations
    {"route": "multi_step", "steps": [...]}
    Each step must be one of: open_app, navigate_url, search_web, read_data, keyboard_sequence, type_text.

12. "needs_planning" — Complex goal requiring screen context, clicking, or conditional logic
    {"route": "needs_planning", "reason": "explanation", "extraction": {"mode": "find_elements", "element_keywords": ["keyword1", "keyword2"]}}

Rules:
- "search for weather" or "google something" = search_web (use browser address bar)
- "find files" in Finder = open_and_search
- "go to URL" or mentions a website = navigate_url
- "open X and type Y" = open_and_type
- "close all X windows" = close_app with action "close_windows"
- "read prices from X" / "what are the headlines on X" / "extract data from X" = read_data (with extraction hints)
- For read_data: include css_hints like "table tr" for tables, "h1, h2, h3" for headlines, ".price" for prices
- CRITICAL: If the goal mentions "then", "and also", "after that", or involves visiting MULTIPLE websites/apps = multi_step
- If the goal involves CLICKING specific UI elements or CONDITIONAL logic = needs_planning
- For needs_planning: include element_keywords to help filter context
- App names: capitalize properly (Finder, Chrome, Terminal, System Settings)
- When unsure between multi_step and needs_planning, prefer multi_step if the steps are clear

JSON only:`;

    const raw = await cel.llmComplete(prompt, goal, 512);
    const cleaned = raw.replace(/```json?\n?/g, "").replace(/```/g, "").trim();
    const route = JSON.parse(cleaned) as GoalRoute;

    switch (route.route) {
      case "open_app": {
        if (!route.app) return { route, actions: null };
        return { route, actions: openAppActions(route.app, spotlightKeys) };
      }

      case "open_and_search": {
        if (!route.app || !route.query) return { route, actions: null };
        const isBrowser = BROWSER_APPS.has(route.app.toLowerCase());
        const searchShortcut = isBrowser ? ["Cmd", "L"] : ["Cmd", "F"];
        return {
          route,
          actions: [
            ...openAppActions(route.app, spotlightKeys),
            { type: "wait", ms: 1000 } as PlannedAction,
            { type: "key_combo", keys: searchShortcut } as PlannedAction,
            { type: "wait", ms: 300 } as PlannedAction,
            { type: "type", text: route.query } as PlannedAction,
            { type: "key", key: "Enter" } as PlannedAction,
          ],
        };
      }

      case "search_web": {
        if (!route.query) return { route, actions: null };
        return {
          route,
          actions: [
            ...openAppActions("Google Chrome", spotlightKeys),
            { type: "wait", ms: 1000 } as PlannedAction,
            { type: "key_combo", keys: ["Cmd", "L"] } as PlannedAction,
            { type: "wait", ms: 200 } as PlannedAction,
            { type: "type", text: route.query } as PlannedAction,
            { type: "key", key: "Enter" } as PlannedAction,
          ],
        };
      }

      case "navigate_url": {
        if (!route.url) return { route, actions: null };
        return {
          route,
          actions: [
            ...openAppActions("Google Chrome", spotlightKeys),
            { type: "wait", ms: 1000 } as PlannedAction,
            { type: "key_combo", keys: ["Cmd", "L"] } as PlannedAction,
            { type: "wait", ms: 200 } as PlannedAction,
            { type: "type", text: route.url } as PlannedAction,
            { type: "key", key: "Enter" } as PlannedAction,
          ],
        };
      }

      case "type_text": {
        if (!route.text) return { route, actions: null };
        return {
          route,
          actions: [{ type: "type", text: route.text } as PlannedAction],
        };
      }

      case "open_and_type": {
        if (!route.app || !route.text) return { route, actions: null };
        return {
          route,
          actions: [
            ...openAppActions(route.app, spotlightKeys),
            { type: "wait", ms: 1000 } as PlannedAction,
            { type: "key_combo", keys: shortcuts?.new_window ?? ["Cmd", "N"] } as PlannedAction,
            { type: "wait", ms: 500 } as PlannedAction,
            { type: "type", text: route.text } as PlannedAction,
          ],
        };
      }

      case "close_app": {
        if (!route.app) return { route, actions: null };
        const closeKeys = shortcuts?.close_window ?? ["Cmd", "W"];
        const quitKeys = shortcuts?.quit_app ?? ["Cmd", "Q"];
        const actions: PlannedAction[] = [
          ...openAppActions(route.app, spotlightKeys),
          { type: "wait", ms: 500 } as PlannedAction,
        ];
        if (route.action === "quit") {
          actions.push({ type: "key_combo", keys: quitKeys } as PlannedAction);
        } else {
          actions.push({ type: "key_combo", keys: ["Cmd", "Option", "W"] } as PlannedAction);
        }
        return { route, actions };
      }

      case "switch_to_app": {
        if (!route.app) return { route, actions: null };
        return { route, actions: openAppActions(route.app, spotlightKeys) };
      }

      case "keyboard_sequence": {
        if (!route.keys?.length) return { route, actions: null };
        const actions: PlannedAction[] = [];
        for (const keys of route.keys) {
          if (keys.length === 1) {
            actions.push({ type: "key", key: keys[0] } as PlannedAction);
          } else {
            actions.push({ type: "key_combo", keys } as PlannedAction);
          }
          actions.push({ type: "wait", ms: 200 } as PlannedAction);
        }
        return { route, actions };
      }

      case "read_data":
        return { route, actions: null };

      case "multi_step":
        return { route, actions: null };

      case "needs_planning":
      default:
        return { route, actions: null };
    }
  } catch {
    return { route: { route: "needs_planning", reason: "router error" }, actions: null };
  }
}
