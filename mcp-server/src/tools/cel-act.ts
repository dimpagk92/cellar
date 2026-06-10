import { z } from "zod";
import type { Cel } from "@cellar/agent/runtime";
import { sleep, resolveCoords, contextReferenceSchema, textResult, errorResult, axPermissionGuard } from "./shared.js";
import { ensureFrontmost } from "../helpers/focus.js";
import { daemonAct, daemonCortex } from "../helpers/daemon.js";

const actionTypes = [
  "click",
  "right_click",
  "double_click",
  "mouse_move",
  "type",
  "key_press",
  "key_combo",
  "scroll",
  "drag",
  "mouse_down",
  "mouse_up",
  "ax_action",
  "set_value",
  "activate_app",
  "launch_app",
  "quit_app",
  "cdp_eval",
  "navigate",
  "write_cells",
  "read_cells",
  "focus_lock",
  "focus_release",
  "adapter_action",
  "window",
  "dialog",
  "dock",
  "menu_extra",
] as const;

/**
 * Process-level focus lock. When set, every subsequent focus-sensitive
 * `cel_act` action auto-fills its `target_app` from this lock before
 * dispatching, so multi-step sequences from external MCP hosts (Claude
 * Code, langgraph, cursor) survive focus shifts between tool round-trips.
 *
 * - Set by `cel_act focus_lock`, cleared by `cel_act focus_release`.
 * - Explicit `target_app` on an individual action always wins (lock is a
 *   default, not an override) — letting agents temporarily redirect a single
 *   action without releasing the lock.
 * - Non-focus-sensitive actions (`cdp_eval`, `navigate`, `ax_action`,
 *   `set_value`, `write_cells`, `read_cells`) are unaffected: they don't
 *   route via CGEventPost so the focus race doesn't apply.
 * - Singleton — only one lock at a time per MCP server process. A second
 *   `focus_lock` replaces the first.
 */
let focusLock: { appName: string; lockedAt: number } | null = null;

/**
 * Shared description for the `target_app` field. All focus-sensitive actions
 * (keystrokes + coord-based mouse events) reuse this so the wording stays in
 * sync across schema variants.
 */
const targetAppDescription =
  "Optional macOS app name (e.g. 'Finder', 'Numbers', 'Google Chrome') to bring " +
  "frontmost before firing this action. Closes the focus race that can route " +
  "the click/keystroke into the MCP host's window when system focus oscillates " +
  "between the previous tool round-trip and the CGEvent firing. If omitted, " +
  "the action is sent to whichever app is frontmost at the instant of the " +
  "event (legacy behavior).";

const focusModeDescription =
  "How to deliver this focus-sensitive action. 'foreground' (default) brings " +
  "target_app frontmost then posts input. 'background' posts the event directly to " +
  "target_app's process via CGEventPostToPid WITHOUT activating it, so the user's " +
  "active window keeps focus. Requires target_app (or an active focus_lock) to resolve " +
  "the target PID; falls back to foreground if no PID resolves. Not every app honors " +
  "background-delivered events.";

/**
 * Optional target_app + focus_mode fields. Spread into every action variant
 * whose dispatch path is focus-sensitive (CGEventPost-based).
 */
const targetAppField = {
  target_app: z.string().optional().describe(targetAppDescription),
  focus_mode: z.enum(["foreground", "background"]).optional().describe(focusModeDescription),
};

/**
 * Action variants that dispatch via CGEventPost and therefore route to the
 * system-frontmost window. `target_app` only has an effect on these.
 */
const FOCUS_SENSITIVE_ACTIONS: ReadonlySet<string> = new Set([
  "click",
  "right_click",
  "double_click",
  "mouse_move",
  "type",
  "key_press",
  "key_combo",
  "scroll",
  "drag",
  "mouse_down",
  "mouse_up",
]);

const coordActionBase = {
  x: z.number().optional().describe("X coordinate. Not needed if target_ref is provided."),
  y: z.number().optional().describe("Y coordinate. Not needed if target_ref is provided."),
  target_ref: contextReferenceSchema
    .optional()
    .describe(
      "Resilient element reference. If provided, CEL resolves the element and uses its center. " +
        "Get references from cel_see with mode 'make_reference'.",
    ),
  ...targetAppField,
};

const singleActionSchema = z.discriminatedUnion("action", [
  z.object({ action: z.literal("click"), ...coordActionBase }),
  z.object({ action: z.literal("right_click"), ...coordActionBase }),
  z.object({ action: z.literal("double_click"), ...coordActionBase }),
  z.object({ action: z.literal("mouse_move"), ...coordActionBase }),
  z.object({
    action: z.literal("type"),
    text: z.string().describe("Text to type using keyboard input"),
    wpm: z
      .number()
      .int()
      .min(20)
      .max(400)
      .optional()
      .describe("Typing speed (words/min) for human cadence. Omit = instant."),
    paste: z
      .boolean()
      .optional()
      .describe(
        "Insert via the clipboard (Cmd+V) instead of keystrokes, restoring the " +
          "user's previous clipboard. Reliable for emoji/newlines; ignores wpm.",
      ),
    ...targetAppField,
  }),
  z.object({
    action: z.literal("key_press"),
    key: z.string().describe("Key name (e.g. Enter, Tab, Escape, Backspace)"),
    ...targetAppField,
  }),
  z.object({
    action: z.literal("key_combo"),
    keys: z
      .array(z.string())
      .min(1)
      .describe("Key names for combination (e.g. ['Ctrl', 'C'], ['Cmd', 'V'])"),
    ...targetAppField,
  }),
  z.object({
    action: z.literal("scroll"),
    dx: z.number().default(0).describe("Horizontal scroll amount"),
    dy: z.number().default(0).describe("Vertical scroll amount (positive = down)"),
    x: z.number().optional().describe("Scroll at this X coordinate"),
    y: z.number().optional().describe("Scroll at this Y coordinate"),
    ...targetAppField,
  }),
  z.object({
    action: z.literal("swipe"),
    direction: z
      .enum(["up", "down", "left", "right"])
      .describe("Swipe direction"),
    amount: z
      .number()
      .int()
      .positive()
      .default(10)
      .describe("Swipe magnitude in scroll units."),
    ...targetAppField,
  }),
  z.object({
    action: z.literal("drag"),
    from_x: z.number().describe("Start X coordinate"),
    from_y: z.number().describe("Start Y coordinate"),
    to_x: z.number().describe("End X coordinate"),
    to_y: z.number().describe("End Y coordinate"),
    ...targetAppField,
  }),
  z.object({
    action: z.literal("mouse_down"),
    x: z.number().describe("X coordinate to press the left button at"),
    y: z.number().describe("Y coordinate to press the left button at"),
    ...targetAppField,
  }),
  z.object({
    action: z.literal("mouse_up"),
    x: z.number().describe("X coordinate to release the left button at"),
    y: z.number().describe("Y coordinate to release the left button at"),
    ...targetAppField,
  }),
  z.object({
    action: z.literal("ax_action"),
    element_id: z.string().describe("Accessibility element ID from cel_see context"),
    ax_action: z
      .enum([
        "click",
        "activate",
        "press",
        "increment",
        "decrement",
        "cancel",
        "show_menu",
        "scroll_to_visible",
        "raise",
        "pick",
        "delete",
      ])
      .describe("Accessibility action to perform"),
  }),
  z.object({
    action: z.literal("set_value"),
    element_id: z.string().describe("Accessibility element ID from cel_see context"),
    value: z
      .string()
      .describe("Value to set (text for fields, 'true'/'false' for checkboxes)"),
  }),
  z.object({
    action: z.literal("activate_app"),
    app_name: z
      .string()
      .describe("Application name to activate/launch (e.g. 'Google Chrome', 'Finder', 'Terminal')"),
  }),
  z.object({
    action: z.literal("launch_app"),
    app_name: z.string().describe("Application name to launch/start (e.g. 'TextEdit', 'Calculator')"),
    background: z
      .boolean()
      .optional()
      .describe(
        "Launch without bringing the app to the front (open -g). Useful for warming up an " +
          "app you will drive headlessly. Default false (foreground).",
      ),
  }),
  z.object({
    action: z.literal("quit_app"),
    app_name: z
      .string()
      .describe(
        "Application name to quit gracefully (like Cmd+Q). Never force-kills — an app with " +
          "unsaved changes may surface a dialog and stay open.",
      ),
  }),
  z.object({
    action: z.literal("cdp_eval"),
    expression: z
      .string()
      .describe(
        "JavaScript to execute in the browser page via Chrome DevTools Protocol. " +
          "Use document.querySelector() to find elements, .click() to click, .value= to set values. " +
          "Works inside iframes and on elements invisible to the accessibility tree (cookie banners, overlays). " +
          "Requires Chrome running with --remote-debugging-port. " +
          "DO NOT use to change the page URL — use the `navigate` action instead. Setting " +
          "`window.location.href` via cdp_eval can leave stray about:blank tabs in the user's window. " +
          "Example: document.querySelectorAll('button').forEach(b => { if(b.textContent.includes('Accept')) b.click() })",
      ),
  }),
  z.object({
    action: z.literal("navigate"),
    url: z
      .string()
      .describe(
        "URL to navigate the focused CEL browser tab to. Routes through the canonical " +
          "browser adapter when one is registered (Playwright `page.goto` with " +
          "`waitUntil: domcontentloaded` + cookie-banner dismissal), otherwise falls " +
          "back to in-cortex CDP `Page.navigate` plus a `document.readyState` poll. " +
          "Prefer this over `cdp_eval` with `window.location` for URL changes.",
      ),
    wait_until: z
      .enum(["none", "domcontentloaded", "load", "networkidle"])
      .default("domcontentloaded")
      .describe(
        "Lifecycle event to wait for before returning. `domcontentloaded` (default) " +
          "matches the TS adapter's existing semantics. `none` skips the wait — only " +
          "useful when the next action will explicitly verify load.",
      ),
    timeout_ms: z
      .number()
      .int()
      .positive()
      .default(30_000)
      .describe("Upper bound on the lifecycle wait, in milliseconds."),
    dismiss_overlays: z
      .boolean()
      .default(true)
      .describe(
        "When true (default), the cortex fallback runs a best-effort cookie-banner / " +
          "overlay-dismiss script after the page settles. The TS browser adapter does " +
          "this unconditionally, so this flag only affects the in-cortex fallback path.",
      ),
  }),
  z.object({
    action: z.literal("write_cells"),
    app: z.string().default("Numbers").describe("Spreadsheet app. Currently only Numbers is supported."),
    sheet: z.string().optional().describe("Optional sheet name. Defaults to the first sheet."),
    table: z.string().optional().describe("Optional table name. Defaults to the first table."),
    writes: z.array(z.object({
      cell_ref: z.string().describe("A1-style cell reference, e.g. A1 or B2"),
      value: z.string().describe("Value to write into the cell"),
    })).min(1).describe("Cells to write in a single deterministic batch."),
    verify: z.boolean().default(true).describe("Read cells back after writing and fail if the stored values mismatch."),
  }),
  z.object({
    action: z.literal("read_cells"),
    app: z.string().default("Numbers").describe("Spreadsheet app. Currently only Numbers is supported."),
    sheet: z.string().optional().describe("Optional sheet name. Defaults to the first sheet."),
    table: z.string().optional().describe("Optional table name. Defaults to the first table."),
    cell_refs: z.array(z.string()).min(1).describe("A1-style cell references to read back from the document model."),
  }),
  z.object({
    action: z.literal("focus_lock"),
    app_name: z
      .string()
      .describe(
        "App to bind subsequent focus-sensitive cel_act calls to. Until focus_release " +
          "is invoked, every keystroke/click/scroll action auto-fills its target_app from " +
          "this lock before dispatching, so multi-step sequences survive focus shifts " +
          "between tool round-trips. Explicit target_app on an action overrides the lock " +
          "for that one action. focus_lock itself activates app_name immediately and errors " +
          "if activation can't be confirmed.",
      ),
    timeout_ms: z
      .number()
      .int()
      .positive()
      .default(1500)
      .describe(
        "Maximum time to wait for app_name to become frontmost before failing the lock. " +
          "Default 1500ms is generous for cold launches; pass a smaller value when the " +
          "target is known to be running.",
      ),
  }),
  z.object({
    action: z.literal("focus_release"),
  }),
  z.object({
    action: z.literal("adapter_action"),
    adapter: z
      .string()
      .describe(
        "Adapter name (matches manifest.name). Examples: 'notes', 'numbers', 'browser'. " +
          "The adapter must be registered AND active (probe()=true) for the call to dispatch.",
      ),
    adapter_op: z
      .string()
      .describe(
        "Operation name declared by the adapter's manifest.actions. Examples for notes: " +
          "'create', 'set_body', 'append', 'get_body', 'list', 'find'. For numbers: " +
          "'write_cells', 'read_cells', 'snapshot_preview'.",
      ),
    params: z
      .record(z.any())
      .default({})
      .describe(
        "Operation parameters. Shape is adapter-defined — see manifest.actions[op].params " +
          "or each adapter's docs. Auto-verification runs after mutating ops unless " +
          "params.verify === false.",
      ),
  }),
  z.object({
    action: z.literal("window"),
    op: z
      .enum([
        "move",
        "resize",
        "set_bounds",
        "minimize",
        "unminimize",
        "maximize",
        "focus",
      ])
      .optional()
      .describe(
        "Window operation. Omit when using `preset`. One of: move, resize, " +
          "set_bounds, minimize, unminimize, maximize, focus.",
      ),
    app: z
      .string()
      .optional()
      .describe("Target app name (e.g. 'Finder'). Omit to target the frontmost app."),
    window_index: z
      .number()
      .int()
      .min(0)
      .default(0)
      .describe("Window index within the app (0 = the frontmost window)."),
    x: z.number().optional().describe("Top-left X in screen points (move / set_bounds)."),
    y: z.number().optional().describe("Top-left Y in screen points (move / set_bounds)."),
    width: z.number().optional().describe("Width in points (resize / set_bounds)."),
    height: z.number().optional().describe("Height in points (resize / set_bounds)."),
    preset: z
      .enum([
        "left_half",
        "right_half",
        "top_half",
        "bottom_half",
        "top_left",
        "top_right",
        "bottom_left",
        "bottom_right",
        "maximize",
        "center",
      ])
      .optional()
      .describe("Tiling preset over the display's visible frame. Overrides op + geometry."),
    display: z
      .number()
      .int()
      .min(0)
      .optional()
      .describe("Target display index (0-based). Defaults to the window's current display."),
  }),
  z.object({
    action: z.literal("dialog"),
    op: z
      .enum(["list", "click", "set_field", "dismiss"])
      .describe(
        "Dialog op: list (enumerate buttons + fields), click (button by title), " +
          "set_field (set a text field), dismiss (Cancel / Don't Save / Close).",
      ),
    button: z
      .string()
      .optional()
      .describe("Button title to click (op=click); case-insensitive substring."),
    value: z.string().optional().describe("Value to set (op=set_field)."),
    field_index: z
      .number()
      .int()
      .min(0)
      .default(0)
      .describe("Which visible text field to set (op=set_field, 0-based)."),
  }),
  z.object({
    action: z.literal("dock"),
    op: z
      .enum(["list", "launch", "right_click", "hide", "show"])
      .describe(
        "Dock op: list (item titles), launch (by title), right_click (show its " +
          "menu), hide / show (toggle auto-hide).",
      ),
    name: z.string().optional().describe("Dock item title (op=launch / right_click)."),
  }),
  z.object({
    action: z.literal("menu_extra"),
    op: z
      .enum(["list", "click"])
      .describe("Menu-extra op: list (system status-item titles) or click (by title)."),
    name: z.string().optional().describe("Status-item title to click (op=click)."),
  }),
]);

export const celActSchema = z.union([
  singleActionSchema,
  z.object({
    actions: z
      .array(singleActionSchema)
      .min(1)
      .max(4)
      .describe(
        "Array of 1-4 actions to execute sequentially. " +
          "Re-observe with cel_see between batches to catch intermediate state changes.",
      ),
    delay_between_ms: z
      .number()
      .default(100)
      .describe("Delay between actions in milliseconds"),
  }),
]);

const advertisedActionSchema = z
  .object({
    action: z.enum(actionTypes).optional().describe("Action variant to execute."),
    target_app: z.string().optional().describe(targetAppDescription),
    focus_mode: z.enum(["foreground", "background"]).optional().describe(focusModeDescription),
  })
  .passthrough();

/**
 * MCP-facing schema. The SDK only advertises object-shaped schemas in
 * tools/list, so the precise union above is still used for runtime parsing
 * while this object schema gives clients discoverable fields.
 */
export const celActMcpInputSchema = z
  .object({
    action: z.enum(actionTypes).optional().describe("Single action variant to execute."),
    actions: z
      .array(advertisedActionSchema)
      .min(1)
      .max(4)
      .optional()
      .describe("Batch of 1-4 actions to execute sequentially."),
    delay_between_ms: z.number().optional().describe("Delay between batched actions in milliseconds."),
    target_app: z.string().optional().describe(targetAppDescription),
    focus_mode: z.enum(["foreground", "background"]).optional().describe(focusModeDescription),
  })
  .passthrough();

type SingleAction = z.infer<typeof singleActionSchema>;
type Input = z.infer<typeof celActSchema>;

type ActionReceiptStatus = "ok" | "error";
type ActionDispatchPath =
  | "native_input"
  | "accessibility"
  | "cdp"
  | "adapter"
  | "cortex"
  | "focus"
  | "unknown";

type ActionReceipt = {
  id: string;
  action: SingleAction["action"];
  requested_at: string;
  completed_at: string;
  status: ActionReceiptStatus;
  dispatch_path: ActionDispatchPath;
  mutates_state: boolean;
  requires_verification: boolean;
  verification: "adapter_readback" | "cdp_or_cortex" | "caller_must_reobserve" | "not_required";
  evidence: Array<{ kind: string; value?: unknown }>;
  summary: string;
  error?: string;
  /**
   * The canonical, core-emitted execution receipt (cel-contracts), when the
   * cortex dispatch path produced one. Present for canonical routes (navigate,
   * cells, adapter, `dom:*` ax_action/set_value); absent for raw native
   * primitives that bypass `cortex.execute`. When present, `dispatch_path`
   * above reflects its real `route` rather than the static guess.
   */
  core?: CoreReceipt;
};

let receiptCounter = 0;

function nextReceiptId(action: SingleAction["action"]): string {
  receiptCounter = (receiptCounter + 1) % 100_000;
  return `cel_act_${action}_${Date.now().toString(36)}_${receiptCounter}`;
}

function dispatchPathForAction(action: SingleAction): ActionDispatchPath {
  switch (action.action) {
    case "click":
    case "right_click":
    case "double_click":
    case "mouse_move":
    case "type":
    case "key_press":
    case "key_combo":
    case "scroll":
    case "drag":
    case "mouse_down":
    case "mouse_up":
      return "native_input";
    case "ax_action":
    case "set_value":
      return action.element_id.startsWith("dom:") ? "cdp" : "accessibility";
    case "activate_app":
    case "launch_app":
    case "quit_app":
    case "focus_lock":
    case "focus_release":
      return "focus";
    case "cdp_eval":
    case "navigate":
      return "cdp";
    case "write_cells":
    case "read_cells":
    case "adapter_action":
      return "adapter";
    case "window":
    case "dialog":
    case "dock":
    case "menu_extra":
      return "accessibility";
    default:
      return "unknown";
  }
}

function mutatesState(action: SingleAction): boolean {
  switch (action.action) {
    case "mouse_move":
    case "read_cells":
      return false;
    default:
      return true;
  }
}

function requiresVerification(action: SingleAction): boolean {
  switch (action.action) {
    case "mouse_move":
    case "read_cells":
    case "focus_release":
      return false;
    case "write_cells":
      return action.verify !== false;
    case "focus_lock":
    case "activate_app":
    case "launch_app":
    case "quit_app":
      return true;
    default:
      return mutatesState(action);
  }
}

function verificationMode(action: SingleAction): ActionReceipt["verification"] {
  if (!requiresVerification(action)) {
    return "not_required";
  }
  switch (action.action) {
    case "write_cells":
      return "adapter_readback";
    case "adapter_action":
      return "adapter_readback";
    case "navigate":
    case "cdp_eval":
    case "focus_lock":
    case "activate_app":
    case "launch_app":
    case "quit_app":
      return "cdp_or_cortex";
    default:
      return "caller_must_reobserve";
  }
}

function evidenceForAction(action: SingleAction): ActionReceipt["evidence"] {
  const evidence: ActionReceipt["evidence"] = [
    { kind: "dispatch_path", value: dispatchPathForAction(action) },
  ];
  if ("target_ref" in action && action.target_ref) {
    evidence.push({ kind: "target_ref", value: action.target_ref });
  }
  if ("target_app" in action && action.target_app) {
    evidence.push({ kind: "target_app", value: action.target_app });
  }
  if ("focus_mode" in action && action.focus_mode) {
    evidence.push({ kind: "focus_mode", value: action.focus_mode });
  }
  if (action.action === "write_cells") {
    evidence.push({ kind: "cell_refs", value: action.writes.map((w) => w.cell_ref) });
    evidence.push({ kind: "verify_requested", value: action.verify !== false });
  }
  if (action.action === "read_cells") {
    evidence.push({ kind: "cell_refs", value: action.cell_refs });
  }
  if (action.action === "adapter_action") {
    evidence.push({ kind: "adapter", value: action.adapter });
    evidence.push({ kind: "adapter_op", value: action.adapter_op });
    evidence.push({ kind: "verify_requested", value: action.params?.verify !== false });
  }
  if (action.action === "navigate") {
    evidence.push({ kind: "url", value: action.url });
    evidence.push({ kind: "wait_until", value: action.wait_until });
  }
  return evidence;
}

/**
 * The canonical, core-emitted ExecutionReceipt (cel-contracts), transported on
 * `StepResult.data._cel_receipt` by the cortex dispatch path (PR #163/#164).
 * Typed loosely here — the MCP server forwards it verbatim; the Rust crate owns
 * the shape.
 */
type CoreReceipt = Record<string, unknown>;

/** A dispatched action's human summary plus the core receipt when one was emitted. */
type ActionExec = { summary: string; receipt: CoreReceipt | null };

/**
 * Pull the core receipt out of a canonical `StepResult.data`, separating it
 * from the action's own payload so summaries that dump `data` stay clean.
 */
function splitReceipt(step: { data?: unknown } | null | undefined): {
  data: unknown;
  receipt: CoreReceipt | null;
} {
  const data = step?.data;
  if (data && typeof data === "object" && "_cel_receipt" in data) {
    const { _cel_receipt, ...rest } = data as Record<string, unknown>;
    return { data: rest, receipt: (_cel_receipt as CoreReceipt) ?? null };
  }
  return { data: data ?? {}, receipt: null };
}

/** Extract just the core receipt (summary built from named fields, not a dump). */
function receiptOf(step: { data?: unknown } | null | undefined): CoreReceipt | null {
  return splitReceipt(step).receipt;
}

/** Map the core receipt's real `route` to the MCP dispatch-path vocabulary. */
function coreRouteToDispatchPath(receipt: CoreReceipt | null): ActionDispatchPath | null {
  const route = receipt?.route as { route?: string } | undefined;
  switch (route?.route) {
    case "cdp":
      return "cdp";
    case "accessibility":
      return "accessibility";
    case "native_input":
      return "native_input";
    case "adapter":
      return "adapter";
    case "focus":
      return "focus";
    default:
      return null;
  }
}

function buildReceipt(
  action: SingleAction,
  requestedAtMs: number,
  status: ActionReceiptStatus,
  summary: string,
  error?: string,
  coreReceipt?: CoreReceipt | null,
): ActionReceipt {
  const core = coreReceipt ?? null;
  // When the core emitted a receipt, its `route` is the REAL dispatch path and
  // overrides the static `dispatchPathForAction` guess (which mislabels e.g. a
  // `dom:*` set_value, routed via CDP, as "accessibility").
  const corePath = coreRouteToDispatchPath(core);
  const evidence = evidenceForAction(action);
  if (core) {
    if (typeof core.receipt_id === "string") {
      evidence.push({ kind: "core_receipt_id", value: core.receipt_id });
    }
    const observed = core.observed_effect as { status?: string } | undefined;
    if (observed?.status) {
      evidence.push({ kind: "observed_effect", value: observed.status });
    }
  }
  return {
    id: nextReceiptId(action.action),
    action: action.action,
    requested_at: new Date(requestedAtMs).toISOString(),
    completed_at: new Date().toISOString(),
    status,
    dispatch_path: corePath ?? dispatchPathForAction(action),
    mutates_state: mutatesState(action),
    requires_verification: requiresVerification(action),
    verification: verificationMode(action),
    evidence,
    summary,
    ...(error ? { error } : {}),
    ...(core ? { core } : {}),
  };
}

function receiptErrorResult(data: unknown) {
  return {
    isError: true,
    content: [
      {
        type: "text" as const,
        text: JSON.stringify(data, null, 2),
      },
    ],
  };
}

/**
 * Before firing a focus-sensitive action, ensure the requested target app is
 * frontmost. Returns the focus diagnostic so the caller can include it in the
 * tool result. Throws if the target couldn't be brought frontmost within the
 * timeout — better to surface a clear error than to send the event into the
 * wrong window.
 */
async function bringTargetFrontmost(targetApp: string | undefined) {
  if (!targetApp) return undefined;
  const focus = await ensureFrontmost(targetApp);
  if (!focus.matchesTarget) {
    throw new Error(
      `target_app="${targetApp}" never became frontmost within ${focus.elapsedMs}ms ` +
        `(still showing "${focus.frontmost}"). Action aborted to avoid routing ` +
        `the event into the wrong window.`,
    );
  }
  return focus;
}

/**
 * WS1: execute a focus-sensitive action via the background (non-focus-stealing)
 * PID path. Returns the result string, or `null` for variants without a
 * background equivalent (scroll/drag/mouse_move) so the caller falls back to
 * the foreground path.
 */
function executeSingleBackground(
  cel: Cel,
  action: SingleAction,
  pid: number,
): string | null {
  switch (action.action) {
    case "click": {
      const { x, y, label } = resolveCoords(cel, action);
      cel.clickToPid(pid, x, y);
      return `Clicked ${label} at (${x}, ${y})`;
    }
    case "right_click": {
      const { x, y, label } = resolveCoords(cel, action);
      cel.rightClickToPid(pid, x, y);
      return `Right-clicked ${label} at (${x}, ${y})`;
    }
    case "double_click": {
      const { x, y, label } = resolveCoords(cel, action);
      cel.doubleClickToPid(pid, x, y);
      return `Double-clicked ${label} at (${x}, ${y})`;
    }
    case "type":
      cel.typeTextToPid(pid, action.text);
      return `Typed "${action.text}"`;
    case "key_press":
      cel.keyPressToPid(pid, action.key);
      return `Pressed key: ${action.key}`;
    case "key_combo":
      cel.keyComboToPid(pid, action.keys);
      return `Pressed combo: ${action.keys.join("+")}`;
    default:
      // scroll / drag / mouse_move have no background variant yet.
      return null;
  }
}

function executeSingle(cel: Cel, action: SingleAction): string {
  switch (action.action) {
    case "click": {
      const { x, y, label } = resolveCoords(cel, action);
      cel.click(x, y);
      return `Clicked ${label} at (${x}, ${y})`;
    }
    case "right_click": {
      const { x, y, label } = resolveCoords(cel, action);
      cel.rightClick(x, y);
      return `Right-clicked ${label} at (${x}, ${y})`;
    }
    case "double_click": {
      const { x, y, label } = resolveCoords(cel, action);
      cel.doubleClick(x, y);
      return `Double-clicked ${label} at (${x}, ${y})`;
    }
    case "mouse_move": {
      const { x, y, label } = resolveCoords(cel, action);
      cel.mouseMove(x, y);
      return `Moved mouse to ${label} at (${x}, ${y})`;
    }
    case "type":
      if (action.paste) {
        cel.pasteWithRestore(action.text);
        return `Pasted "${action.text}" via clipboard (restored)`;
      }
      if (action.wpm) {
        cel.typeTextCadence(action.text, Math.max(1, Math.round(12000 / action.wpm)));
      } else {
        cel.typeText(action.text);
      }
      return `Typed "${action.text}"${action.wpm ? ` @ ${action.wpm}wpm` : ""}`;
    case "key_press":
      cel.keyPress(action.key);
      return `Pressed key: ${action.key}`;
    case "key_combo":
      cel.keyCombo(action.keys);
      return `Pressed combo: ${action.keys.join("+")}`;
    case "scroll":
      if (action.x !== undefined && action.y !== undefined) {
        cel.mouseMove(action.x, action.y);
      }
      cel.scroll(action.dx ?? 0, action.dy ?? 0);
      return `Scrolled (${action.dx ?? 0}, ${action.dy ?? 0})`;
    case "swipe":
      cel.swipe(action.direction, action.amount);
      return `Swiped ${action.direction} (${action.amount})`;
    case "drag":
      cel.drag(action.from_x, action.from_y, action.to_x, action.to_y);
      return `Dragged from (${action.from_x}, ${action.from_y}) to (${action.to_x}, ${action.to_y})`;
    case "mouse_down":
      (cel as any).mouseDown(action.x, action.y);
      return `Pressed mouse at (${action.x}, ${action.y})`;
    case "mouse_up":
      (cel as any).mouseUp(action.x, action.y);
      return `Released mouse at (${action.x}, ${action.y})`;
    case "ax_action": {
      const success = cel.axPerformAction(action.element_id, action.ax_action);
      return success
        ? `Performed ${action.ax_action} on element ${action.element_id}`
        : `Failed to perform ${action.ax_action} on element ${action.element_id}`;
    }
    case "set_value": {
      const success = cel.axSetValue(action.element_id, action.value);
      return success
        ? `Set value "${action.value}" on element ${action.element_id}`
        : `Failed to set value on element ${action.element_id}`;
    }
    case "activate_app": {
      const success = (cel as any).activateApp(action.app_name);
      return success
        ? `Activated app: ${action.app_name}`
        : `Failed to activate app: ${action.app_name}`;
    }
    case "launch_app": {
      const success = (cel as any).launchApp(action.app_name, action.background ?? false);
      const how = action.background ? " (background)" : "";
      return success
        ? `Launched app: ${action.app_name}${how}`
        : `Failed to launch app: ${action.app_name}`;
    }
    case "quit_app": {
      const success = (cel as any).quitApp(action.app_name);
      return success
        ? `Asked app to quit: ${action.app_name}`
        : `Failed to quit app: ${action.app_name}`;
    }
    case "cdp_eval": {
      // cdp_eval is async — handled separately in handleCelAct
      throw new Error("cdp_eval must be handled async");
    }
    case "navigate":
      throw new Error("navigate must be handled async");
    case "write_cells":
    case "read_cells":
      throw new Error(`${action.action} must be handled async`);
    case "focus_lock":
    case "focus_release":
      // Focus-lock actions are async and have side effects on module
      // state, so they're handled in executeAction above. This branch
      // exists only to keep the exhaustive switch happy.
      throw new Error(`${action.action} must be handled async`);
    case "adapter_action":
      // adapter_action routes through the canonical pipeline (PlannedAction::Custom
      // in cortex.rs); handled async in executeAction.
      throw new Error("adapter_action must be handled async");
    case "window":
      throw new Error("window must be handled async");
    case "dialog":
      throw new Error("dialog must be handled async");
    case "dock":
      throw new Error("dock must be handled async");
    case "menu_extra":
      throw new Error("menu_extra must be handled async");
  }
}

async function ensureCortexForCanonicalAction(cel: Cel): Promise<void> {
  // Two-Cortex guard (cellar-daemon-cortex.md Phase C): when the daemon hosts
  // the single live Cortex, canonical actions proxy to it over IPC and the
  // napi Cortex must NOT boot — two Cortexes would fight over one AX tree
  // and input focus.
  if (await daemonCortex()) {
    return;
  }
  if (!cel.isCortexRunning()) {
    cel.bootCortex();
    await sleep(700);
  }
}

/**
 * Route a canonical step to whichever Cortex owns execution: the
 * daemon-hosted one over IPC (`cortex.act`) when available, else the
 * in-process napi Cortex (`canonicalExecuteStep`). Both paths return the
 * engine's `ActionResult` shape with the core-emitted `ExecutionReceipt`
 * riding on `data._cel_receipt`, so receipt handling downstream is
 * transport-agnostic.
 */
async function canonicalStep(
  cel: Cel,
  step: { purpose: string; kind: string; action: Record<string, unknown> },
): Promise<{ status: string; data?: unknown; message?: string }> {
  const daemon = await daemonCortex();
  if (daemon) {
    const res = await daemonAct(daemon, step.action);
    return res.success
      ? { status: "ok", data: res.data ?? {} }
      : { status: "err", message: res.error ?? "cortex.act failed" };
  }
  return cel.canonicalExecuteStep(step as Parameters<Cel["canonicalExecuteStep"]>[0]);
}

async function executeSpreadsheetAction(
  cel: Cel,
  action: Extract<SingleAction, { action: "write_cells" | "read_cells" }>,
): Promise<ActionExec> {
  await ensureCortexForCanonicalAction(cel);
  const canonicalAction = action.action === "write_cells"
    ? {
        type: "write_cells" as const,
        app: action.app,
        sheet: action.sheet ?? null,
        table: action.table ?? null,
        writes: action.writes,
        verify: action.verify,
      }
    : {
        type: "read_cells" as const,
        app: action.app,
        sheet: action.sheet ?? null,
        table: action.table ?? null,
        cell_refs: action.cell_refs,
      };

  const result = await canonicalStep(cel, {
    purpose:
      action.action === "write_cells"
        ? "Write spreadsheet cells through the deterministic Numbers backend"
        : "Read spreadsheet cells through the deterministic Numbers backend",
    kind: "deterministic",
    action: canonicalAction,
  });

  if (result.status !== "ok") {
    throw new Error(result.message);
  }
  const { data, receipt } = splitReceipt(result);
  return { summary: JSON.stringify(data, null, 2), receipt };
}

/**
 * Route `cel_act navigate` through the canonical adapter pipeline —
 * same shape as `executeSpreadsheetAction`. The cortex's
 * `dispatch_navigate` (cortex.rs) prefers a registered browser-DOM
 * adapter (TS Playwright peer) when one is active and otherwise falls
 * back to in-cortex `cel_cdp::Page.navigate` plus a `document.readyState`
 * poll. The canonical path replaces the prior MCP shortcut that called
 * `cel.cdpNavigate` directly, which bypassed both the adapter system
 * and the lifecycle wait.
 */
async function executeNavigateAction(
  cel: Cel,
  action: Extract<SingleAction, { action: "navigate" }>,
): Promise<ActionExec> {
  await ensureCortexForCanonicalAction(cel);
  const result = await canonicalStep(cel, {
    purpose: `Navigate the focused browser tab to ${action.url}`,
    kind: "deterministic",
    action: {
      type: "navigate",
      url: action.url,
      wait_until: action.wait_until,
      timeout_ms: action.timeout_ms,
      dismiss_overlays: action.dismiss_overlays,
    },
  });
  if (result.status !== "ok") {
    throw new Error(result.message);
  }
  const data = (result.data ?? {}) as {
    final_url?: string;
    load_ms?: number;
    redirected?: boolean;
    dismissed_overlays?: boolean;
  };
  const finalUrl = data.final_url ?? action.url;
  const loadMs = typeof data.load_ms === "number" ? data.load_ms : 0;
  return {
    summary: `Navigated to ${action.url} (final: ${finalUrl}, ${loadMs}ms)`,
    receipt: receiptOf(result),
  };
}

/**
 * Route a `cel_act window` op through the canonical pipeline →
 * `PlannedAction::Window` → cortex `dispatch_window` (cel-accessibility AX).
 * The receipt carries the window geometry read back afterward. WS2.
 */
async function executeWindowAction(
  cel: Cel,
  action: Extract<SingleAction, { action: "window" }>,
): Promise<ActionExec> {
  await ensureCortexForCanonicalAction(cel);
  const result = await canonicalStep(cel, {
    purpose: action.preset ? `Window preset ${action.preset}` : `Window ${action.op ?? "op"}`,
    kind: "deterministic",
    action: {
      type: "window",
      // `op` is required by the contract; for preset-only calls it is ignored
      // (the cortex resolves the preset before matching op).
      op: action.op ?? "set_bounds",
      app: action.app ?? null,
      window_index: action.window_index ?? 0,
      x: action.x ?? null,
      y: action.y ?? null,
      width: action.width ?? null,
      height: action.height ?? null,
      preset: action.preset ?? null,
      display: action.display ?? null,
    },
  });
  if (result.status !== "ok") {
    throw new Error(result.message);
  }
  const g = (result.data ?? {}) as {
    x?: number;
    y?: number;
    width?: number;
    height?: number;
    minimized?: boolean;
  };
  const what = action.preset ? `preset ${action.preset}` : (action.op ?? "op");
  const summary =
    `Window ${what} → ${Math.round(g.x ?? 0)},${Math.round(g.y ?? 0)} ` +
    `${Math.round(g.width ?? 0)}×${Math.round(g.height ?? 0)}` +
    (g.minimized ? " (minimized)" : "");
  return { summary, receipt: receiptOf(result) };
}

/**
 * Route a `cel_act dialog` op through the canonical pipeline →
 * `PlannedAction::Dialog` → cortex `dispatch_dialog` (AX tree). WS5.
 */
async function executeDialogAction(
  cel: Cel,
  action: Extract<SingleAction, { action: "dialog" }>,
): Promise<ActionExec> {
  await ensureCortexForCanonicalAction(cel);
  const result = await canonicalStep(cel, {
    purpose: `Dialog ${action.op}${action.button ? ` "${action.button}"` : ""}`,
    kind: "deterministic",
    action: {
      type: "dialog",
      op: action.op,
      button: action.button ?? null,
      value: action.value ?? null,
      field_index: action.field_index ?? 0,
    },
  });
  if (result.status !== "ok") {
    throw new Error(result.message);
  }
  if (action.op === "list") {
    const d = (result.data ?? {}) as { buttons?: string[]; fields?: string[] };
    return {
      summary: `Dialog: buttons [${(d.buttons ?? []).join(", ")}]; fields [${(d.fields ?? []).join(" | ")}]`,
      receipt: receiptOf(result),
    };
  }
  return {
    summary: `Dialog ${action.op}${action.button ? ` "${action.button}"` : ""} ok`,
    receipt: receiptOf(result),
  };
}

/**
 * Route a `cel_act dock` op through the canonical pipeline →
 * `PlannedAction::Dock` → cortex `dispatch_dock`. WS6.
 */
async function executeDockAction(
  cel: Cel,
  action: Extract<SingleAction, { action: "dock" }>,
): Promise<ActionExec> {
  await ensureCortexForCanonicalAction(cel);
  const result = await canonicalStep(cel, {
    purpose: `Dock ${action.op}${action.name ? ` "${action.name}"` : ""}`,
    kind: "deterministic",
    action: {
      type: "dock",
      op: action.op,
      name: action.name ?? null,
    },
  });
  if (result.status !== "ok") {
    throw new Error(result.message);
  }
  if (action.op === "list") {
    const d = (result.data ?? {}) as { items?: string[] };
    return {
      summary: `Dock items: [${(d.items ?? []).join(", ")}]`,
      receipt: receiptOf(result),
    };
  }
  return {
    summary: `Dock ${action.op}${action.name ? ` "${action.name}"` : ""} ok`,
    receipt: receiptOf(result),
  };
}

/**
 * Route a `cel_act menu_extra` op through the canonical pipeline →
 * `PlannedAction::MenuExtra` → cortex `dispatch_menu_extra`. WS7.
 */
async function executeMenuExtraAction(
  cel: Cel,
  action: Extract<SingleAction, { action: "menu_extra" }>,
): Promise<ActionExec> {
  await ensureCortexForCanonicalAction(cel);
  const result = await canonicalStep(cel, {
    purpose: `MenuExtra ${action.op}${action.name ? ` "${action.name}"` : ""}`,
    kind: "deterministic",
    action: {
      type: "menu_extra",
      op: action.op,
      name: action.name ?? null,
    },
  });
  if (result.status !== "ok") {
    throw new Error(result.message);
  }
  if (action.op === "list") {
    const d = (result.data ?? {}) as { items?: string[] };
    return {
      summary: `Menu extras: [${(d.items ?? []).join(", ")}]`,
      receipt: receiptOf(result),
    };
  }
  return {
    summary: `MenuExtra ${action.op}${action.name ? ` "${action.name}"` : ""} ok`,
    receipt: receiptOf(result),
  };
}

/**
 * Route a generic `adapter_action` through the canonical pipeline. Any
 * adapter registered with the cortex that declares the requested op in
 * its manifest will be dispatched. Auto-verification runs after mutating
 * ops unless `params.verify === false`.
 *
 * This is the "every new adapter just works over MCP" entry point —
 * adapters add value by registering, not by adding MCP schema variants
 * per operation.
 */
async function executeAdapterAction(
  cel: Cel,
  action: Extract<SingleAction, { action: "adapter_action" }>,
): Promise<ActionExec> {
  await ensureCortexForCanonicalAction(cel);
  const params = (action.params ?? {}) as Record<string, unknown>;
  const result = await canonicalStep(cel, {
    purpose: `Adapter ${action.adapter}.${action.adapter_op}`,
    kind: "deterministic",
    action: {
      type: "custom",
      adapter: action.adapter,
      action: action.adapter_op,
      params,
    },
  });
  if (result.status !== "ok") {
    throw new Error(result.message);
  }
  const { data, receipt } = splitReceipt(result);
  return { summary: JSON.stringify(data, null, 2), receipt };
}

/**
 * Route an `ax_action` / `set_value` action whose `element_id` starts
 * with `dom:` through the canonical execution pipeline. The cortex's
 * `try_cdp_dispatch` (cortex.rs ~L1256) recognises `dom:*` targets and
 * routes them through CDP's JS-click / JS-set-value helpers — exactly
 * what we need for browser-DOM elements pumped by the Rust browser
 * adapter (PR #49). Returns `null` for non-`dom:` targets so the
 * caller falls through to the legacy AX-only path for `ax:*` ids.
 *
 * Without this routing, MCP `cel_act set_value dom:input:name "Alice"`
 * dispatches via `cel.axSetValue` which only knows the accessibility
 * tree and returns "Element not found" — defeating the planner-prompt
 * change in PR #50 that encourages using `set_value` against `dom:*`
 * targets in browser scenarios.
 */
async function tryRouteDomViaCanonical(
  cel: Cel,
  action: SingleAction,
): Promise<ActionExec | null> {
  if (action.action !== "ax_action" && action.action !== "set_value") {
    return null;
  }
  if (!action.element_id.startsWith("dom:")) {
    return null;
  }
  await ensureCortexForCanonicalAction(cel);
  const canonicalAction = action.action === "set_value"
    ? {
        type: "set_value" as const,
        target_id: action.element_id,
        value: action.value,
      }
    : {
        type: "ax_action" as const,
        target_id: action.element_id,
        action: action.ax_action,
        // Cortex's JS dispatch substring-matches the id_part — for
        // browser-DOM elements that's the HTML id/name, which is enough.
        // label/role_hint are AX-tree fallback signals that don't apply
        // to dom:* targets.
        label: null,
        role_hint: null,
      };
  const result = await canonicalStep(cel, {
    purpose: `${action.action} on ${action.element_id} via CDP`,
    kind: "deterministic",
    action: canonicalAction,
  });
  if (result.status !== "ok") {
    throw new Error(result.message);
  }
  const summary = action.action === "set_value"
    ? `Set value "${action.value}" on element ${action.element_id} (via CDP)`
    : `Performed ${action.ax_action} on element ${action.element_id} (via CDP)`;
  return { summary, receipt: receiptOf(result) };
}

async function executeAction(cel: Cel, action: SingleAction): Promise<ActionExec> {
  if (action.action === "focus_lock") {
    const focus = await ensureFrontmost(action.app_name, action.timeout_ms);
    if (!focus.matchesTarget) {
      throw new Error(
        `focus_lock failed: ${action.app_name} never became frontmost within ` +
          `${focus.elapsedMs}ms (still showing "${focus.frontmost}"). Lock NOT set.`,
      );
    }
    const previous = focusLock?.appName ?? null;
    focusLock = { appName: action.app_name, lockedAt: Date.now() };
    const replacedSuffix = previous && previous !== action.app_name
      ? `, replaced previous lock on ${previous}`
      : "";
    return {
      summary: focus.activated
        ? `Focus locked to ${action.app_name} (activated in ${focus.elapsedMs}ms, was ${focus.previousFrontmost}${replacedSuffix})`
        : `Focus locked to ${action.app_name} (was already frontmost${replacedSuffix})`,
      receipt: null,
    };
  }
  if (action.action === "focus_release") {
    const previous = focusLock?.appName ?? null;
    focusLock = null;
    return {
      summary: previous ? `Focus released (was locked to ${previous})` : "Focus released (no active lock)",
      receipt: null,
    };
  }
  if (action.action === "cdp_eval") {
    const result = await cel.cdpEvaluate(action.expression);
    const resultStr = result === undefined || result === null ? "void" : JSON.stringify(result);
    return { summary: `CDP eval result: ${resultStr}`, receipt: null };
  }
  if (action.action === "navigate") {
    return executeNavigateAction(cel, action);
  }
  if (action.action === "write_cells" || action.action === "read_cells") {
    return executeSpreadsheetAction(cel, action);
  }
  if (action.action === "adapter_action") {
    return executeAdapterAction(cel, action);
  }
  if (action.action === "window") {
    return executeWindowAction(cel, action);
  }
  if (action.action === "dialog") {
    return executeDialogAction(cel, action);
  }
  if (action.action === "dock") {
    return executeDockAction(cel, action);
  }
  if (action.action === "menu_extra") {
    return executeMenuExtraAction(cel, action);
  }
  // Browser DOM elements (`dom:*` ids from the Rust browser adapter,
  // PR #49) route through the cortex's CDP dispatch, not the AX tree.
  // Run this BEFORE the focus-sensitive check below — `dom:*` only
  // applies to `ax_action` / `set_value`, neither of which goes through
  // CGEventPost, so the focus race doesn't apply to them.
  const domRouted = await tryRouteDomViaCanonical(cel, action);
  if (domRouted !== null) {
    return domRouted;
  }
  // Every focus-sensitive action variant — the three keystroke variants and
  // the six coord-based variants — dispatches via CGEventPost, which routes
  // to the system-frontmost window. If the caller supplied target_app OR a
  // focus_lock is active, bring the target frontmost first so the action
  // lands where they meant it to. Explicit target_app on the action wins
  // over the lock (lock is a sequence-level default; per-action target_app
  // is a deliberate one-off override).
  if (FOCUS_SENSITIVE_ACTIONS.has(action.action)) {
    const explicitTarget =
      "target_app" in action && action.target_app ? action.target_app : undefined;
    const effectiveTarget = explicitTarget ?? focusLock?.appName;
    const focusMode = "focus_mode" in action ? action.focus_mode : undefined;

    // WS1: background (non-focus-stealing) path. Post directly to the target
    // app's PID without activating it. Requires a resolvable target; falls back
    // to the foreground path below when the PID can't be resolved or the action
    // has no background variant (scroll/drag/mouse_move).
    if (focusMode === "background" && effectiveTarget) {
      const pid = cel.pidForApp(effectiveTarget);
      if (pid != null) {
        const bg = executeSingleBackground(cel, action, pid);
        if (bg != null) {
          return {
            summary: `${bg} (focus: background → ${effectiveTarget} pid ${pid}, not activated)`,
            receipt: null,
          };
        }
      }
      // pid unresolved or no background variant → fall through to foreground.
    }

    if (effectiveTarget) {
      const focus = await bringTargetFrontmost(effectiveTarget);
      const result = executeSingle(cel, action);
      const source = explicitTarget ? "target_app" : "focus_lock";
      if (focus?.activated) {
        return {
          summary: `${result} (focus[${source}]: activated ${focus.frontmost} in ${focus.elapsedMs}ms, was ${focus.previousFrontmost})`,
          receipt: null,
        };
      }
      return {
        summary: `${result} (focus[${source}]: ${focus?.frontmost} already frontmost)`,
        receipt: null,
      };
    }
  }
  return { summary: executeSingle(cel, action), receipt: null };
}

export async function handleCelAct(cel: Cel, rawArgs: unknown) {
  const parsed = celActSchema.safeParse(rawArgs);
  if (!parsed.success) {
    return errorResult(`Invalid cel_act arguments: ${parsed.error.message}`);
  }
  const args: Input = parsed.data;

  const denied = axPermissionGuard(cel);
  if (denied) return denied;

  if ("actions" in args) {
    const results: string[] = [];
    const receipts: ActionReceipt[] = [];
    const delay = args.delay_between_ms ?? 100;
    for (let i = 0; i < args.actions.length; i++) {
      const action = args.actions[i];
      const requestedAtMs = Date.now();
      try {
        const exec = await executeAction(cel, action);
        results.push(exec.summary);
        receipts.push(buildReceipt(action, requestedAtMs, "ok", exec.summary, undefined, exec.receipt));
      } catch (err) {
        const error = err instanceof Error ? err.message : String(err);
        receipts.push(buildReceipt(action, requestedAtMs, "error", error, error));
        return receiptErrorResult({ success: false, error, results, receipts });
      }
      if (i < args.actions.length - 1 && delay > 0) {
        await sleep(delay);
      }
    }
    return textResult({ success: true, results, receipts });
  }

  const action = args as SingleAction;
  const requestedAtMs = Date.now();
  try {
    const exec = await executeAction(cel, action);
    return textResult({
      success: true,
      result: exec.summary,
      receipt: buildReceipt(action, requestedAtMs, "ok", exec.summary, undefined, exec.receipt),
    });
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    return receiptErrorResult({
      success: false,
      error,
      receipt: buildReceipt(action, requestedAtMs, "error", error, error),
    });
  }
}
