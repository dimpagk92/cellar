import { z } from "zod";
import {
  type Cel,
  type ScreenContext,
  type CelEvent,
  discoverCanonicalCdpTargets,
  getCanonicalCdpState,
  normalizeCortexModel,
} from "@cellar/agent";
import {
  sleep,
  buildUrlMap,
  compactElement,
  sanitizeElement,
  contextFingerprint,
  elementMatches,
  textResult,
  errorResult,
  axPermissionGuard,
} from "./shared.js";
import { persistObservation, readObservation } from "./observations.js";
import { compressContext, hasActiveSpinner } from "@cellar/agent";

export const celSeeSchema = z.discriminatedUnion("mode", [
  // --- context: full screen context ---
  z.object({
    mode: z.literal("context"),
    filter: z
      .object({
        element_types: z
          .array(z.string())
          .optional()
          .describe("Only include elements of these types (e.g. button, input, link)"),
        min_confidence: z
          .number()
          .min(0)
          .max(1)
          .optional()
          .describe("Minimum confidence threshold (0.0-1.0)"),
        detail: z
          .enum(["full", "compact", "actionable_only", "summary"])
          .default("full")
          .describe(
            "full: all fields. compact: id+type+label+actions (~40% fewer tokens). " +
              "actionable_only: enabled+visible elements with actions. " +
              "summary: element counts by type only.",
          ),
      })
      .optional(),
    compression: z
      .object({
        enabled: z.boolean().default(false).describe("Enable context compression to reduce tokens"),
        strip_wrappers: z.boolean().default(true).describe("Remove structural-only container elements"),
        collapse_repetitive: z.boolean().default(true).describe("Collapse repeated sibling elements"),
        truncate_tables: z.number().default(5).describe("Max table rows before truncation (0=disabled)"),
        dedup_against: z.string().optional().describe("Previous snapshot_hash for cross-snapshot dedup"),
      })
      .optional()
      .describe("Compress the accessibility tree to reduce token usage (~40-60% reduction)"),
  }),

  // --- screenshot ---
  z.object({
    mode: z.literal("screenshot"),
  }),

  // --- observation: load a previously persisted observation snapshot ---
  z.object({
    mode: z.literal("observation"),
    observation_id: z.string().describe("Observation id returned by a previous context call"),
  }),

  // --- windows ---
  z.object({
    mode: z.literal("windows"),
  }),

  // --- monitors ---
  z.object({
    mode: z.literal("monitors"),
  }),

  // --- focused: zoom into one element ---
  z.object({
    mode: z.literal("focused"),
    element_id: z.string().describe("Element ID from a previous context snapshot"),
  }),

  // --- element_at: hit-test coordinates ---
  z.object({
    mode: z.literal("element_at"),
    x: z.number().describe("Screen X coordinate"),
    y: z.number().describe("Screen Y coordinate"),
  }),

  // --- is_settable: check if element supports direct value set ---
  z.object({
    mode: z.literal("is_settable"),
    element_id: z.string().describe("Element ID to check"),
  }),

  // --- make_reference: create resilient element reference ---
  z.object({
    mode: z.literal("make_reference"),
    element_id: z.string().describe("Element ID from a previous context snapshot"),
  }),

  // --- cursor_position ---
  z.object({
    mode: z.literal("cursor_position"),
  }),

  // --- cdp_status ---
  z.object({
    mode: z.literal("cdp_status"),
  }),

  // --- cdp_page: get browser page content ---
  z.object({
    mode: z.literal("cdp_page"),
  }),

  // --- wait_for_element ---
  z.object({
    mode: z.literal("wait_for_element"),
    element_type: z.string().optional().describe("Required element type (e.g. button, input)"),
    label_contains: z.string().optional().describe("Element label must contain this text"),
    timeout_ms: z.number().default(10000).describe("Max wait time in milliseconds"),
    poll_interval_ms: z.number().default(500).describe("Poll interval in milliseconds"),
  }),

  // --- wait_for_idle ---
  z.object({
    mode: z.literal("wait_for_idle"),
    timeout_ms: z.number().default(10000).describe("Max wait time in milliseconds"),
    poll_interval_ms: z.number().default(500).describe("Poll interval in milliseconds"),
  }),

  // --- watch: event-driven waiting ---
  z.object({
    mode: z.literal("watch"),
    events: z
      .array(
        z.enum([
          "tree_changed",
          "network_idle",
          "focus_changed",
          "value_changed",
          "window_created",
          "menu_opened",
          "menu_closed",
          "sheet_created",
          "layout_changed",
          "title_changed",
          "app_activated",
          "app_deactivated",
          "window_moved",
          "window_resized",
          "window_minimized",
          "window_restored",
          "selection_changed",
          "row_count_changed",
        ]),
      )
      .describe("Event types to watch for"),
    timeout_ms: z.number().default(30000).describe("Max wait time in milliseconds"),
    poll_interval_ms: z.number().default(200).describe("Poll interval in milliseconds"),
  }),
]);

type Input = z.infer<typeof celSeeSchema>;

/** Map snake_case event names to Rust PascalCase enum variants. */
function toRustEventType(e: string): string {
  return e
    .split("_")
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join("");
}

export async function handleCelSee(cel: Cel, args: Input) {
  const denied = axPermissionGuard(cel);
  if (denied) return denied;
  try {
    switch (args.mode) {
      case "context": {
        // If Rust Cortex is running, use its always-fresh mental model (instant, shared memory)
        // Otherwise fall back to on-demand cel.getContext()
        let ctx: ScreenContext;
        let cortexConfidence: number | undefined;
        let cortexModel: ReturnType<typeof normalizeCortexModel> | null = null;

        if (cel.isCortexRunning()) {
          cortexModel = normalizeCortexModel(cel.readCortexModel());
          ctx = cortexModel?.currentContext ?? cel.getContext();
          cortexConfidence = cortexModel?.confidence;
        } else {
          ctx = cel.getContext();
        }

        // CDP enrichment: when a browser is focused and CDP is available,
        // merge page text content into the context. The accessibility tree
        // provides structure but misses page body text that CDP can read.
        let cdpEnriched = false;
        try {
          const cdpTargets = await discoverCanonicalCdpTargets(cel);
          if (cdpTargets.length > 0) {
            const pageContent = await cel.getCdpPageContent();
            if (pageContent?.body_text && pageContent.body_text.length > 10) {
              // Check if page-text already exists (avoid duplicates)
              const hasPageText = ctx.elements.some(
                (el: import("@cellar/agent").ContextElement) =>
                  el.id === "page-text" || el.id?.includes("page-text"),
              );
              if (!hasPageText) {
                // Add page body text as a content element
                ctx = { ...ctx, elements: [
                  ...ctx.elements,
                  {
                    id: "cdp-page-text",
                    element_type: "text",
                    label: "Page content",
                    value: pageContent.body_text.slice(0, 2000),
                    state: { focused: false, enabled: true, visible: true, selected: false },
                    actions: [],
                    confidence: 0.9,
                    source: "merged" as const,
                    content_role: "content" as const,
                  } as import("@cellar/agent").ContextElement,
                ]};
                cdpEnriched = true;
              }
            }
          }
        } catch { /* CDP not available — proceed with a11y-only context */ }

        const detail = args.filter?.detail ?? "full";

        // Summary mode
        if (detail === "summary") {
          const typeCounts: Record<string, number> = {};
          let actionableCount = 0;
          for (const el of ctx.elements) {
            typeCounts[el.element_type] = (typeCounts[el.element_type] || 0) + 1;
            if (el.state?.enabled && el.state?.visible && (el.actions?.length ?? 0) > 0) {
              actionableCount++;
            }
          }
          const observationId = persistObservation(ctx);
          return textResult({
            observation_id: observationId,
            app: ctx.app,
            window: ctx.window,
            element_count: ctx.elements.length,
            actionable_count: actionableCount,
            element_types: typeCounts,
            timestamp_ms: ctx.timestamp_ms,
          });
        }

        // Filter elements
        let elements = ctx.elements;
        if (args.filter) {
          elements = elements.filter((el) => {
            if (
              args.filter!.element_types &&
              !args.filter!.element_types.includes(el.element_type)
            ) {
              return false;
            }
            if (
              args.filter!.min_confidence !== undefined &&
              el.confidence < args.filter!.min_confidence
            ) {
              return false;
            }
            return true;
          });
        }

        // Apply compression if requested (before any format-specific rendering)
        let snapshotHash: string | undefined;
        if (args.compression?.enabled) {
          const compressed = compressContext(
            { ...ctx, elements },
            {
              stripWrappers: args.compression.strip_wrappers ?? true,
              collapseRepetitive: args.compression.collapse_repetitive ?? true,
              truncateTableRows: args.compression.truncate_tables ?? 5,
              dedupAgainst: args.compression.dedup_against,
            },
          );
          elements = compressed.context.elements;
          snapshotHash = compressed.snapshotHash;
        }

        // Actionable-only filter
        if (detail === "actionable_only") {
          elements = elements.filter(
            (el) =>
              el.state?.enabled && el.state?.visible && (el.actions?.length ?? 0) > 0,
          );
        }

        // Compact format
        if (detail === "compact" || detail === "actionable_only") {
          const observationId = persistObservation({ ...ctx, elements });
          return textResult({
            observation_id: observationId,
            app: ctx.app,
            window: ctx.window,
            elements: elements.map(compactElement),
            timestamp_ms: ctx.timestamp_ms,
            ...(snapshotHash ? { snapshot_hash: snapshotHash } : {}),
          });
        }

        // Full detail
        const result: ScreenContext & {
          observation_id?: string;
          page_content?: unknown;
          url_map?: Record<number, string>;
          cdp_available?: boolean;
          cdp_target_count?: number;
          cortex_confidence?: number;
          cortex_freshness?: string;
          cortex_semantic?: unknown;
          cortex_source_summary?: unknown;
          snapshot_hash?: string;
        } = {
          ...ctx,
          elements: elements.map(sanitizeElement),
        };

        if (snapshotHash) {
          result.snapshot_hash = snapshotHash;
        }

        // Include cortex confidence if cortex is providing the context
        if (cortexConfidence !== undefined) {
          result.cortex_confidence = cortexConfidence;
        }
        if (cortexModel?.freshness?.state) {
          result.cortex_freshness = cortexModel.freshness.state;
        }
        if (cortexModel?.semantic) {
          result.cortex_semantic = cortexModel.semantic;
        }
        if (cortexModel?.sourceSummary) {
          result.cortex_source_summary = cortexModel.sourceSummary;
        }

        // Indicate CDP availability
        const cdpTargets = await discoverCanonicalCdpTargets(cel);
        result.cdp_available = cdpTargets.length > 0;
        result.cdp_target_count = cdpTargets.length;

        const urlMapObj = buildUrlMap(elements);
        if (Object.keys(urlMapObj).length > 0) {
          result.url_map = urlMapObj;
        }

        // Enrich with CDP page content if available
        try {
          const pageContent = await cel.getCdpPageContent();
          if (pageContent) {
            result.page_content = {
              title: pageContent.title,
              url: pageContent.url,
              body_text:
                pageContent.body_text.length > 3000
                  ? pageContent.body_text.slice(0, 3000) + "..."
                  : pageContent.body_text,
              text_blocks: pageContent.text_blocks.slice(0, 50),
              interactive_elements: pageContent.interactive_elements.slice(0, 50),
            };
          }
        } catch {
          // CDP not available
        }

        result.observation_id = persistObservation(result);

        return textResult(result);
      }

      case "screenshot": {
        const buffer = cel.captureScreen();
        const base64 = buffer.toString("base64");
        return {
          content: [
            {
              type: "image" as const,
              data: base64,
              mimeType: "image/png",
            },
          ],
        };
      }

      case "observation": {
        const observation = await readObservation(args.observation_id);
        if (!observation) {
          return errorResult(`Observation not found: ${args.observation_id}`);
        }
        return textResult(observation);
      }

      case "windows":
        return textResult(cel.listWindows());

      case "monitors":
        return textResult(cel.listMonitors());

      case "focused": {
        const focused = cel.getContextFocused(args.element_id);
        if (!focused) {
          return errorResult(
            `Element "${args.element_id}" not found in current context`,
          );
        }
        return textResult(focused);
      }

      case "element_at": {
        const element = cel.axElementAtPosition(args.x, args.y);
        if (!element) {
          return textResult({ found: false, x: args.x, y: args.y });
        }
        return textResult({ found: true, element });
      }

      case "is_settable": {
        const settable = cel.axIsSettable(args.element_id);
        return textResult({ element_id: args.element_id, settable });
      }

      case "make_reference": {
        const ctx = cel.getContext();
        const element = ctx.elements.find((el) => el.id === args.element_id);
        if (!element) {
          return errorResult(
            `Element "${args.element_id}" not found. Available IDs: ${ctx.elements
              .slice(0, 10)
              .map((e) => e.id)
              .join(", ")}`,
          );
        }
        const ref = cel.makeReference(element);
        return textResult(ref);
      }

      case "cursor_position": {
        const [x, y] = cel.mousePosition();
        return textResult({ x, y });
      }

      case "cdp_status": {
        const installed = cel.isCdpSetup();
        const state = await getCanonicalCdpState(cel);
        return textResult({
          installed,
          ready: state.status.ready,
          running: state.status.running,
          port: state.status.port,
          browser: state.status.browserVersion,
          targets: state.targets,
          raw_target_count: state.rawTargetCount,
          mismatch: state.mismatch,
          preferred_target: state.preferredTarget,
        });
      }

      case "cdp_page": {
        const state = await getCanonicalCdpState(cel);
        if (state.targets.length === 0) {
          return errorResult("No CDP target found. Is the dedicated CEL browser running?");
        }
        const pageContent = await cel.getCdpPageContent();
        if (!pageContent) {
          return errorResult("CDP target exists, but page extraction failed. Check cdp_status for discovery mismatches.");
        }
        return textResult(pageContent);
      }

      case "wait_for_element": {
        const deadline = Date.now() + args.timeout_ms;
        while (Date.now() < deadline) {
          const ctx = cel.getContext();
          const match = ctx.elements.find((el) =>
            elementMatches(el, args.element_type, args.label_contains),
          );
          if (match) {
            return textResult({
              found: true,
              element: match,
              context_summary: {
                app: ctx.app,
                window: ctx.window,
                total_elements: ctx.elements.length,
              },
            });
          }
          await sleep(args.poll_interval_ms);
        }
        return errorResult(
          `No matching element found within ${args.timeout_ms}ms (type: ${args.element_type ?? "any"}, label: ${args.label_contains ?? "any"})`,
        );
      }

      case "wait_for_idle": {
        const deadline = Date.now() + args.timeout_ms;
        let lastFp = "";
        let stableCount = 0;
        while (Date.now() < deadline) {
          const ctx = cel.getContext();

          // Wait through active spinners/loading indicators
          if (hasActiveSpinner(ctx)) {
            lastFp = "";
            stableCount = 0;
            await sleep(args.poll_interval_ms);
            continue;
          }

          const fp = contextFingerprint(ctx);
          if (fp === lastFp && lastFp !== "") {
            stableCount++;
            // Require 2 consecutive stable polls (not just 1) for reliability
            if (stableCount >= 2) {
              return textResult({ idle: true, context: ctx });
            }
          } else {
            stableCount = 0;
          }
          lastFp = fp;
          await sleep(args.poll_interval_ms);
        }
        const ctx = cel.getContext();
        return textResult({
          idle: false,
          reason: `Context still changing after ${args.timeout_ms}ms`,
          context: ctx,
        });
      }

      case "watch": {
        if (cel.isCortexRunning()) {
          return errorResult(
            "Cortex is active. Use cel_perceive read instead of cel_see watch.",
          );
        }
        cel.startWatchdog();
        const deadline = Date.now() + args.timeout_ms;
        const wantedTypes = new Set(args.events.map(toRustEventType));

        while (Date.now() < deadline) {
          const events = cel.pollEvents();
          const matching = events.filter((e: CelEvent) => wantedTypes.has(e.type));
          if (matching.length > 0) {
            const ctx = cel.getContext();
            cel.stopWatchdog();
            return textResult({ events: matching, context: ctx });
          }
          await sleep(args.poll_interval_ms);
        }
        cel.stopWatchdog();
        return errorResult(
          `No matching events within ${args.timeout_ms}ms`,
        );
      }
    }
  } catch (err) {
    return errorResult(err instanceof Error ? err.message : String(err));
  }
}
