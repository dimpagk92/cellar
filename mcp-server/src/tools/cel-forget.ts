import { z } from "zod";
import type { Cel } from "@cellar/agent/runtime";
import { errorResult, textResult } from "./shared.js";

const KIND = z.enum([
  "chat",
  "action",
  "fire",
  "observation",
  "correction",
  "job_summary",
  "context",
  "rollup",
]);

const PREDICATE = z.object({
  kind: z
    .array(KIND)
    .optional()
    .describe(
      "Delete chunks of these kinds (e.g., ['chat', 'observation']). Combine with other " +
        "predicate fields to AND the criteria.",
    ),
  older_than: z
    .string()
    .datetime()
    .optional()
    .describe(
      "ISO-8601 timestamp. Delete chunks created strictly before this instant. Useful for " +
        "rolling retention windows ('forget everything older than 30 days').",
    ),
  tag: z
    .string()
    .optional()
    .describe(
      "Delete chunks whose content contains this substring (v1 storage doesn't yet have a " +
        "dedicated tag index; the predicate falls back to content match). Phase 5 will add " +
        "metadata tag indexing.",
    ),
});

// One of chunk_ids / predicate, exactly. The schema is a base object plus a
// refine so JSON-schema generation still produces a usable shape (zod's
// discriminatedUnion would require a discriminator field the caller has to
// supply explicitly, which the natural surface here doesn't have).
export const celForgetSchema = z
  .object({
    chunk_ids: z
      .array(z.string().min(1))
      .optional()
      .describe(
        "Exact chunk IDs to delete. Only chunks owned by the calling MCP client are deleted; " +
          "ids owned by other callers are silently skipped (cross-caller deletion requires " +
          "the Memory tab UI or a permissive rule).",
      ),
    predicate: PREDICATE.optional().describe(
      "Predicate-based delete. ANDed criteria. Always scoped to the calling MCP client — " +
        "this surface can never mass-delete another caller's history.",
    ),
    caller_id: z
      .string()
      .optional()
      .describe(
        "Override the caller_id used for ownership check + predicate scoping. Normally " +
          "inferred from the MCP host. Override only for diagnostic / test paths.",
      ),
  })
  .refine(
    (v) => {
      const byIds = (v.chunk_ids?.length ?? 0) > 0;
      const byPredicate =
        v.predicate !== undefined &&
        (v.predicate.kind !== undefined ||
          v.predicate.older_than !== undefined ||
          v.predicate.tag !== undefined);
      return byIds !== byPredicate; // XOR
    },
    {
      message: "Provide exactly one of `chunk_ids` or `predicate` (not both, not neither)",
    },
  );

type Input = z.infer<typeof celForgetSchema>;

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

export async function handleCelForget(cel: Cel, args: Input) {
  try {
    const callerId = resolveCallerId(args.caller_id);
    if (args.chunk_ids && args.chunk_ids.length > 0) {
      const deleted = cel.memoryForgetIds(callerId, args.chunk_ids);
      return textResult({
        ok: true,
        mode: "ids",
        caller_id: callerId,
        deleted,
        requested: args.chunk_ids.length,
      });
    }
    // Predicate path — scope to caller.
    const p = args.predicate ?? {};
    const predicate: Record<string, unknown> = {
      callers: [callerId],
    };
    if (p.kind && p.kind.length > 0) predicate.kinds = p.kind;
    if (p.older_than) predicate.before = p.older_than;
    if (p.tag) predicate.content_contains = p.tag;
    const deleted = cel.memoryForgetMatching(predicate);
    return textResult({
      ok: true,
      mode: "predicate",
      caller_id: callerId,
      deleted,
    });
  } catch (err) {
    return errorResult(err instanceof Error ? err.message : String(err));
  }
}
