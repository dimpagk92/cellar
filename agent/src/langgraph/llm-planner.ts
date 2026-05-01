import { compressContext } from "../context-compressor.js";
import { serializeContextForLLM } from "../context-serializer.js";
import type { Cel } from "../cel-bindings.js";
import type {
  AttemptRecord,
  CanonicalAction,
  CanonicalStep,
  DoneVerdict,
  NextMove,
  PerceptionFrame,
} from "./canonical.js";
import type { CellarLangGraphPlanner } from "./planner.js";

const NEXT_MOVE_SYSTEM_PROMPT = `
You are the planning runtime for a desktop and browser automation agent.

Return ONLY raw JSON with exactly one of these shapes:

1. Batch
{
  "kind": "batch",
  "purpose": "short batch intent",
  "steps": [
    {
      "purpose": "what this step does",
      "kind": "deterministic" | "llm_assisted",
      "action": { ... }
    }
  ]
}

2. Done
{
  "kind": "done",
  "summary": "what was completed",
  "extracted_data": { ... }
}

3. Fail
{
  "kind": "fail",
  "reason": "why the goal cannot continue"
}

Allowed action shapes:
- {"type":"click","target_id":"12"}
- {"type":"type","target_id":"12","text":"hello"}
- {"type":"type","target_id":null,"text":"hello"}
- {"type":"set_value","target_id":"12","value":"hello"}
- {"type":"key","key":"Return"}
- {"type":"key_combo","keys":["Cmd","L"]}
- {"type":"scroll","dx":0,"dy":400}
- {"type":"wait","ms":800}
- {"type":"ax_action","target_id":"12","action":"click"}
- {"type":"activate_app","app_name":"Numbers"}
- {"type":"cdp_eval","expression":"document.body.innerText.slice(0,1000)"}
- {"type":"navigate","url":"https://example.com"}
- {"type":"extract_with_fallback","name":"price","selectors":[".price","[data-test='price']"],"parse_as":"float"}
- {"type":"write_cells","app":"Numbers","writes":[{"ref":"A1","value":"Ticker"},{"ref":"B1","value":"Price"}],"verify":true}

Rules:
- Keep batches small: usually 1-3 steps.
- Use the numbered element indices from the context as target_id values. Do not invent ids.
- If the app is a browser and CDP is available, prefer navigate and cdp_eval for in-page work.
- If the app is a desktop app, prefer ax_action, click, set_value, key, or activate_app.
- Only emit done when the current context or screenshot supports the claim.
- If unsure, do more work instead of declaring done.
- If genuinely blocked, emit fail with a concrete reason.
- Return JSON only. No markdown fences. No prose.
`.trim();

const VERIFY_DONE_SYSTEM_PROMPT = `
You are a strict completion checker for an automation agent.

Return ONLY raw JSON:
{
  "verified": true | false,
  "reason": "brief explanation"
}

Rules:
- verified=true only when the current context or screenshot clearly supports the claimed completion.
- If there is any important missing evidence, return verified=false.
- Be strict about partial completion.
- Return JSON only.
`.trim();

export interface CelLlmPlannerOptions {
  maxSteps?: number;
  maxTokens?: number;
  maxHistoryItems?: number;
  maxContextChars?: number;
}

type LlmSurface = Pick<Cel, "llmCompleteWithRole" | "llmCompleteWithImage">;

const DEFAULT_OPTIONS: Required<CelLlmPlannerOptions> = {
  maxSteps: 80,
  maxTokens: 4096,
  maxHistoryItems: 12,
  maxContextChars: 12000,
};

export class CelLlmPlanner implements CellarLangGraphPlanner {
  private readonly options: Required<CelLlmPlannerOptions>;

  constructor(
    private readonly llm: LlmSurface,
    options: CelLlmPlannerOptions = {},
  ) {
    this.options = { ...DEFAULT_OPTIONS, ...options };
  }

  async decideNext(input: {
    goal: string;
    history: AttemptRecord[];
    sharedMemory: unknown;
    frame: PerceptionFrame;
  }): Promise<NextMove> {
    if (input.history.length >= this.options.maxSteps) {
      return {
        kind: "fail",
        reason: `max_steps budget exhausted after ${input.history.length} executed steps`,
      };
    }

    const { prompt, indexMap } = this.buildNextMovePrompt(input);
    const raw = await this.complete(
      NEXT_MOVE_SYSTEM_PROMPT,
      prompt,
      input.frame.screenshot_base64 ?? null,
    );
    const parsed = parseJsonLoose(raw);
    return normalizeNextMove(parsed, indexMap);
  }

  async verifyDone(input: {
    goal: string;
    summary: string;
    sharedMemory: unknown;
    frame: PerceptionFrame;
  }): Promise<DoneVerdict> {
    const prompt = buildVerifyDonePrompt(input, this.options.maxContextChars);
    const raw = await this.complete(
      VERIFY_DONE_SYSTEM_PROMPT,
      prompt,
      input.frame.screenshot_base64 ?? null,
    );
    const parsed = parseJsonLoose(raw) as Partial<DoneVerdict>;
    return {
      verified: parsed.verified === true,
      reason: typeof parsed.reason === "string" ? parsed.reason : "",
    };
  }

  private async complete(
    systemPrompt: string,
    userPrompt: string,
    screenshotBase64: string | null,
  ): Promise<string> {
    if (screenshotBase64) {
      return this.llm.llmCompleteWithImage(
        systemPrompt,
        screenshotBase64,
        userPrompt,
        this.options.maxTokens,
      );
    }
    return this.llm.llmCompleteWithRole(
      systemPrompt,
      userPrompt,
      "planner",
      this.options.maxTokens,
    );
  }

  private buildNextMovePrompt(input: {
    goal: string;
    history: AttemptRecord[];
    sharedMemory: unknown;
    frame: PerceptionFrame;
  }): { prompt: string; indexMap: Map<number, string> } {
    const compressed = compressContext(input.frame.perception).context;
    const serialized = serializeContextForLLM(compressed);
    const historyText = formatHistory(input.history, this.options.maxHistoryItems);
    const contextText = truncate(serialized.text, this.options.maxContextChars);

    const prompt = [
      `GOAL`,
      input.goal,
      ``,
      `BUDGET`,
      `steps_used=${input.history.length}`,
      `max_steps=${this.options.maxSteps}`,
      ``,
      `RUNTIME CAPS`,
      safeJson({
        ...input.frame.caps,
        steps_used: input.history.length,
        max_steps: this.options.maxSteps,
      }),
      ``,
      `SHARED MEMORY`,
      safeJson(input.sharedMemory),
      ``,
      `RECENT HISTORY`,
      historyText,
      ``,
      `CURRENT CONTEXT`,
      contextText,
      ``,
      `IMPORTANT`,
      `Use the numeric element indices shown in CURRENT CONTEXT as target_id values.`,
      `If a screenshot is attached, use it as extra evidence for overlays, dialogs, and visual state.`,
    ].join("\n");

    return {
      prompt,
      indexMap: serialized.indexMap,
    };
  }
}

function buildVerifyDonePrompt(
  input: {
    goal: string;
    summary: string;
    sharedMemory: unknown;
    frame: PerceptionFrame;
  },
  maxContextChars: number,
): string {
  const compressed = compressContext(input.frame.perception).context;
  const serialized = serializeContextForLLM(compressed);
  return [
    `GOAL`,
    input.goal,
    ``,
    `CLAIMED SUMMARY`,
    input.summary,
    ``,
    `SHARED MEMORY`,
    safeJson(input.sharedMemory),
    ``,
    `RUNTIME CAPS`,
    safeJson(input.frame.caps),
    ``,
    `CURRENT CONTEXT`,
    truncate(serialized.text, maxContextChars),
    ``,
    `QUESTION`,
    `Does the current state prove the goal is complete?`,
  ].join("\n");
}

function formatHistory(history: AttemptRecord[], maxItems: number): string {
  if (history.length === 0) {
    return `(none)`;
  }
  return history
    .slice(-maxItems)
    .map((item, idx) => {
      const status = item.succeeded ? "ok" : `fail: ${item.error ?? "unknown"}`;
      return `${idx + 1}. ${item.step_purpose} :: ${status} :: ${formatAction(item.action)}`;
    })
    .join("\n");
}

function formatAction(action: CanonicalAction): string {
  try {
    return JSON.stringify(action);
  } catch {
    return action.type;
  }
}

function safeJson(value: unknown): string {
  try {
    return JSON.stringify(value ?? null, null, 2);
  } catch {
    return "null";
  }
}

function truncate(text: string, maxChars: number): string {
  return text.length > maxChars ? `${text.slice(0, maxChars)}\n...[truncated]` : text;
}

function parseJsonLoose(raw: string): unknown {
  const cleaned = raw
    .trim()
    .replace(/^```json\s*/i, "")
    .replace(/^```\s*/i, "")
    .replace(/\s*```$/i, "")
    .trim();

  try {
    return JSON.parse(cleaned);
  } catch {
    const firstBrace = cleaned.indexOf("{");
    const lastBrace = cleaned.lastIndexOf("}");
    if (firstBrace >= 0 && lastBrace > firstBrace) {
      return JSON.parse(cleaned.slice(firstBrace, lastBrace + 1));
    }
    throw new Error(`Planner returned invalid JSON: ${raw.slice(0, 400)}`);
  }
}

function normalizeNextMove(raw: unknown, indexMap: Map<number, string>): NextMove {
  if (!raw || typeof raw !== "object") {
    throw new Error("Planner returned a non-object next move");
  }

  const input = raw as Record<string, unknown>;
  const inferredKind = typeof input.kind === "string"
    ? input.kind
    : inferKind(input);

  if (inferredKind === "done") {
    return {
      kind: "done",
      summary: typeof input.summary === "string" ? input.summary : "Goal completed",
      extracted_data: input.extracted_data ?? null,
    };
  }

  if (inferredKind === "fail") {
    return {
      kind: "fail",
      reason: typeof input.reason === "string" ? input.reason : "Planner reported failure",
    };
  }

  const rawSteps = Array.isArray(input.steps) ? input.steps : [];
  if (rawSteps.length === 0) {
    throw new Error("Planner returned a batch with no steps");
  }

  const steps = rawSteps
    .slice(0, 5)
    .map((step, index) => normalizeStep(step, indexMap, index));

  return {
    kind: "batch",
    purpose: typeof input.purpose === "string" ? input.purpose : "Execute the next small batch",
    steps,
  };
}

function inferKind(input: Record<string, unknown>): "batch" | "done" | "fail" {
  if ("summary" in input && !("steps" in input)) {
    return "done";
  }
  if ("reason" in input && !("steps" in input)) {
    return "fail";
  }
  return "batch";
}

function normalizeStep(
  raw: unknown,
  indexMap: Map<number, string>,
  index: number,
): CanonicalStep {
  if (!raw || typeof raw !== "object") {
    throw new Error(`Planner returned invalid step at index ${index}`);
  }
  const step = raw as Record<string, unknown>;
  const action = normalizeAction(step.action, indexMap);
  return {
    purpose: typeof step.purpose === "string" ? step.purpose : `Execute step ${index + 1}`,
    kind: step.kind === "llm_assisted" ? "llm_assisted" : "deterministic",
    action,
  };
}

function normalizeAction(raw: unknown, indexMap: Map<number, string>): CanonicalAction {
  if (!raw || typeof raw !== "object") {
    throw new Error("Planner returned an invalid action");
  }
  const action = structuredClone(raw as Record<string, unknown>) as Record<string, unknown>;
  if (typeof action.type !== "string") {
    throw new Error("Planner action is missing a type");
  }

  for (const field of ["target_id", "from_target_id", "to_target_id"] as const) {
    if (field in action && action[field] != null) {
      action[field] = resolveIndexedId(action[field], indexMap);
    }
  }

  if (Array.isArray(action.evidence_ids)) {
    action.evidence_ids = action.evidence_ids.map((id) => resolveIndexedId(id, indexMap));
  }

  if (Array.isArray(action.actions)) {
    action.actions = action.actions.map((nested) => normalizeAction(nested, indexMap));
  }

  return action as CanonicalAction;
}

function resolveIndexedId(value: unknown, indexMap: Map<number, string>): string {
  const raw = String(value);
  if (/^\d+$/.test(raw)) {
    return indexMap.get(Number(raw)) ?? raw;
  }
  return raw;
}
