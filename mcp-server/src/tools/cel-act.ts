import { z } from "zod";
import type { Cel } from "@cellar/agent";
import { sleep, resolveCoords, contextReferenceSchema, textResult, errorResult, axPermissionGuard } from "./shared.js";

const coordActionBase = {
  x: z.number().optional().describe("X coordinate. Not needed if target_ref is provided."),
  y: z.number().optional().describe("Y coordinate. Not needed if target_ref is provided."),
  target_ref: contextReferenceSchema
    .optional()
    .describe(
      "Resilient element reference. If provided, CEL resolves the element and uses its center. " +
        "Get references from cel_see with mode 'make_reference'.",
    ),
};

const singleActionSchema = z.discriminatedUnion("action", [
  z.object({ action: z.literal("click"), ...coordActionBase }),
  z.object({ action: z.literal("right_click"), ...coordActionBase }),
  z.object({ action: z.literal("double_click"), ...coordActionBase }),
  z.object({ action: z.literal("mouse_move"), ...coordActionBase }),
  z.object({
    action: z.literal("type"),
    text: z.string().describe("Text to type using keyboard input"),
  }),
  z.object({
    action: z.literal("key_press"),
    key: z.string().describe("Key name (e.g. Enter, Tab, Escape, Backspace)"),
  }),
  z.object({
    action: z.literal("key_combo"),
    keys: z
      .array(z.string())
      .min(1)
      .describe("Key names for combination (e.g. ['Ctrl', 'C'], ['Cmd', 'V'])"),
  }),
  z.object({
    action: z.literal("scroll"),
    dx: z.number().default(0).describe("Horizontal scroll amount"),
    dy: z.number().default(0).describe("Vertical scroll amount (positive = down)"),
    x: z.number().optional().describe("Scroll at this X coordinate"),
    y: z.number().optional().describe("Scroll at this Y coordinate"),
  }),
  z.object({
    action: z.literal("drag"),
    from_x: z.number().describe("Start X coordinate"),
    from_y: z.number().describe("Start Y coordinate"),
    to_x: z.number().describe("End X coordinate"),
    to_y: z.number().describe("End Y coordinate"),
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

type SingleAction = z.infer<typeof singleActionSchema>;
type Input = z.infer<typeof celActSchema>;

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

async function executeAction(cel: Cel, action: SingleAction): Promise<string> {
  if (action.action === "cdp_eval") {
    const result = await cel.cdpEvaluate(action.expression);
    const resultStr = result === undefined || result === null ? "void" : JSON.stringify(result);
    return `CDP eval result: ${resultStr}`;
  }
  if (action.action === "write_cells" || action.action === "read_cells") {
    return executeSpreadsheetAction(cel, action);
  }
  return executeSingle(cel, action);
}

export async function handleCelAct(cel: Cel, args: Input) {
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
