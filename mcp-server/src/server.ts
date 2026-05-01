import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { Cel, ensureDedicatedCdpBrowser } from "@cellar/agent";
import { celSeeSchema, handleCelSee } from "./tools/cel-see.js";
import { celActSchema, handleCelAct } from "./tools/cel-act.js";
import { celThinkSchema, handleCelThink } from "./tools/cel-think.js";
import { celPerceiveSchema, handleCelPerceive } from "./tools/cel-perceive.js";

type CdpTargetLike = {
  app_name?: string;
  appName?: string;
  ws_url?: string;
  wsUrl?: string;
};

const BROWSER_CDP_APP_PATTERNS = [
  /google chrome/i,
  /chromium/i,
  /brave/i,
  /microsoft edge/i,
  /arc/i,
  /opera/i,
  /vivaldi/i,
];

export function isBrowserCdpTarget(target: CdpTargetLike): boolean {
  const appName = target.app_name ?? target.appName ?? "";
  return BROWSER_CDP_APP_PATTERNS.some((pattern) => pattern.test(appName));
}

export function filterBrowserCdpTargets<T extends CdpTargetLike>(targets: T[]): T[] {
  return targets.filter((target) => isBrowserCdpTarget(target));
}

export function createCelMcpServer(cel?: Cel): McpServer {
  const instance = cel ?? new Cel();

  if (!instance.isNativeAvailable) {
    throw new Error(
      "CEL native module not available. Make sure the cel-napi binary is built.",
    );
  }

  const server = new McpServer(
    {
      name: "cel",
      version: "0.2.0",
    },
    {
      instructions: [
        "CEL (Computer Experience Layer) gives you native control of the user's macOS desktop.",
        "It reads the screen via the macOS Accessibility API and optionally Chrome DevTools Protocol.",
        "",
        "## Default Workflow: Perceive → See → Act → Think",
        "",
        "For ANY task that will take more than 3 observations, start with `cel_perceive start`.",
        "The Cortex maintains a warm mental model with diffs, stability classification, and",
        "focus-trail tracking. Polling `cel_see context` on every step throws away that signal",
        "and forces you to re-parse the entire AX tree.",
        "",
        "### Recommended loop (multi-step tasks):",
        "1. **cel_perceive start** — Boot Cortex with your goal. enable_suggestions=true (default)",
        "   gives you next-action hints on each read.",
        "2. **cel_perceive read** — Instant mental-model snapshot. Cortex keeps it warm via background events.",
        "3. **cel_act** — Execute actions. Prefer `ax_action` over `click`, `set_value` over `type`,",
        "   `cdp_eval` over coordinate clicks for browser DOM.",
        "4. **cel_perceive feed** — Report action + expected outcome. Cortex verifies via screen diff.",
        "5. **cel_think observe** — Record what worked / what broke. Use priority='high' for anything",
        "   that should change future runs. Feeds `search_knowledge` for later sessions.",
        "6. Repeat 2–5. Use **cel_perceive checkpoint** between task phases.",
        "7. **cel_perceive stop** when done — returns a run summary.",
        "",
        "### Light path (≤3 observations, one-shot task):",
        "1. **cel_see** mode 'context' — get current screen.",
        "2. **cel_act** — do the thing.",
        "3. **cel_see** mode 'context' again — verify.",
        "",
        "If you find yourself on the light path for more than 3 cycles, switch to Cortex.",
        "",
        "## Host-vs-Cortex Planning",
        "",
        "If the MCP host is a strong reasoning model (Claude, GPT-4+), keep PLANNING in the host.",
        "Use `cel_perceive` / `cel_see` / `cel_act` as eyes + hands. Only use `cel_think run_goal`",
        "when you want to fully delegate the loop (fire-and-forget).",
        "",
        "## AX-Hostile Apps — Prefer Structured App Truth",
        "",
        "Some macOS apps render their content via Core Graphics canvases that are invisible to",
        "the accessibility tree. Pure UI automation on these apps is hopeless — you'll see the",
        "toolbar but not the table/cells/chart objects.",
        "",
        "Known AX-hostile apps: **Numbers, Pages, Keynote, Adobe Creative Cloud, most pro audio/video.**",
        "",
        "For these, prefer CEL's deterministic structured actions over raw UI guessing. Numbers",
        "supports `cel_act` actions `write_cells` and `read_cells`, which go through the document",
        "model instead of AX text. Use AX for app/window/dialog navigation, and structured app truth",
        "for the spreadsheet contents themselves.",
        "",
        "Apps that are CEL-friendly (full AX tree): Finder, TextEdit, Mail, Calendar, Safari (+CDP),",
        "Chrome (+CDP), most system settings, most Electron apps.",
        "",
        "## Coordinate System — IMPORTANT",
        "",
        "`cel_act click x,y` takes **screen coordinates** (logical points, 1× on Retina).",
        "`cel_see` returns bounds that on Retina displays may be in **pixel coordinates** (2× on Retina).",
        "If a window appears at x=881 in `cel_see windows` but at x=1762 in `cel_see context` bounds,",
        "you have a Retina mismatch. Divide AX bounds by the monitor scale_factor before clicking,",
        "or use `ax_action` / `make_reference` which don't need coordinates.",
        "",
        "Check `cel_see monitors` for the active scale_factor.",
        "",
        "## Focus Stability",
        "",
        "Keyboard actions (`type`, `key_combo`, `key_press`) go to the **frontmost** app. If the host",
        "process (e.g. Claude Desktop, Codex) is in a full-screen window covering the target app,",
        "a click into the target's pixel area will front it BUT subsequent keystrokes may snap back.",
        "",
        "If you hit focus instability: shell out to `osascript -e 'tell app \"<Name>\" to activate'`",
        "before sending keys. `cel_perceive` emits `focus_changed` events that let you detect this.",
        "",
        "## Key Patterns",
        "",
        "- Element IDs from `cel_see context` are content-hashed but can rotate if structural",
        "  context (parent, position, sibling count) changes. For ANY cross-observation reference,",
        "  call `cel_see make_reference` to get a resilient handle first, then use it in `cel_act`.",
        "- Batch up to 4 related non-UI-changing actions in one `cel_act` call. Re-observe between batches.",
        "- `cel_see wait_for_element` / `wait_for_idle` after actions that trigger UI changes.",
        "- `cel_see context` response includes an `observation_id` and writes the full snapshot to",
        "  `~/.cellar/observations/<id>.json`. Load it later with `cel_see` mode `observation`",
        "  instead of re-polling the live screen.",
        "- The `filter` parameter on `cel_see context` is an **object**, not a string. Example:",
        "  { filter: { element_types: ['button', 'link'], detail: 'actionable_only' } }",
        "",
        "## Browser Interactions (CDP)",
        "",
        "- When the focused app is a browser with CDP enabled, use `cel_act cdp_eval` for DOM work",
        "  (forms, cookie banners, iframes, overlays invisible to AX).",
        "- `cel_see cdp_status` to check availability; `cel_see cdp_page` for full page text.",
        "- `cel_see context` response includes `cdp_available: true` when a browser target is connected.",
        "",
        "## Knowledge & Memory",
        "",
        "- **cel_think store_knowledge**: persist facts (URLs, creds, app states, workflow gotchas).",
        "  These survive across sessions. Scope by `workflow_name`.",
        "- **cel_think search_knowledge**: FTS5 recall (~10 results default).",
        "- **cel_think observe**: record insight with priority. Use liberally — it's cheap.",
        "- **cel_think memory_get/set**: per-workflow scratchpad, NOT persisted across sessions.",
        "",
        "## Defaults & Limits",
        "",
        "- **run_goal**: canonical loop, 80 step budget, 900s timeout. Only budget limits are tunable.",
        "- **cel_see context** CDP enrichment: 50 text_blocks, 50 interactive_elements, 3000 char body_text.",
        "- **cel_act batch**: max 4 actions per call, 100ms default delay.",
        "- **cel_perceive**: singleton — one session at a time. `cel_see watch` unavailable during.",
        "- **wait_for_idle**: 2 consecutive stable polls required.",
        "",
        "## Decision Guide",
        "",
        "- AX-hostile app (Numbers/Pages/Keynote) → structured app truth first; use `write_cells` / `read_cells` where available",
        "- Browser DOM task → `cel_act cdp_eval`",
        "- Multi-step task → `cel_perceive` session from step 1",
        "- Quick one-shot screen check → `cel_see` mode 'context'",
        "- Fire-and-forget delegation → `cel_think run_goal`",
        "",
        "## Requirements",
        "",
        "- macOS with Accessibility permissions granted to the host process.",
        "- For CDP features: run 'cellar browser ensure' or keep a Chromium browser exposing remote debugging.",
        "- Knowledge store at ~/.cellar/cel-store.db (created automatically).",
        "- Observations archive at ~/.cellar/observations/ (created automatically).",
      ].join("\n"),
    },
  );

  server.registerTool(
    "cel_see",
    {
      title: "CEL See",
      description:
        "Read and observe the current screen state. Returns structured UI elements, " +
        "window lists, screenshots, CDP page content, accessibility element details, " +
        "and screen change events. Always use this BEFORE acting to understand what's on screen.\n\n" +
        "Screen Context: context (elements with filter/compression — use detail 'compact' to save tokens), " +
        "screenshot (PNG capture), windows (visible window list), monitors (display list).\n\n" +
        "Element Inspection: focused (high-fidelity detail for one element_id), " +
        "element_at (hit-test x,y coordinates), is_settable (check if set_value works), " +
        "make_reference (resilient ref that survives across snapshots), cursor_position.\n\n" +
        "Browser (CDP): cdp_status (debug targets & connection state), cdp_page (full page content as text).\n\n" +
        "Observation Recall: observation (load a persisted context snapshot by observation_id).\n\n" +
        "Waiting & Watching: wait_for_element (poll for element by type/label, default 10s timeout), " +
        "wait_for_idle (poll until screen stabilizes — requires 2 consecutive stable polls), " +
        "watch (event-driven — 18 event types: tree_changed, network_idle, focus_changed, value_changed, " +
        "window_created, menu_opened, menu_closed, sheet_created, layout_changed, title_changed, " +
        "app_activated, app_deactivated, window_moved, window_resized, window_minimized, " +
        "window_restored, selection_changed, row_count_changed). " +
        "Note: watch is unavailable during an active cel_perceive session.\n\n" +
        "Limits: CDP enrichment caps at 50 text_blocks, 50 interactive_elements, 3000 char body_text.",
      inputSchema: celSeeSchema,
    },
    async (args) => handleCelSee(instance, args),
  );

  server.registerTool(
    "cel_act",
    {
      title: "CEL Act",
      description:
        "Execute actions on the screen: mouse clicks, keyboard input, accessibility actions, " +
        "drag & drop, and direct value setting. Always use cel_see first to understand the screen.\n\n" +
        "For click/move: provide (x, y) coordinates or a target_ref from cel_see make_reference.\n" +
        "For form filling: prefer set_value over type — faster and more reliable.\n" +
        "For buttons/checkboxes: prefer ax_action over click — uses native accessibility API.\n\n" +
        "Coordinate Actions (x,y or target_ref): click, right_click, double_click, mouse_move.\n\n" +
        "Keyboard: type (text string), key_press (single key: Enter, Tab, Escape, etc.), " +
        "key_combo (modifier combinations: ['Ctrl','C'], ['Cmd','Shift','S']).\n\n" +
        "Accessibility API (preferred for reliability): " +
        "ax_action — native a11y actions on element_id: click, activate, press, increment, " +
        "decrement, cancel, show_menu, scroll_to_visible, raise, pick, delete. " +
        "set_value — direct value injection on element_id: text for fields, 'true'/'false' for checkboxes.\n\n" +
        "Deterministic spreadsheet actions: write_cells (atomic Numbers cell writes with optional readback verification), " +
        "read_cells (read Numbers cell values from the document model instead of guessing from AX text).\n\n" +
        "Other: scroll (dx,dy at optional x,y), drag (from_x,from_y to to_x,to_y), " +
        "cdp_eval (execute JavaScript in browser via CDP — best for cookie banners, iframes, " +
        "overlays, and elements invisible to the accessibility tree).\n\n" +
        "Batching: pass array of 1-4 actions for sequential execution (100ms default delay). " +
        "Re-observe with cel_see after each batch to avoid stale-state cascading failures.",
      inputSchema: celActSchema,
    },
    async (args) => handleCelAct(instance, args),
  );

  server.registerTool(
    "cel_think",
    {
      title: "CEL Think",
      description:
        "CEL's cognitive layer: delegated autonomy, planning, knowledge, run tracking, and LLM passthrough.\n\n" +
        "Efficiency rule: if the MCP host already reasons well step-by-step, prefer `cel_see` + `cel_act` " +
        "and keep planning in the host. Use `run_goal` only when you intentionally want CEL to take over " +
        "the control loop.\n\n" +
        "Delegated Autonomous Execution: run_goal — give a natural language goal, " +
        "CEL runs a full internal see→plan→act loop autonomously. This can be convenient, but it adds " +
        "an internal planner loop and may be slower or more expensive than host-driven execution. " +
        "Only `goal`, `max_steps` (default 80), and `timeout_ms` (default 900_000) are tunable — " +
        "vision, self-healing, decomposition, and notebook are implicit in the canonical loop and " +
        "no longer per-invocation knobs (see docs/canonical-agent-plan.md).\n\n" +
        "Planning: plan (LLM-powered step planning with optional history for multi-step context), " +
        "plan_with_vision (plan with screenshot — use for visual/spatial tasks).\n\n" +
        "Knowledge Store (persisted to ~/.cellar/cel-store.db): " +
        "store_knowledge (save facts with source and optional tags), " +
        "search_knowledge (FTS5 full-text search, default 10 results, scope by workflow).\n\n" +
        "Working Memory: memory_get, memory_set (per-workflow scratchpad, not persisted across sessions).\n\n" +
        "Observations: observe (record insight with priority high/medium/low), get_observations (retrieve, default 50).\n\n" +
        "Run Tracking: run_start, run_finish, run_log_step (per-step with confidence score), " +
        "run_history, run_steps.\n\n" +
        "LLM Passthrough: llm_complete (text, 4096 tokens default), " +
        "llm_complete_with_image (vision, 4096 tokens default).\n\n" +
        "Maintenance: eviction (TTL cleanup — default 90 days runs, 365 days knowledge).",
      inputSchema: celThinkSchema,
    },
    async (args) => handleCelThink(instance, args),
  );

  server.registerTool(
    "cel_perceive",
    {
      title: "CEL Perceive",
      description:
        "Always-on perception engine (Cortex). Maintains a continuously-updated " +
        "mental model via background event streams with periodic accessibility tree refreshes " +
        "on significant changes, and vision/screenshots when flagged as needed.\n\n" +
        "IMPORTANT: Singleton — only one perception session can be active at a time. " +
        "cel_see 'watch' mode is unavailable during an active session.\n\n" +
        "Modes:\n" +
        "- start: Boot the cortex with a goal. Set enable_suggestions=true (default) " +
        "for LLM-powered next-action recommendations on each read.\n" +
        "- read: Get the mental model snapshot (instant — model is kept warm by background events).\n" +
        "- feed: Report an action you took (action, target, expected outcome). " +
        "Cortex waits for screen to settle, diffs against current model, returns verification.\n" +
        "- checkpoint: Summarize completed work and reset action history. Use between phases of multi-step tasks.\n" +
        "- configure: Update goal or enable_suggestions mid-session.\n" +
        "- status: Cortex health — confidence score, uptime, cycle count, " +
        "element counts (stable vs volatile), temporal state (loading, errors, focus trail).\n" +
        "- stop: Shutdown the cortex and get a summary.\n\n" +
        "The model includes temporal awareness (loading states, error persistence, " +
        "focus trail) and element stability classification (stable vs volatile targets).",
      inputSchema: celPerceiveSchema,
    },
    async (args) => handleCelPerceive(instance, args),
  );

  return server;
}

export async function startStdioServer(cel?: Cel): Promise<void> {
  const instance = cel ?? new Cel();

  // Graceful shutdown on process exit or fatal error
  const cleanup = () => {
    try { instance.stopCortex(); } catch { /* best effort */ }
  };
  process.on("SIGINT", cleanup);
  process.on("SIGTERM", cleanup);
  process.on("beforeExit", cleanup);

  try {
    const server = createCelMcpServer(instance);
    const transport = new StdioServerTransport();
    await server.connect(transport);
    console.error("CEL MCP server started (stdio transport)");

    // Boot Rust Cortex via NAPI — always-on perception starts immediately.
    // The mental model is warm before Claude's first tool call.
    instance.bootCortex();
    console.error("Rust Cortex booted — perception active");

    // CDP is launched on-demand by tools that need it (e.g. run_goal)
    // rather than eagerly at startup, to avoid interfering with the user's Chrome.
  } catch (err) {
    cleanup();
    throw err;
  }
}

/**
 * Ensure a dedicated Chromium-family browser instance is running with CDP enabled.
 *
 * Important safety property:
 * - Never reuses or symlinks the user's live browser profile
 * - Never requires the user's already-open browser to quit
 * - Launches an isolated automation profile under ~/.cellar/cdp-profiles/
 *
 * If the default browser is Chromium-based (Chrome/Chromium/Brave/Edge/Arc),
 * prefer that. Otherwise fall back to the first installed Chromium-family app.
 */
/**
 * Ensure a dedicated CEL browser instance is running with CDP enabled.
 *
 * Delegates to the shared runtime utility so the CLI, MCP server, and adapter
 * selection all follow the same dedicated-browser rules.
 */
export async function ensureCdpChrome(cel: Cel): Promise<void> {
  const result = await ensureDedicatedCdpBrowser({ cel });
  console.error(result.message);
}
