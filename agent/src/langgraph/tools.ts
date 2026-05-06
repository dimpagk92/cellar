import { tool } from "@langchain/core/tools";
import { z } from "zod";

import type { PageContent } from "../types.js";
import type {
  CanonicalAction,
  CanonicalStep,
  PerceptionFrame,
  PlanningView,
} from "./canonical.js";
import type { CellarLangGraphDriver } from "./driver.js";
import { evaluateDraftAnswer, inferGoalContract } from "./goal-contract.js";

const SEE_DESCRIPTION = `
Read the current fused Cortex context.

Use this before the first act() call and again after every act() call.
The result includes a compact indexed UI snapshot. Numeric target ids in that
snapshot are valid only until the next see() call.
`.trim();

const ACT_DESCRIPTION = `
Execute exactly one canonical action through Cortex.

Arguments:
- purpose: short description of what this action is trying to achieve
- action: one canonical action object

Preferred action shapes:
- {"type":"click","target_id":"1"}
- {"type":"type","target_id":"1","text":"hello"}
- {"type":"type","target_id":null,"text":"hello"}
- {"type":"set_value","target_id":"1","value":"hello"}
- {"type":"key","key":"Return"}
- {"type":"key_combo","keys":["Cmd","L"]}
- {"type":"scroll","dx":0,"dy":400}
- {"type":"wait","ms":800}
- {"type":"ax_action","target_id":"1","action":"click"}
- {"type":"activate_app","app_name":"Numbers"}
- {"type":"cdp_eval","expression":"document.title"}
- {"type":"navigate","url":"https://example.com"}
- {"type":"extract_with_fallback","name":"price","selectors":[".price"],"parse_as":"float"}
- {"type":"write_cells","app":"Numbers","writes":[{"cell_ref":"A1","value":"Ticker"}],"verify":true}

Use numeric target ids from the latest see() result, or pass through an actual target id.
Do not use act() for done/fail. Finish by returning a final answer instead.
`.trim();

const DONE_CHECK_DESCRIPTION = `
Validate whether a draft final answer actually satisfies the goal contract.

Arguments:
- draft_answer: the exact final answer text you want to return

Use this immediately before returning the final answer. If verification fails,
continue working instead of claiming success.
`.trim();

export interface CellarToolSession {
  executedSteps: number;
  lastFrame: PerceptionFrame | null;
  lastIndexMap: Map<number, string>;
  lastPageContent: PageContent | null;
}

export interface CreateCortexToolsOptions {
  driver: CellarLangGraphDriver;
  session?: CellarToolSession;
  captureScreenshotOnSee?: boolean;
  maxContextChars?: number;
  maxActions?: number;
  goal?: string;
}

const SEE_SCHEMA = z.object({
  hint: z.string().optional().describe("Optional brief note about what you want to inspect."),
  capture_screenshot: z.boolean().optional().describe("Whether to capture a screenshot as part of the observation."),
});

const ACT_SCHEMA = z.object({
  purpose: z.string().min(1).describe("Short description of what this action is trying to achieve."),
  action: z.object({
    type: z.string().min(1),
  }).passthrough().describe("One canonical action object."),
});

const DONE_CHECK_SCHEMA = z.object({
  draft_answer: z.string().min(1).describe("The exact final answer draft you want to validate."),
});

export function createCellarToolSession(): CellarToolSession {
  return {
    executedSteps: 0,
    lastFrame: null,
    lastIndexMap: new Map(),
    lastPageContent: null,
  };
}

export function createCortexTools(options: CreateCortexToolsOptions) {
  const session = options.session ?? createCellarToolSession();
  const maxContextChars = options.maxContextChars ?? 12_000;
  const goalContract = options.goal ? inferGoalContract(options.goal) : null;

  const see = tool(async (input) => {
    const frame = await options.driver.perceive({
      captureScreenshot: input.capture_screenshot ?? options.captureScreenshotOnSee ?? false,
    });
    const model = options.driver.readModel?.() ?? null;
    const activeAdapters = Array.isArray(model?.activeAdapters)
      ? model.activeAdapters.filter((value): value is string => typeof value === "string" && value.trim().length > 0)
      : [];
    // PR1c: render the SAME PlanningView the canonical Rust planner uses,
    // built by the cortex selector. The TS-side compressContext +
    // serializeContextForLLM fork is gone; both surfaces converge here.
    const view = await options.driver.buildPlanningView(options.goal ?? "", {
      perception: frame.perception,
      caps: frame.caps,
    });
    const rendered = renderPlanningView(view, maxContextChars);
    const pageContent = await options.driver.getPageContent?.() ?? null;
    session.lastFrame = frame;
    session.lastIndexMap = rendered.indexMap;
    session.lastPageContent = pageContent;

    return JSON.stringify({
      app: frame.perception.app,
      window: frame.perception.window,
      timestamp_ms: frame.perception.timestamp_ms,
      active_adapters: activeAdapters,
      caps: {
        ...frame.caps,
        cdp_bound: frame.caps.cdp_bound || pageContent != null,
        steps_used: session.executedSteps,
        max_steps: options.maxActions ?? frame.caps.max_steps,
      },
      element_count: rendered.elementCount,
      context: rendered.text,
      page_content: pageContent ? renderPageContent(pageContent, Math.max(Math.floor(maxContextChars / 2), 2_000)) : null,
      note: "Use numeric target ids from this see() result in the next act() call. After any act(), call see() again.",
    }, null, 2);
  }, {
    name: "see",
    description: SEE_DESCRIPTION,
    schema: SEE_SCHEMA,
  });

  const act = tool(async (input) => {
    if (options.maxActions != null && session.executedSteps >= options.maxActions) {
      return JSON.stringify({
        status: "err",
        message: `max_steps budget exhausted after ${session.executedSteps} executed steps`,
        recoverable: false,
      }, null, 2);
    }

    try {
      const originalAction = input.action as unknown as CanonicalAction;
      if (originalAction.type === "done" || originalAction.type === "fail") {
        return JSON.stringify({
          status: "err",
          message: "Use a final answer instead of act() for done/fail outcomes",
          recoverable: false,
        }, null, 2);
      }

      const resolvedAction = resolveActionTargetIds(originalAction, session.lastIndexMap);
      const step: CanonicalStep = {
        purpose: input.purpose,
        kind: "llm_assisted",
        action: resolvedAction,
      };

      session.executedSteps += 1;
      const result = await options.driver.executeStep(step);

      return JSON.stringify({
        ...result,
        purpose: input.purpose,
        action: resolvedAction,
        steps_used: session.executedSteps,
      }, null, 2);
    } catch (error) {
      return JSON.stringify({
        status: "err",
        message: error instanceof Error ? error.message : String(error),
        recoverable: true,
        steps_used: session.executedSteps,
      }, null, 2);
    }
  }, {
    name: "act",
    description: ACT_DESCRIPTION,
    schema: ACT_SCHEMA,
  });

  const doneCheck = tool(async (input) => {
    const verdict = goalContract
      ? evaluateDraftAnswer(goalContract, input.draft_answer)
      : {
        verified: true,
        missing: [] as string[],
        reason: "No explicit structured goal contract was inferred from the task",
      };

    return JSON.stringify({
      ...verdict,
      steps_used: session.executedSteps,
    }, null, 2);
  }, {
    name: "done_check",
    description: DONE_CHECK_DESCRIPTION,
    schema: DONE_CHECK_SCHEMA,
  });

  return {
    see,
    act,
    doneCheck,
    session,
  };
}

/**
 * Render a `PlanningView` into the see-tool output shape (text + numeric
 * index map + element count).
 *
 * Numeric indices (1-based) are assigned in the view's element order so
 * the LLM can reference elements by short ids. The act() tool converts
 * those numeric ids back to real element ids via the returned indexMap.
 *
 * Replaces the legacy `renderPerception` (which ran TS-side compression).
 * Selection, budget enforcement, and capability folding all live in the
 * cortex now — this function is presentation only.
 */
function renderPlanningView(view: PlanningView, maxContextChars: number) {
  const indexMap = new Map<number, string>();
  const lines: string[] = [];

  lines.push(`APP: ${view.screen.active_app || "<none>"}`);
  if (view.screen.window) lines.push(`WINDOW: ${view.screen.window}`);
  if (view.screen.url) lines.push(`URL: ${view.screen.url}`);
  if (view.screen.summary) lines.push(`SUMMARY: ${view.screen.summary}`);
  lines.push("");
  lines.push("ELEMENTS (use the leading number as target_id in the next act):");

  view.elements.forEach((el, i) => {
    const idx = i + 1;
    indexMap.set(idx, el.id);
    const label = el.label ? ` ${JSON.stringify(el.label)}` : "";
    const value =
      el.value !== null && el.value !== undefined && el.value.length > 0
        ? ` value=${JSON.stringify(el.value)}`
        : "";
    const flags: string[] = [];
    if (el.state.focused) flags.push("focused");
    if (el.state.selected) flags.push("selected");
    if (el.state.checked) flags.push("checked");
    if (el.state.expanded) flags.push("expanded");
    if (!el.state.enabled) flags.push("disabled");
    const flagStr = flags.length > 0 ? ` [${flags.join(",")}]` : "";
    lines.push(
      `  ${idx}. [${el.element_type}]${label}${value}${flagStr}`,
    );
  });

  if (view.elements.length === 0) {
    lines.push("  (no elements selected by the planning view)");
  }

  if (view.omitted_counts.elements > 0) {
    lines.push(
      `  … ${view.omitted_counts.elements} more elements omitted to fit the planning budget.`,
    );
  }

  if (view.blockers.length > 0) {
    lines.push("");
    lines.push("BLOCKERS:");
    for (const b of view.blockers) {
      const ref = b.element_id ? ` (element ${b.element_id})` : "";
      lines.push(`  - [${b.kind}] ${b.description}${ref}`);
    }
  }

  if (view.adapter_facts.length > 0) {
    lines.push("");
    lines.push("ADAPTER FACTS:");
    for (const f of view.adapter_facts) {
      lines.push(
        `  - [${f.adapter}/${f.kind}] ${JSON.stringify(f.payload)}`,
      );
    }
  }

  return {
    text: truncate(lines.join("\n"), maxContextChars),
    indexMap,
    elementCount: view.elements.length,
  };
}

function resolveActionTargetIds(
  action: CanonicalAction,
  indexMap: Map<number, string>,
): CanonicalAction {
  switch (action.type) {
    case "click":
      return { ...action, target_id: resolveMaybeIndexedId(action.target_id, indexMap) };
    case "type":
      return {
        ...action,
        target_id: typeof action.target_id === "string"
          ? resolveMaybeIndexedId(action.target_id, indexMap)
          : action.target_id ?? null,
      };
    case "set_value":
      return { ...action, target_id: resolveMaybeIndexedId(action.target_id, indexMap) };
    case "drag":
      return {
        ...action,
        from_target_id: resolveMaybeIndexedId(action.from_target_id, indexMap),
        to_target_id: resolveMaybeIndexedId(action.to_target_id, indexMap),
      };
    case "ax_action":
      return { ...action, target_id: resolveMaybeIndexedId(action.target_id, indexMap) };
    case "batch":
      return {
        ...action,
        actions: action.actions.map((item) => resolveActionTargetIds(item, indexMap)),
      };
    default:
      return action;
  }
}

function resolveMaybeIndexedId(targetId: string, indexMap: Map<number, string>): string {
  const trimmed = targetId.trim();
  if (!/^\d+$/.test(trimmed)) {
    return targetId;
  }

  const resolved = indexMap.get(Number(trimmed));
  if (!resolved) {
    throw new Error(`No element id found for index ${trimmed}. Call see() again before acting.`);
  }
  return resolved;
}

function truncate(text: string, maxChars: number): string {
  return text.length > maxChars ? `${text.slice(0, maxChars)}\n...[truncated]` : text;
}

function renderPageContent(pageContent: PageContent, maxChars: number) {
  return {
    title: pageContent.title,
    url: pageContent.url,
    load_time_ms: pageContent.load_time_ms,
    dom_ready_ms: pageContent.dom_ready_ms,
    body_excerpt: truncate(pageContent.body_text, maxChars),
    text_blocks: pageContent.text_blocks
      .slice(0, 8)
      .map((block) => ({
        block_type: block.block_type,
        level: block.level,
        text: truncate(block.text, 240),
      })),
    interactive_elements: pageContent.interactive_elements
      .slice(0, 12)
      .map((element) => ({
        tag: element.tag,
        element_type: element.element_type,
        text: truncate(element.text, 120),
        href: element.href,
        value: element.value,
        placeholder: element.placeholder,
      })),
  };
}
