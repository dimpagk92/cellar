import { z } from "zod";
import type { Cel } from "@cellar/agent/runtime";
import { errorResult, textResult } from "./shared.js";

const KIND_FILTER = z.enum([
  "chat",
  "action",
  "fire",
  "observation",
  "correction",
  "job_summary",
  "context",
  "rollup",
]);

const SCOPE = z
  .enum(["own", "own_plus_shared", "global"])
  .default("own");

export const celRecallSchema = z.object({
  query: z
    .string()
    .min(1)
    .describe(
      "Free-text query. Embedded for vector search and tokenized for FTS. Phrase it like a " +
        "question or a fact you're trying to find — the hybrid retriever blends semantic " +
        "similarity, keyword match, and recency, so both natural phrasing and exact-keyword " +
        "queries work.",
    ),
  limit: z
    .number()
    .int()
    .min(1)
    .max(50)
    .default(8)
    .describe(
      "Top-k results. Default 8 fits comfortably in a model context window. Raise sparingly — " +
        "the retriever's recall@5 is tuned to >= 0.85, so the first 5 already contain the " +
        "expected chunk most of the time.",
    ),
  kind: z
    .array(KIND_FILTER)
    .optional()
    .describe(
      "Optional kind filter. Use to narrow ('only corrections') when the query alone doesn't " +
        "disambiguate. Omit to search across all kinds.",
    ),
  scope: SCOPE.describe(
    "Multi-agent visibility scope. 'own' (default) — only chunks this caller wrote. " +
      "'own_plus_shared' — own chunks plus any chunk another caller marked shareable=true " +
      "(use for cross-tool preferences). 'global' — every chunk regardless of caller (reserved " +
      "for the Memory tab and audit timeline; external MCP clients should rarely use it).",
  ),
  min_importance: z
    .number()
    .min(0)
    .max(1)
    .optional()
    .describe(
      "Lower bound on importance in [0.0, 1.0]. Use 0.7+ for 'only high-signal stuff' " +
        "(corrections + pinned + summaries dominate this band). Omit for no floor.",
    ),
  session_id: z
    .string()
    .optional()
    .describe(
      "Restrict to chunks belonging to this session. Pair with `query` to ask 'what did we " +
        "discuss about X in this conversation?'",
    ),
  caller_id: z
    .string()
    .optional()
    .describe(
      "Override the caller_id used for scoping. Normally inferred from the MCP host. " +
        "Override only for diagnostic / test paths — overriding to another caller's id is " +
        "a no-op (you still only see chunks that match the scope semantics).",
    ),
});

type Input = z.infer<typeof celRecallSchema>;

function resolveCallerId(override?: string): string {
  if (override && override.length > 0) {
    return override.startsWith("mcp:") || override.startsWith("embedded")
      ? override
      : `mcp:${override}`;
  }
  const env = process.env.CELLAR_MCP_CALLER_ID;
  if (env && env.length > 0) {
    return env.startsWith("mcp:") || env.startsWith("embedded") ? env : `mcp:${env}`;
  }
  return "mcp:unknown";
}

export async function handleCelRecall(cel: Cel, args: Input) {
  try {
    const callerId = resolveCallerId(args.caller_id);
    const hits = cel.memoryRecall({
      text: args.query,
      caller_id: callerId,
      caller_scope: args.scope,
      k: args.limit,
      kinds: args.kind ?? null,
      session_id: args.session_id ?? null,
      min_importance: args.min_importance ?? null,
    });
    return textResult({
      ok: true,
      caller_id: callerId,
      scope: args.scope,
      count: Array.isArray(hits) ? hits.length : 0,
      chunks: hits,
    });
  } catch (err) {
    return errorResult(err instanceof Error ? err.message : String(err));
  }
}
