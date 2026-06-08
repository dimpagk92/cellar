import { z } from "zod";
import type { Cel } from "@cellar/agent/runtime";
import { errorResult, textResult } from "./shared.js";

// ─── Schema ─────────────────────────────────────────────────────────────────

/**
 * The set of chunk kinds the external MCP surface accepts. Mirrors
 * `cel_memory::ChunkKind`; we exclude `rollup` (only the summarizer
 * produces those) but otherwise the surface matches.
 */
const KIND = z
  .enum([
    "chat",
    "action",
    "fire",
    "observation",
    "correction",
    "job_summary",
    "context",
  ])
  .default("chat");

export const celRememberSchema = z.object({
  content: z
    .string()
    .min(1)
    .describe(
      "The text to remember. Indexed for full-text search and embedded for vector similarity. " +
        "Write it the way you'd want to find it later — concise, self-contained, no pronouns " +
        "that reference vanished context.",
    ),
  kind: KIND.describe(
    "Chunk category. Defaults to 'chat' (durable assistant context). Use 'correction' for " +
      "user feedback the agent must remember; 'observation' for noticed facts about the user's " +
      "workflow; 'context' for file/app/url focus episodes.",
  ),
  tags: z
    .array(z.string())
    .optional()
    .describe(
      "Optional tag list, stored in chunk metadata. Useful for predicate-based forget later " +
        "('forget everything tagged \"draft-notes\"'). Free-form short strings.",
    ),
  shareable: z
    .boolean()
    .default(false)
    .describe(
      "When true, this chunk surfaces to other MCP clients that query with scope=own_plus_shared. " +
        "Use for cross-tool preferences ('user prefers MM-DD-YYYY date format'). Off by default — " +
        "chunks are caller-private unless you explicitly share them.",
    ),
  importance: z
    .number()
    .min(0)
    .max(1)
    .optional()
    .describe(
      "Caller-supplied importance hint in [0.0, 1.0]. The provider's heuristic scorer assigns " +
        "a sensible default per kind (corrections > job summaries > observations > chat) when " +
        "omitted; override only when you know better than the heuristic.",
    ),
  session_id: z
    .string()
    .optional()
    .describe(
      "Optional session this chunk belongs to. Group related chunks (one conversation, one " +
        "delegated job) so the Memory tab can show them together and summarization can roll " +
        "them up.",
    ),
  project_root: z
    .string()
    .optional()
    .describe(
      "Optional project / workspace path. Lets the Memory tab filter by project. Use an " +
        "absolute path; the prefix is matched on recall.",
    ),
  pinned: z
    .boolean()
    .default(false)
    .describe(
      "Pin from creation. Pinned chunks are never auto-evicted regardless of age or importance. " +
        "Use sparingly — pinning everything defeats the eviction policy.",
    ),
  caller_id: z
    .string()
    .optional()
    .describe(
      "Override the caller_id stamped on the chunk. Normally inferred from the MCP host name " +
        "(client id) and prefixed with 'mcp:'. Override only for diagnostic / test paths.",
    ),
});

type Input = z.infer<typeof celRememberSchema>;

/**
 * Resolve the caller identity for an MCP client. The CEL MCP server doesn't
 * yet carry per-request client identity (the SDK gives us only the transport
 * peer), so we fall back to an env-derived label that production deployments
 * can pin via `CELLAR_MCP_CALLER_ID=mcp:<your-client-name>`. Defaults to
 * `mcp:unknown` to make the origin visible in the Memory tab.
 */
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

export async function handleCelRemember(cel: Cel, args: Input) {
  try {
    const callerId = resolveCallerId(args.caller_id);
    const metadata: Record<string, unknown> = {};
    if (args.tags && args.tags.length > 0) {
      metadata.tags = args.tags;
    }
    const chunk = cel.memoryRemember({
      kind: args.kind,
      source: "mcp",
      caller_id: callerId,
      content: args.content,
      session_id: args.session_id ?? null,
      project_root: args.project_root ?? null,
      metadata,
      importance: args.importance ?? null,
      shareable: args.shareable,
      pinned: args.pinned,
    });
    return textResult({
      ok: true,
      chunk,
      caller_id: callerId,
    });
  } catch (err) {
    return errorResult(err instanceof Error ? err.message : String(err));
  }
}
