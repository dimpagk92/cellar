import { z } from "zod";
import type { Cel } from "@cellar/agent";
import { sleep, resolveCoords, contextReferenceSchema, textResult, errorResult, axPermissionGuard } from "./shared.js";
import { ensureFrontmost } from "../helpers/focus.js";

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
  "ax_action",
  "set_value",
  "activate_app",
  "cdp_eval",
  "write_cells",
  "read_cells",
] as const;

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

/**
 * Optional target_app field. Spread into every action variant whose dispatch
 * path is focus-sensitive (CGEventPost-based).
 */
const targetAppField = {
  target_app: z.string().optional().describe(targetAppDescription),
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
    action: z.literal("drag"),
    from_x: z.number().describe("Start X coordinate"),
    from_y: z.number().describe("Start Y coordinate"),
    to_x: z.number().describe("End X coordinate"),
    to_y: z.number().describe("End Y coordinate"),
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
    action: z.literal("cdp_eval"),
    expression: z
      .string()
      .describe(
        "JavaScript to execute in the browser page via Chrome DevTools Protocol. " +
          "Use document.querySelector() to find elements, .click() to click, .value= to set values. " +
          "Works inside iframes and on elements invisible to the accessibility tree (cookie banners, overlays). " +
          "Requires Chrome running with --remote-debugging-port. " +
          "Example: document.querySelectorAll('button').forEach(b => { if(b.textContent.includes('Accept')) b.click() })",
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
  })
  .passthrough();

type SingleAction = z.infer<typeof singleActionSchema>;
type Input = z.infer<typeof celActSchema>;

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
      cel.typeText(action.text);
      return `Typed "${action.text}"`;
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
    case "drag":
      cel.drag(action.from_x, action.from_y, action.to_x, action.to_y);
      return `Dragged from (${action.from_x}, ${action.from_y}) to (${action.to_x}, ${action.to_y})`;
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
    case "cdp_eval": {
      // cdp_eval is async — handled separately in handleCelAct
      throw new Error("cdp_eval must be handled async");
    }
    case "write_cells":
    case "read_cells":
      throw new Error(`${action.action} must be handled async`);
  }
}

async function ensureCortexForCanonicalAction(cel: Cel): Promise<void> {
  if (!cel.isCortexRunning()) {
    cel.bootCortex();
    await sleep(700);
  }
}

async function executeSpreadsheetAction(
  cel: Cel,
  action: Extract<SingleAction, { action: "write_cells" | "read_cells" }>,
): Promise<string> {
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

  const result = await cel.canonicalExecuteStep({
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
  return JSON.stringify(result.data ?? {}, null, 2);
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
): Promise<string | null> {
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
  const result = await cel.canonicalExecuteStep({
    purpose: `${action.action} on ${action.element_id} via CDP`,
    kind: "deterministic",
    action: canonicalAction,
  });
  if (result.status !== "ok") {
    throw new Error(result.message);
  }
  return action.action === "set_value"
    ? `Set value "${action.value}" on element ${action.element_id} (via CDP)`
    : `Performed ${action.ax_action} on element ${action.element_id} (via CDP)`;
}

async function executeAction(cel: Cel, action: SingleAction): Promise<string> {
  if (action.action === "cdp_eval") {
    const result = await cel.cdpEvaluate(action.expression);
    const resultStr = result === undefined || result === null ? "void" : JSON.stringify(result);
    return `CDP eval result: ${resultStr}`;
  }
  if (action.action === "write_cells" || action.action === "read_cells") {
    return executeSpreadsheetAction(cel, action);
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
  // to the system-frontmost window. If the caller supplied target_app, bring
  // it frontmost first so the action lands where they meant it to.
  if (FOCUS_SENSITIVE_ACTIONS.has(action.action) && "target_app" in action && action.target_app) {
    const focus = await bringTargetFrontmost(action.target_app);
    const result = executeSingle(cel, action);
    if (focus?.activated) {
      return `${result} (focus: activated ${focus.frontmost} in ${focus.elapsedMs}ms, was ${focus.previousFrontmost})`;
    }
    return `${result} (focus: ${focus?.frontmost} already frontmost)`;
  }
  return executeSingle(cel, action);
}

export async function handleCelAct(cel: Cel, rawArgs: unknown) {
  const parsed = celActSchema.safeParse(rawArgs);
  if (!parsed.success) {
    return errorResult(`Invalid cel_act arguments: ${parsed.error.message}`);
  }
  const args: Input = parsed.data;

  const denied = axPermissionGuard(cel);
  if (denied) return denied;
  try {
    if ("actions" in args) {
      const results: string[] = [];
      const delay = args.delay_between_ms ?? 100;
      for (let i = 0; i < args.actions.length; i++) {
        results.push(await executeAction(cel, args.actions[i]));
        if (i < args.actions.length - 1 && delay > 0) {
          await sleep(delay);
        }
      }
      return textResult({ success: true, results });
    } else {
      const result = await executeAction(cel, args as SingleAction);
      return textResult({ success: true, result });
    }
  } catch (err) {
    return errorResult(err instanceof Error ? err.message : String(err));
  }
}
