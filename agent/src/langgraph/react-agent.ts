import type { BaseMessage } from "@langchain/core/messages";
import { MemorySaver } from "@langchain/langgraph";
import { createReactAgent } from "@langchain/langgraph/prebuilt";

import type { Cel } from "../cel-bindings.js";
import type { CellarLangGraphDriver } from "./driver.js";
import { CelToolCallingChatModel, type CelToolCallingChatModelOptions } from "./tool-calling-model.js";
import { createCortexTools, type CreateCortexToolsOptions } from "./tools.js";

const DEFAULT_CELLAR_REACT_PROMPT = `
You are Cellar's LangGraph agent.

Drive the computer through the available tools.

Operating rules:
- Start with see() unless the transcript already contains a fresh observation you trust.
- After every act(), call see() again before deciding what to do next.
- Use small, safe steps.
- Prefer browser-native actions like navigate and cdp_eval when available.
- Do not claim success unless the latest observation supports it.
- If the task is blocked, explain the blocker clearly in the final answer.
`.trim();

type LlmSurface = Pick<Cel, "llmCompleteWithRole">;

export interface CreateCellarReactAgentOptions
  extends Pick<CreateCortexToolsOptions, "captureScreenshotOnSee" | "maxContextChars" | "maxActions" | "goal">,
    CelToolCallingChatModelOptions {
  driver: CellarLangGraphDriver;
  llm: LlmSurface;
  checkpointer?: unknown;
  prompt?: string;
}

export function createCellarReactAgent(options: CreateCellarReactAgentOptions) {
  const { see, act, doneCheck, session } = createCortexTools({
    driver: options.driver,
    captureScreenshotOnSee: options.captureScreenshotOnSee,
    maxContextChars: options.maxContextChars,
    maxActions: options.maxActions,
    goal: options.goal,
  });

  const llm = new CelToolCallingChatModel(options.llm, {
    maxTokens: options.maxTokens,
    maxTranscriptChars: options.maxTranscriptChars,
    role: options.role,
  });

  return {
    agent: createReactAgent({
      llm,
      tools: [see, act, doneCheck],
      prompt: options.prompt ?? DEFAULT_CELLAR_REACT_PROMPT,
      checkpointer: (options.checkpointer ?? new MemorySaver()) as any,
      name: "cellar",
    }),
    session,
  };
}

export function extractFinalAgentText(messages: BaseMessage[]): string {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index] as {
      content?: unknown;
      tool_calls?: unknown[];
      _getType(): string;
    };

    if (message._getType() !== "ai") {
      continue;
    }
    if (Array.isArray(message.tool_calls) && message.tool_calls.length > 0) {
      continue;
    }

    return stringifyContent(message.content).trim();
  }
  return "";
}

export function serializeAgentMessages(messages: BaseMessage[]) {
  return messages.map((message) => {
    const base = message as {
      content?: unknown;
      name?: string;
      tool_calls?: unknown[];
      _getType(): string;
    };
    return {
      type: message._getType(),
      name: base.name ?? null,
      content: stringifyContent(base.content),
      tool_calls: Array.isArray(base.tool_calls) ? base.tool_calls : [],
    };
  });
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
      return JSON.stringify(item);
    }).join(" ");
  }
  if (content == null) {
    return "";
  }
  try {
    return JSON.stringify(content);
  } catch {
    return String(content);
  }
}
