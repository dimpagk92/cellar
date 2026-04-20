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
        "## Workflow: See → Act → Think (or Perceive → Act → Feed)",
        "",
        "### Standard (one-shot observations):",
        "1. **cel_see** — Always observe first. Use mode 'context' to get all UI elements with IDs,",
        "   types, labels, bounds, and actions. Use 'screenshot' for a visual snapshot.",
        "2. **cel_act** — Then act. Prefer 'ax_action' over 'click' for buttons (more reliable).",
        "   Prefer 'set_value' over 'type' for form fields (faster, bypasses keyboard).",
        "   Use cel_see 'is_settable' to check if an element supports direct value setting.",
        "3. **cel_think** — Use for planning help, memory, and delegated autonomy.",
        "   If the MCP host is already a strong reasoning model (for example Claude),",
        "   prefer keeping planning in the host and using CEL as eyes + hands.",
        "",
        "### Continuous (persistent perception):",
        "1. **cel_perceive start** — Begin a perception session with a goal. Starts background event monitoring.",
        "2. **cel_perceive read** — Get the mental model snapshot (instant — kept warm by background events).",
        "3. **cel_act** — Execute the suggested action.",
        "4. **cel_perceive feed** — Report the action back. Verifies it landed via screen diff.",
        "5. Repeat 2-4 until goal achieved, then **cel_perceive stop**.",
        "",
        "Use cel_perceive for multi-step tasks where continuous awareness matters.",
        "Use cel_see for quick one-off observations.",
        "Note: cel_see 'watch' mode is unavailable while a perception session is active — use read instead.",
        "",
        "## Key Patterns",
        "",
        "- Element IDs from cel_see context (e.g. 'a11y:42') are used in cel_act's ax_action/set_value.",
        "- For coordinate-based actions, use the element's bounds center from cel_see context.",
        "- Batch up to 4 actions in cel_act, then re-observe with cel_see between batches.",
        "- Use cel_see 'wait_for_element' or 'wait_for_idle' after actions that trigger UI changes.",
        "- Use cel_see 'make_reference' to create resilient refs that survive across context snapshots.",
        "- When the host model can reason step-by-step, keep the loop in the host: observe with cel_see,",
        "  act with cel_act, then re-observe. This avoids paying for a second internal planner loop.",
        "",
        "## Browser Interactions (CDP)",
        "",
        "- When the focused app is a browser with CDP enabled, use cel_act's 'cdp_eval' action",
        "  for DOM interactions (clicking buttons, filling forms, dismissing dialogs).",
        "  This is more reliable than coordinate-based clicking, especially for iframe content.",
        "- Use cel_see 'cdp_status' to check if CDP is available.",
        "- The context response includes 'cdp_available: true' when a browser target is connected.",
        "",
        "## Defaults & Limits",
        "",
        "- **run_goal**: delegated autonomy path. Best when you explicitly want CEL to own the loop.",
        "  It uses an internal planner and may be less efficient than host-driven see/act control.",
        "  Max 30 steps, 120s timeout. Vision, self_heal, context_lazy, notebook all ON by default.",
        "- **cel_see context** CDP enrichment: capped at 50 text_blocks, 50 interactive_elements, 3000 char body_text.",
        "- **cel_act batch**: max 4 actions per call, 100ms default delay between actions.",
        "- **cel_perceive**: singleton — only one perception session at a time.",
        "- **cel_see watch**: unavailable while a cel_perceive session is active.",
        "- **wait_for_idle**: requires 2 consecutive stable polls to confirm idle.",
        "",
        "## Decision Guide",
        "",
        "- Quick screen check → `cel_see` mode 'context'",
        "- Read browser page text → `cel_see` mode 'cdp_page'",
        "- Browser/CDP task with a capable host model → stay in `cel_see` + `cel_act`; do not delegate unless needed",
        "- Multi-step task (you are the brain) → `cel_see` + `cel_act` loop, re-observe after each batch",
        "- Multi-step task (fire and forget / delegated autonomy) → `cel_think` mode 'run_goal'",
        "- Continuous awareness needed → `cel_perceive` session (start → read → act → feed → repeat)",
        "",
        "## Requirements",
        "",
        "- macOS with Accessibility permissions granted to the host process.",
        "- For CDP features: run 'cellar browser ensure' or keep a Chromium browser exposing remote debugging.",
        "- Knowledge store at ~/.cellar/cel-store.db (created automatically).",
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
        "an internal planner loop and may be slower or more expensive than host-driven execution. Key options: " +
        "decompose (break into sub-tasks with orchestrator), enable_vision (screenshots for visual tasks), " +
        "self_heal (replan on failure), context_lazy (skip a11y tree for keyboard-only actions), " +
        "enable_notebook (persist extracted data across replans — prices, URLs, confirmation numbers), " +
        "workflow_name (scope history for learning). Defaults: 30 max steps, 120s timeout, " +
        "vision/self_heal/context_lazy/notebook all ON.\n\n" +
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
