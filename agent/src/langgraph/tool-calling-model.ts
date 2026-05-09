import { randomUUID } from "crypto";
import { spawn } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

import {
  AIMessage,
  type BaseMessage,
} from "@langchain/core/messages";
import {
  BaseChatModel,
  type BaseChatModelCallOptions,
  type BindToolsInput,
} from "@langchain/core/language_models/chat_models";
import type { ChatResult } from "@langchain/core/outputs";

import type { Cel } from "../cel-bindings.js";
import { celConfig, discoverClaudeCodeOauthTokens, hydrateLlmEnvFromConfig } from "../config.js";
import { inferGoalContract, renderGoalContract } from "./goal-contract.js";

const TOOL_CALLING_SYSTEM_PROMPT = `
You are the reasoning engine for a LangGraph agent that controls a computer through tools.

You do not directly perform actions. You either:
- call one tool, or
- return the final user-facing answer.

Rules:
- Call see() before the first act().
- After every act(), call see() again before deciding the next action.
- If done_check() is available, call it right before returning the final answer.
- Never invent target ids. Use only ids or numeric indices returned by the latest see() result.
- If a tool returns an error, inspect again instead of blindly repeating the same action.
- Only finish when the goal is complete or genuinely blocked.

Return ONLY raw JSON with exactly one of these shapes:
{"kind":"tool","name":"see","args":{"hint":"optional brief reason"},"thought":"optional short note"}
{"kind":"tool","name":"act","args":{"purpose":"what this action does","action":{"type":"...","...":"..."}},"thought":"optional short note"}
{"kind":"tool","name":"done_check","args":{"draft_answer":"exact final answer text"},"thought":"optional short note"}
{"kind":"final","content":"final answer for the user"}
`.trim();

export interface CelToolCallingChatModelOptions {
  maxTokens?: number;
  maxTranscriptChars?: number;
  role?: string;
}

export interface CelToolCallingCallOptions extends BaseChatModelCallOptions {
  tools?: BindToolsInput[];
}

type LlmSurface = Pick<Cel, "llmCompleteWithRole">;

interface ToolCallResponse {
  kind: "tool";
  name: string;
  args?: Record<string, unknown>;
  thought?: string;
}

interface FinalResponse {
  kind: "final";
  content: string;
}

type ModelResponse = ToolCallResponse | FinalResponse;

const DEFAULT_OPTIONS: Required<CelToolCallingChatModelOptions> = {
  maxTokens: 4096,
  maxTranscriptChars: 16_000,
  role: "planner",
};

let cachedClaudeCliPath: string | null | undefined;
const EMPTY_MCP_CONFIG = JSON.stringify({ mcpServers: {} });

export class CelToolCallingChatModel extends BaseChatModel<CelToolCallingCallOptions> {
  lc_namespace = ["cellar", "langgraph"];

  private readonly options: Required<CelToolCallingChatModelOptions>;

  constructor(
    private readonly llm: LlmSurface,
    options: CelToolCallingChatModelOptions = {},
  ) {
    super({ disableStreaming: true });
    this.options = {
      ...DEFAULT_OPTIONS,
      ...Object.fromEntries(
        Object.entries(options).filter(([, value]) => value !== undefined),
      ),
    };
  }

  _llmType(): string {
    return "cellar_tool_calling";
  }

  bindTools(tools: BindToolsInput[], kwargs: Partial<CelToolCallingCallOptions> = {}) {
    return this.bind({
      ...kwargs,
      tools,
    });
  }

  async _generate(
    messages: BaseMessage[],
    options: this["ParsedCallOptions"],
  ): Promise<ChatResult> {
    hydrateLlmEnvFromConfig();

    const userPrompt = buildUserPrompt(messages, options.tools ?? [], this.options.maxTranscriptChars);
    const raw = await this.complete(TOOL_CALLING_SYSTEM_PROMPT, userPrompt);

    const parsed = applyTranscriptGuardrails(
      normalizeModelResponse(parseJsonLoose(raw)),
      messages,
      options.tools ?? [],
    );
    const message = toAiMessage(parsed);

    return {
      generations: [
        {
          text: typeof message.content === "string" ? message.content : JSON.stringify(message.content),
          message,
        },
      ],
    };
  }

  private async complete(systemPrompt: string, userPrompt: string): Promise<string> {
    if (shouldUseClaudeCli()) {
      return completeWithClaudeCli(systemPrompt, userPrompt);
    }

    try {
      return await this.llm.llmCompleteWithRole(
        systemPrompt,
        userPrompt,
        this.options.role,
        this.options.maxTokens,
      );
    } catch (error) {
      if (shouldUseClaudeCli() && isRateLimitLikeError(error)) {
        return completeWithClaudeCli(systemPrompt, userPrompt);
      }
      throw error;
    }
  }
}

function buildUserPrompt(
  messages: BaseMessage[],
  tools: BindToolsInput[],
  maxTranscriptChars: number,
): string {
  const toolCatalog = tools.length > 0
    ? tools.map((tool) => renderTool(tool)).join("\n\n")
    : "(none)";

  const goal = extractPrimaryGoal(messages);
  const contract = goal ? inferGoalContract(goal) : null;
  const renderedContract = contract ? renderGoalContract(contract) : "(none)";

  return [
    "AVAILABLE TOOLS",
    toolCatalog,
    "",
    "TASK CONTRACT",
    renderedContract,
    "",
    "CONVERSATION TRANSCRIPT",
    truncateFromStart(renderTranscript(messages), maxTranscriptChars),
    "",
    "Respond with JSON only.",
  ].join("\n");
}

function renderTool(tool: BindToolsInput): string {
  const maybeTool = tool as {
    name?: string;
    description?: string;
  };
  const name = maybeTool.name ?? "unnamed_tool";
  const description = maybeTool.description?.trim() || "(no description)";
  return `- ${name}: ${description}`;
}

function renderTranscript(messages: BaseMessage[]): string {
  return messages
    .map((message, index) => `${index + 1}. ${renderMessage(message)}`)
    .join("\n\n");
}

function renderMessage(message: BaseMessage): string {
  const type = message._getType();
  const content = stringifyContent((message as { content?: unknown }).content);

  if (type === "ai") {
    const maybeAi = message as {
      tool_calls?: Array<{
        name?: string;
        args?: Record<string, unknown>;
      }>;
    };
    const toolCalls = maybeAi.tool_calls ?? [];
    const renderedCalls = toolCalls.map((call) => (
      `ASSISTANT TOOL CALL ${call.name ?? "unknown"} ${safeJson(call.args ?? {})}`
    ));
    const parts = [];
    if (content) {
      parts.push(`ASSISTANT ${content}`);
    }
    parts.push(...renderedCalls);
    return parts.join("\n");
  }

  if (type === "tool") {
    return `TOOL RESULT ${content}`;
  }

  if (type === "system") {
    return `SYSTEM ${content}`;
  }

  if (type === "human") {
    return `USER ${content}`;
  }

  return `${type.toUpperCase()} ${content}`;
}

function stringifyContent(content: unknown): string {
  if (typeof content === "string") {
    return content;
  }
  if (Array.isArray(content)) {
    return content.map((item) => {
      if (typeof item === "string") {
        return item;
      }
      if (item && typeof item === "object" && "text" in item && typeof item.text === "string") {
        return item.text;
      }
      return safeJson(item);
    }).join(" ");
  }
  if (content == null) {
    return "";
  }
  return safeJson(content);
}

function truncateFromStart(text: string, maxChars: number): string {
  if (text.length <= maxChars) {
    return text;
  }
  return `...[older transcript truncated]\n${text.slice(-maxChars)}`;
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
    const extracted = extractFirstJsonValue(cleaned);
    if (extracted) {
      return JSON.parse(extracted);
    }
    throw new Error(`Tool-calling model returned invalid JSON: ${raw.slice(0, 400)}`);
  }
}

function extractFirstJsonValue(text: string): string | null {
  const start = text.search(/[\[{]/);
  if (start < 0) {
    return null;
  }

  const stack: string[] = [];
  let inString = false;
  let escaped = false;

  for (let index = start; index < text.length; index += 1) {
    const char = text[index];

    if (inString) {
      if (escaped) {
        escaped = false;
        continue;
      }
      if (char === "\\") {
        escaped = true;
        continue;
      }
      if (char === "\"") {
        inString = false;
      }
      continue;
    }

    if (char === "\"") {
      inString = true;
      continue;
    }

    if (char === "{" || char === "[") {
      stack.push(char === "{" ? "}" : "]");
      continue;
    }

    if (char === "}" || char === "]") {
      const expected = stack.pop();
      if (expected !== char) {
        return null;
      }
      if (stack.length === 0) {
        return text.slice(start, index + 1);
      }
    }
  }

  return null;
}

function normalizeModelResponse(raw: unknown): ModelResponse {
  if (typeof raw === "string") {
    return {
      kind: "final",
      content: raw,
    };
  }

  if (!raw || typeof raw !== "object") {
    throw new Error("Tool-calling model returned a non-object response");
  }

  const input = raw as Record<string, unknown>;
  const inferredKind = typeof input.kind === "string"
    ? normalizeKind(input.kind)
    : inferKind(input);

  if (inferredKind === "tool") {
    const toolName = typeof input.name === "string" && input.name.length > 0
      ? input.name
      : (typeof input.kind === "string" ? normalizeToolNameFromKind(input.kind) : null);
    if (!toolName) {
      throw new Error("Tool response is missing a valid tool name");
    }
    return {
      kind: "tool",
      name: toolName,
      args: isRecord(input.args) ? input.args : {},
      thought: typeof input.thought === "string" ? input.thought : "",
    };
  }

  if (inferredKind === "final") {
    return {
      kind: "final",
      content: typeof input.content === "string"
        ? input.content
        : typeof input.answer === "string"
          ? input.answer
          : safeJson(input),
    };
  }

  throw new Error(`Unknown tool-calling response kind: ${safeJson(raw)}`);
}

function applyTranscriptGuardrails(
  response: ModelResponse,
  messages: BaseMessage[],
  tools: BindToolsInput[],
): ModelResponse {
  const state = inspectTranscript(messages);
  const hasDoneCheck = tools.some((tool) => {
    const maybeTool = tool as { name?: string };
    return maybeTool.name === "done_check";
  });

  if (state.mustSeeBeforeNextDecision) {
    if (response.kind === "final" || response.name !== "see") {
      return forceSeeResponse(
        response.kind === "final"
          ? "Need a fresh observation before answering"
          : "Need a fresh observation before acting again",
      );
    }
  }

  if (response.kind === "final" && hasDoneCheck) {
    if (state.lastToolName === "done_check" && state.lastDoneCheckVerified === true) {
      return response;
    }
    return forceDoneCheckResponse(response.content);
  }

  return response;
}

function inspectTranscript(messages: BaseMessage[]) {
  let sawToolResult = false;
  let lastToolName: string | null = null;
  let lastDoneCheckVerified: boolean | null = null;

  for (const message of messages) {
    if (message._getType() !== "tool") {
      continue;
    }
    sawToolResult = true;
    const maybeTool = message as { name?: string; content?: unknown };
    lastToolName = typeof maybeTool.name === "string" ? maybeTool.name : null;
    lastDoneCheckVerified = lastToolName === "done_check"
      ? getBooleanFieldFromToolResult(maybeTool.content, "verified")
      : null;
  }

  return {
    sawToolResult,
    lastToolName,
    lastDoneCheckVerified,
    mustSeeBeforeNextDecision: !sawToolResult
      || lastToolName === "act"
      || (lastToolName === "done_check" && lastDoneCheckVerified === false),
  };
}

function forceSeeResponse(thought: string): ToolCallResponse {
  return {
    kind: "tool",
    name: "see",
    args: {
      hint: "runtime guardrail requested a fresh observation",
    },
    thought,
  };
}

function forceDoneCheckResponse(draftAnswer: string): ToolCallResponse {
  return {
    kind: "tool",
    name: "done_check",
    args: {
      draft_answer: draftAnswer,
    },
    thought: "Validate the draft answer against the goal contract before finishing",
  };
}

function inferKind(input: Record<string, unknown>): "tool" | "final" {
  if (typeof input.kind === "string" && normalizeToolNameFromKind(input.kind)) {
    return "tool";
  }
  if (typeof input.name === "string" && ("args" in input || "tool" in input)) {
    return "tool";
  }
  return "final";
}

function normalizeKind(kind: string): "tool" | "final" | "unknown" {
  if (kind === "tool" || kind === "final") {
    return kind;
  }
  if (normalizeToolNameFromKind(kind)) {
    return "tool";
  }
  return "unknown";
}

function normalizeToolNameFromKind(kind: string): string | null {
  switch (kind) {
    case "see":
    case "act":
    case "done_check":
      return kind;
    default:
      return null;
  }
}

function toAiMessage(response: ModelResponse): AIMessage {
  if (response.kind === "tool") {
    return new AIMessage({
      content: response.thought ?? "",
      tool_calls: [
        {
          id: randomUUID(),
          name: response.name,
          args: response.args ?? {},
          type: "tool_call",
        },
      ],
    });
  }

  return new AIMessage({
    content: response.content,
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function safeJson(value: unknown): string {
  try {
    return JSON.stringify(value ?? null, null, 2);
  } catch {
    return "null";
  }
}

function extractPrimaryGoal(messages: BaseMessage[]): string {
  const firstHuman = messages.find((message) => message._getType() === "human");
  if (!firstHuman) {
    return "";
  }
  return stringifyContent((firstHuman as { content?: unknown }).content).trim();
}

function getBooleanFieldFromToolResult(content: unknown, field: string): boolean | null {
  const text = stringifyContent(content).trim();
  if (!text) {
    return null;
  }
  try {
    const parsed = parseJsonLoose(text);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      const value = (parsed as Record<string, unknown>)[field];
      return typeof value === "boolean" ? value : null;
    }
  } catch {
    // Best-effort only.
  }
  return null;
}

function isRateLimitLikeError(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return /\b429\b|rate[_ -]?limit/i.test(message);
}

function shouldUseClaudeCli(): boolean {
  // WK5: under vitest, an injected `llmCompleteWithRole` mock is the
  // entire point of the test — silently shelling out to the real
  // `claude` CLI (when the dev's machine has Claude Code installed
  // and no API key configured) bypasses the mock, hangs for 5s waiting
  // on the subprocess, and times out. Skip the CLI fallback in any
  // vitest worker so the mock always wins. Production runs (no
  // VITEST env vars set) keep the CLI fallback intact.
  if (process.env.VITEST || process.env.VITEST_WORKER_ID) {
    return false;
  }
  return (
    celConfig.llmProvider === "anthropic" &&
    discoverClaudeCliOauthTokens().length > 0 &&
    !process.env.CEL_LLM_API_KEY &&
    !process.env.ANTHROPIC_API_KEY &&
    Boolean(discoverClaudeCliPath())
  );
}

async function completeWithClaudeCli(
  systemPrompt: string,
  userPrompt: string,
): Promise<string> {
  const claudePath = discoverClaudeCliPath();
  const oauthTokens = discoverClaudeCliOauthTokens();

  if (!claudePath || oauthTokens.length === 0) {
    throw new Error("Claude Code CLI fallback is not available");
  }

  const args = [
    "-p",
    "--output-format",
    "text",
    "--tools",
    "",
    "--strict-mcp-config",
    "--mcp-config",
    EMPTY_MCP_CONFIG,
    "--system-prompt",
    systemPrompt,
    userPrompt,
  ];

  let lastError: Error | null = null;

  for (const oauthToken of oauthTokens) {
    try {
      return await runClaudeCli(claudePath, args, oauthToken);
    } catch (error) {
      lastError = error instanceof Error ? error : new Error(String(error));
      if (!/\b401\b|authentication/i.test(lastError.message)) {
        throw lastError;
      }
    }
  }

  throw lastError ?? new Error("Claude CLI fallback failed: no usable OAuth token found");
}

async function runClaudeCli(
  claudePath: string,
  args: string[],
  oauthToken: string,
): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const child = spawn(
      claudePath,
      args,
      {
        env: {
          ...process.env,
          CLAUDE_CODE_OAUTH_TOKEN: oauthToken,
        },
      },
    );

    let stdout = "";
    let stderr = "";
    let settled = false;
    const timeout = setTimeout(() => {
      if (settled) {
        return;
      }
      settled = true;
      child.kill("SIGTERM");
      reject(new Error("Claude CLI fallback failed: timed out after 120000ms"));
    }, 120_000);

    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
    });

    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => {
      stderr += chunk;
    });

    child.on("error", (error) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timeout);
      reject(new Error(`Claude CLI fallback failed: ${error.message}`));
    });

    child.on("close", (code) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timeout);

      const trimmedStdout = stdout.trim();
      const trimmedStderr = stderr.trim();
      if (code !== 0) {
        reject(new Error(`Claude CLI fallback failed: ${(trimmedStderr || trimmedStdout || `exit code ${code}`).trim()}`));
        return;
      }
      resolve(trimmedStdout);
    });

    child.stdin.end();
  });
}

function discoverClaudeCliOauthTokens(): string[] {
  const tokens = [];

  if (process.env.CLAUDE_CODE_OAUTH_TOKEN) {
    tokens.push(process.env.CLAUDE_CODE_OAUTH_TOKEN);
  }

  for (const token of discoverClaudeCodeOauthTokens()) {
    if (!tokens.includes(token)) {
      tokens.push(token);
    }
  }

  return tokens;
}

function discoverClaudeCliPath(): string | undefined {
  if (cachedClaudeCliPath !== undefined) {
    return cachedClaudeCliPath ?? undefined;
  }

  try {
    const base = join(
      homedir(),
      "Library",
      "Application Support",
      "Claude",
      "claude-code",
    );
    if (!existsSync(base)) {
      cachedClaudeCliPath = null;
      return undefined;
    }

    const versions = readdirSync(base, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => entry.name)
      .sort()
      .reverse();

    for (const version of versions) {
      const candidate = join(
        base,
        version,
        "claude.app",
        "Contents",
        "MacOS",
        "claude",
      );
      if (existsSync(candidate)) {
        cachedClaudeCliPath = candidate;
        return candidate;
      }
    }
  } catch {
    // Best-effort only.
  }

  cachedClaudeCliPath = null;
  return undefined;
}
