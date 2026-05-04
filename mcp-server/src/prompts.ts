import { z } from "zod";
import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import type { GetPromptResult } from "@modelcontextprotocol/sdk/types.js";

/**
 * MCP prompts shipped by the CEL server.
 *
 * Prompts are static templates the host surfaces to the user as quick-start
 * commands. They expand into a sequence of suggested tool calls so the user
 * doesn't need to remember tool names or compose long instructions.
 *
 * All prompts are returned regardless of cel-napi availability — they're
 * static templates that don't touch the native module. This makes them a
 * useful smoke test for hosts running the server in schema-only mode.
 */

type TextMessage = {
  role: "user" | "assistant";
  content: { type: "text"; text: string };
};

const userMsg = (text: string): TextMessage => ({
  role: "user",
  content: { type: "text", text },
});

const assistantMsg = (text: string): TextMessage => ({
  role: "assistant",
  content: { type: "text", text },
});

type PromptDefinition = {
  name: string;
  title: string;
  description: string;
  argsSchema?: Record<string, z.ZodType>;
  build: (args: Record<string, string | undefined>) => GetPromptResult;
};

const PROMPTS: PromptDefinition[] = [
  {
    name: "cellar/setup-task",
    title: "Start a multi-step automation",
    description:
      "Boot Cortex with a goal and walk through the recommended perceive → see → act → feed loop. Use for any task that takes more than 3 observations.",
    argsSchema: {
      goal: z.string().describe("Natural-language goal — e.g. 'fill out the expense form in Concur'"),
    },
    build: (args) => {
      const goal = args.goal ?? "<describe your goal>";
      return {
        description: "Multi-step automation with Cortex",
        messages: [
          userMsg(
            `I want to use Cellar to: ${goal}\n\nFollow the recommended Cortex loop:\n` +
              `1. Call cel_perceive { mode: "start", goal: "${goal}", enable_suggestions: true }\n` +
              `2. Call cel_perceive { mode: "read" } to see the current mental model + suggestions\n` +
              `3. Pick the first action and execute via cel_act\n` +
              `4. Call cel_perceive { mode: "feed", action: "<verb>", target: "<id>" } to verify\n` +
              `5. Repeat 2–4. Use cel_perceive { mode: "checkpoint" } at phase boundaries.\n` +
              `6. End with cel_perceive { mode: "stop" } to flush the run summary.`,
          ),
        ],
      };
    },
  },

  {
    name: "cellar/inspect-app",
    title: "Identify the focused app and its accessibility surface",
    description:
      "Reads the frontmost window, lists visible monitors, and surfaces a high-fidelity view of the focused element so you can see what CEL is actually working with.",
    build: () => ({
      description: "Inspect the focused app",
      messages: [
        userMsg(
          `Inspect the currently focused app. Run these in order:\n` +
            `1. cel_see { mode: "context", filter: { detail: "summary" } } — get the app name + element count\n` +
            `2. cel_see { mode: "windows" } — list visible windows\n` +
            `3. cel_see { mode: "focused" } — high-fidelity detail for the focused element\n` +
            `4. cel_see { mode: "monitors" } — confirm the active scale_factor (matters for clicks on Retina)\n\n` +
            `Report: app name, window title, focused element type/role, and any scale-factor mismatches.`,
        ),
      ],
    }),
  },

  {
    name: "cellar/debug-hung-action",
    title: "Diagnose why an action didn't land",
    description:
      "When an action seemed to execute but the screen didn't update as expected, this walks through the Cortex feed outcome and recent diff trail to find the cause.",
    argsSchema: {
      action: z
        .string()
        .optional()
        .describe("What you tried to do — e.g. 'click the Submit button'"),
    },
    build: (args) => {
      const action = args.action ?? "<the action you tried>";
      return {
        description: "Diagnose a hung or no-op action",
        messages: [
          userMsg(
            `An action didn't land as expected: ${action}\n\nDiagnose:\n` +
              `1. cel_perceive { mode: "status" } — confirm Cortex is active and check confidence\n` +
              `2. cel_perceive { mode: "read" } — inspect recent diffs and anomalies\n` +
              `3. If a session is active, cel_perceive { mode: "feed", action: "${action}", target: "<id>" } returns landed/anomalies/side_effects for the most recent attempt\n` +
              `4. If the focused app changed unexpectedly, suspect focus instability — re-check cel_see { mode: "windows" }\n` +
              `5. For browser tasks, cel_see { mode: "cdp_status" } to confirm CDP is connected\n\n` +
              `Common causes: stale element_id (call make_reference for resilient handles), Retina coordinate mismatch (divide AX bounds by scale_factor), AX-hostile app (use structured app truth like write_cells for Numbers), focus snapped back to host process (osascript activate before keys).`,
          ),
          assistantMsg(
            `I'll start with cel_perceive status to check if Cortex is healthy, then read the recent mental model to see what the screen currently looks like vs. what you expected.`,
          ),
        ],
      };
    },
  },

  {
    name: "cellar/extract-table",
    title: "Pull a structured table from the screen",
    description:
      "Filter the screen context to table-shaped elements and return them as JSON. Works against any AX-friendly app; for Numbers prefer cel_act read_cells (deterministic).",
    argsSchema: {
      app_hint: z
        .string()
        .optional()
        .describe("Optional: the app you expect the table to be in (e.g. 'Mail', 'Safari')"),
    },
    build: (args) => {
      const appHint = args.app_hint ? ` (expected in ${args.app_hint})` : "";
      return {
        description: "Extract a table from the current screen",
        messages: [
          userMsg(
            `Extract the table currently visible on screen${appHint}.\n\n` +
              `Strategy:\n` +
              `1. cel_see { mode: "context", filter: { element_types: ["table", "row", "cell", "outline"], detail: "compact" } }\n` +
              `2. If the app is Numbers, prefer cel_act { action: "read_cells", range: "A1:Z100" } — deterministic, bypasses AX guessing\n` +
              `3. If the app is a browser, cel_see { mode: "cdp_page" } returns the full page text including table cells from the DOM\n` +
              `4. Group cells by row, return as JSON: { headers: [...], rows: [[...], ...] }\n\n` +
              `Watch for: virtualised rows that aren't in the AX tree (scroll first), merged cells (split by AX position), header detection (row 0 vs. role="columnheader").`,
          ),
        ],
      };
    },
  },

  {
    name: "cellar/run-numbers-write",
    title: "Write cells deterministically into Numbers",
    description:
      "Uses the Numbers structured app-truth path (cel_act write_cells) instead of AX text-injection — atomic, with optional readback verification.",
    argsSchema: {
      sheet: z.string().optional().describe("Sheet name (default: first visible sheet)"),
      cells: z
        .string()
        .describe(
          'Cells to write as JSON: {"A1":"Label","B1":42,"A2":"=SUM(B1:B10)"}. Strings, numbers, and formulas all work.',
        ),
    },
    build: (args) => {
      const sheet = args.sheet ?? "<active sheet>";
      const cells = args.cells ?? '{"A1":"value"}';
      return {
        description: "Write cells to Numbers via structured app truth",
        messages: [
          userMsg(
            `Write the following cells to Numbers (sheet: ${sheet}):\n${cells}\n\n` +
              `Use the deterministic path:\n` +
              `1. cel_see { mode: "windows" } — confirm a Numbers document is open and capture its window id\n` +
              `2. cel_act { action: "write_cells", sheet: "${sheet}", cells: ${cells}, verify: true } — atomic write with readback\n` +
              `3. If verify reports a mismatch, the document was probably read-only or the sheet name was wrong — re-check via cel_act { action: "read_cells", range: "A1:Z10" }\n\n` +
              `Why this beats AX typing: Numbers renders cells via Core Graphics, which is invisible to the accessibility tree. Typing into the visible cell often lands in the wrong cell or silently no-ops. write_cells goes through the document model.`,
          ),
        ],
      };
    },
  },
];

export function registerPrompts(server: McpServer): void {
  for (const def of PROMPTS) {
    if (def.argsSchema) {
      server.registerPrompt(
        def.name,
        {
          title: def.title,
          description: def.description,
          argsSchema: def.argsSchema,
        },
        (args) => def.build(args as Record<string, string | undefined>),
      );
    } else {
      server.registerPrompt(
        def.name,
        {
          title: def.title,
          description: def.description,
        },
        () => def.build({}),
      );
    }
  }
}

export const PROMPT_NAMES = PROMPTS.map((p) => p.name);
