import { describe, expect, it, vi } from "vitest";
import { AIMessage, ToolMessage } from "@langchain/core/messages";
import { tool } from "@langchain/core/tools";
import { z } from "zod";

import { CelToolCallingChatModel } from "./tool-calling-model.js";

describe("CelToolCallingChatModel", () => {
  it("converts a JSON tool decision into an AI message with tool calls", async () => {
    const model = new CelToolCallingChatModel({
      llmCompleteWithRole: vi.fn(async () => JSON.stringify({
        kind: "tool",
        name: "see",
        args: {
          hint: "inspect",
        },
        thought: "Need context first",
      })),
    });

    const see = tool(async () => "{}", {
      name: "see",
      description: "Inspect the current UI state.",
      schema: z.object({
        hint: z.string().optional(),
      }),
    });

    const message = await model.bindTools([see]).invoke([
      {
        role: "user",
        content: "Check the page",
      },
    ]);

    expect(message).toBeInstanceOf(AIMessage);
    const aiMessage = message as AIMessage;
    expect(aiMessage.tool_calls).toHaveLength(1);
    expect(aiMessage.tool_calls?.[0]).toMatchObject({
      name: "see",
      args: {
        hint: "inspect",
      },
    });
  });

  it("recovers the first JSON object when the model appends extra text", async () => {
    const model = new CelToolCallingChatModel({
      llmCompleteWithRole: vi.fn(async () => [
        "{\"kind\":\"final\",\"content\":\"Example Domain\"}",
        "Note: extra debug output {not-json-for-us}",
      ].join("\n")),
    });

    const message = await model.invoke([
      {
        role: "user",
        content: "Tell me the title",
      },
      new ToolMessage({
        name: "see",
        tool_call_id: "call-0",
        content: "{\"window\":\"Example Domain\"}",
      }),
    ]);

    expect(message).toBeInstanceOf(AIMessage);
    expect((message as AIMessage).content).toBe("Example Domain");
  });

  it("forces see before a final answer when no tool result exists yet", async () => {
    const model = new CelToolCallingChatModel({
      llmCompleteWithRole: vi.fn(async () => JSON.stringify({
        kind: "final",
        content: "BTC is up today.",
      })),
    });

    const message = await model.invoke([
      {
        role: "user",
        content: "Get crypto prices",
      },
    ]);

    expect(message).toBeInstanceOf(AIMessage);
    const aiMessage = message as AIMessage;
    expect(aiMessage.tool_calls).toHaveLength(1);
    expect(aiMessage.tool_calls?.[0]).toMatchObject({
      name: "see",
    });
  });

  it("forces see after an act tool result before another act or final answer", async () => {
    const model = new CelToolCallingChatModel({
      llmCompleteWithRole: vi.fn(async () => JSON.stringify({
        kind: "final",
        content: "Done.",
      })),
    });

    const message = await model.invoke([
      {
        role: "user",
        content: "Do the task",
      },
      new ToolMessage({
        name: "act",
        tool_call_id: "call-1",
        content: "{\"status\":\"ok\"}",
      }),
    ]);

    expect(message).toBeInstanceOf(AIMessage);
    const aiMessage = message as AIMessage;
    expect(aiMessage.tool_calls).toHaveLength(1);
    expect(aiMessage.tool_calls?.[0]).toMatchObject({
      name: "see",
    });
  });

  it("forces done_check before a final answer when the tool is available", async () => {
    const model = new CelToolCallingChatModel({
      llmCompleteWithRole: vi.fn(async () => JSON.stringify({
        kind: "final",
        content: "BTC is 93,100. ETH is 1,780. SOL is 151. Headline: Crypto rallies.",
      })),
    });

    const doneCheck = tool(async () => "{}", {
      name: "done_check",
      description: "Validate the draft answer against the goal contract.",
      schema: z.object({
        draft_answer: z.string(),
      }),
    });

    const message = await model.bindTools([doneCheck]).invoke([
      {
        role: "user",
        content: "Get BTC, ETH, and SOL prices plus one headline.",
      },
      new ToolMessage({
        name: "see",
        tool_call_id: "call-0",
        content: "{\"context\":\"fresh observation\"}",
      }),
    ]);

    expect(message).toBeInstanceOf(AIMessage);
    const aiMessage = message as AIMessage;
    expect(aiMessage.tool_calls).toHaveLength(1);
    expect(aiMessage.tool_calls?.[0]).toMatchObject({
      name: "done_check",
      args: {
        draft_answer: "BTC is 93,100. ETH is 1,780. SOL is 151. Headline: Crypto rallies.",
      },
    });
  });

  it("allows a final answer after done_check verified true", async () => {
    const model = new CelToolCallingChatModel({
      llmCompleteWithRole: vi.fn(async () => JSON.stringify({
        kind: "final",
        content: "BTC is 93,100. ETH is 1,780. SOL is 151. Headline: Crypto rallies.",
      })),
    });

    const doneCheck = tool(async () => "{}", {
      name: "done_check",
      description: "Validate the draft answer against the goal contract.",
      schema: z.object({
        draft_answer: z.string(),
      }),
    });

    const message = await model.bindTools([doneCheck]).invoke([
      {
        role: "user",
        content: "Get BTC, ETH, and SOL prices plus one headline.",
      },
      new ToolMessage({
        name: "done_check",
        tool_call_id: "call-1",
        content: "{\"verified\":true,\"missing\":[]}",
      }),
    ]);

    expect(message).toBeInstanceOf(AIMessage);
    expect((message as AIMessage).content).toBe(
      "BTC is 93,100. ETH is 1,780. SOL is 151. Headline: Crypto rallies.",
    );
  });

  it("accepts tool responses that use the tool name as kind", async () => {
    const model = new CelToolCallingChatModel({
      llmCompleteWithRole: vi.fn(async () => JSON.stringify({
        kind: "done_check",
        args: {
          draft_answer: "Ruby Martinez — ruby.martinez@company.com",
        },
        thought: "Validate before answering",
      })),
    });

    const doneCheck = tool(async () => "{}", {
      name: "done_check",
      description: "Validate the draft answer against the goal contract.",
      schema: z.object({
        draft_answer: z.string(),
      }),
    });

    const message = await model.bindTools([doneCheck]).invoke([
      {
        role: "user",
        content: "Find employee EMP-0742 and return the name and email.",
      },
      new ToolMessage({
        name: "see",
        tool_call_id: "call-0",
        content: "{\"context\":\"fresh observation\"}",
      }),
    ]);

    expect(message).toBeInstanceOf(AIMessage);
    const aiMessage = message as AIMessage;
    expect(aiMessage.tool_calls).toHaveLength(1);
    expect(aiMessage.tool_calls?.[0]).toMatchObject({
      name: "done_check",
      args: {
        draft_answer: "Ruby Martinez — ruby.martinez@company.com",
      },
    });
  });
});
