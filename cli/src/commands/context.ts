import { Command } from "commander";
import { Cel, normalizeCortexModel, type MentalModel, type ScreenContext } from "@cellar/agent";

interface ContextOptions {
  json?: boolean;
  watch?: boolean;
  interval: string;
  direct?: boolean;
}

interface ContextSnapshot {
  source: "cortex" | "direct";
  captured_at_ms: number;
  context: ScreenContext;
  cortex?: {
    confidence: number;
    age_ms: number;
    cycle_count: number;
    uptime_ms: number;
    vision_needed: boolean;
    freshness: string;
    stable_elements: number;
    volatile_elements: number;
    focused_element: { id: string; label?: string } | null;
    pending_anomalies: unknown[];
    temporal: MentalModel["temporal"];
    last_diff: MentalModel["lastDiffSummary"] | null;
    stream_status: MentalModel["streamStatus"] | null;
    active_adapters: string[];
    semantic: MentalModel["semantic"] | null;
    source_summary: MentalModel["sourceSummary"] | null;
  };
}

function actionableCount(ctx: ScreenContext): number {
  return ctx.elements.filter(
    (el) => el.state?.enabled && el.state?.visible && (el.actions?.length ?? 0) > 0,
  ).length;
}

function buildSnapshot(cel: Cel, direct: boolean): ContextSnapshot {
  if (direct) {
    return {
      source: "direct",
      captured_at_ms: Date.now(),
      context: cel.getContext(),
    };
  }

  const model = normalizeCortexModel(cel.readCortexModel());
  if (!model) {
    throw new Error("Failed to read default Cortex mental model");
  }

  return {
    source: "cortex",
    captured_at_ms: Date.now(),
    context: model.currentContext,
    cortex: {
      confidence: model.confidence,
      age_ms: model.ageMs,
      cycle_count: model.cycleCount,
      uptime_ms: model.uptimeMs,
      vision_needed: model.visionNeeded,
      freshness: model.freshness?.state ?? "unknown",
      stable_elements: model.stability.stable.size,
      volatile_elements: model.stability.volatile.size,
      focused_element: model.focusedElement,
      pending_anomalies: model.anomalyQueue ?? [],
      temporal: model.temporal,
      last_diff: model.lastDiffSummary ?? null,
      stream_status: model.streamStatus ?? null,
      active_adapters: model.activeAdapters ?? [],
      semantic: model.semantic ?? null,
      source_summary: model.sourceSummary ?? null,
    },
  };
}

function printHuman(snapshot: ContextSnapshot): void {
  const ctx = snapshot.context;
  const streamStatus = snapshot.cortex?.stream_status;
  const streamSummary = streamStatus
    ? [
        `a11y=${streamStatus.accessibility ? "on" : "off"}`,
        `display=${streamStatus.display ? "on" : "off"}`,
        `signals=${streamStatus.signals ? "on" : "off"}`,
        `network=${streamStatus.network ? "on" : "off"}`,
        `vision=${streamStatus.vision ? "on" : "off"}`,
        `audio=${streamStatus.audioCapture ? "on" : "off"}`,
      ].join(" ")
    : null;

  console.log(`Source: ${snapshot.source}`);
  console.log(`Captured: ${new Date(snapshot.captured_at_ms).toISOString()}`);
  console.log(`App: ${ctx.app || "(unknown)"}`);
  console.log(`Window: ${ctx.window || "(unknown)"}`);
  console.log(`Elements: ${ctx.elements.length}`);
  console.log(`Actionable: ${actionableCount(ctx)}`);
  console.log(
    `Device: windows=${ctx.window_list?.length ?? 0} apps=${ctx.running_apps?.length ?? 0} recent_files=${ctx.recent_files?.length ?? 0}`,
  );
  console.log(
    `Network: tcp=${ctx.network_events?.length ?? 0} http=${ctx.http_events?.length ?? 0} transcripts=${ctx.transcripts?.length ?? 0}`,
  );

  if (snapshot.cortex) {
    console.log(
      `Cortex: confidence=${snapshot.cortex.confidence.toFixed(2)} age=${snapshot.cortex.age_ms}ms freshness=${snapshot.cortex.freshness} cycles=${snapshot.cortex.cycle_count} vision_needed=${snapshot.cortex.vision_needed}`,
    );
    if (streamSummary) {
      console.log(`Streams: ${streamSummary}`);
    }
    if (snapshot.cortex.semantic) {
      console.log(`Activity: ${snapshot.cortex.semantic.currentActivity}`);
      console.log(`Phase: ${snapshot.cortex.semantic.taskPhase}`);
      if (snapshot.cortex.semantic.recentTransition) {
        console.log(`Transition: ${snapshot.cortex.semantic.recentTransition}`);
      }
      if (snapshot.cortex.semantic.likelyBlocker) {
        console.log(`Blocker: ${snapshot.cortex.semantic.likelyBlocker}`);
      }
      if (snapshot.cortex.semantic.suggestedNextStep) {
        console.log(`Next: ${snapshot.cortex.semantic.suggestedNextStep}`);
      }
    }
    console.log(
      `Adapters: ${snapshot.cortex.active_adapters.length > 0 ? snapshot.cortex.active_adapters.join(", ") : "(none)"}`,
    );
    if (snapshot.cortex.source_summary) {
      console.log(
        `Sources: a11y=${snapshot.cortex.source_summary.accessibility} ` +
          `native=${snapshot.cortex.source_summary.nativeApi} ` +
          `vision=${snapshot.cortex.source_summary.vision} ` +
          `merged=${snapshot.cortex.source_summary.merged} ` +
          `adapter=${snapshot.cortex.source_summary.adapterBacked}`,
      );
    }
    if (snapshot.cortex.focused_element) {
      console.log(
        `Focused: ${snapshot.cortex.focused_element.label ?? "(no label)"} (${snapshot.cortex.focused_element.id})`,
      );
    }
    console.log(
      `Stability: stable=${snapshot.cortex.stable_elements} volatile=${snapshot.cortex.volatile_elements}`,
    );
    if (snapshot.cortex.pending_anomalies.length > 0) {
      const anomalyLines = snapshot.cortex.pending_anomalies
        .slice(0, 3)
        .map((anomaly: any) => anomaly.description ?? anomaly.type ?? JSON.stringify(anomaly));
      console.log(`Anomalies: ${anomalyLines.join(" | ")}`);
    }
    if (snapshot.cortex.last_diff) {
      console.log(
        `Last diff: +${snapshot.cortex.last_diff.addedCount} -${snapshot.cortex.last_diff.removedCount} ~${snapshot.cortex.last_diff.changedCount}`,
      );
    }
  }

  console.log("---");
  for (const el of ctx.elements.slice(0, 20)) {
    const bounds = el.bounds
      ? `(${el.bounds.x},${el.bounds.y} ${el.bounds.width}x${el.bounds.height})`
      : "";
    const conf = `[${(el.confidence * 100).toFixed(0)}%]`;
    console.log(
      `  ${conf} ${el.element_type}: ${el.label ?? "(no label)"} ${bounds}`,
    );
  }
  if (ctx.elements.length > 20) {
    console.log(`  ... and ${ctx.elements.length - 20} more`);
  }
}

export const contextCommand = new Command("context")
  .description("Get the unified screen context from the default Cortex (or use --direct for raw reads)")
  .option("--json", "Output raw JSON")
  .option("--watch", "Continuously poll and display context changes")
  .option("--interval <ms>", "Poll interval in milliseconds", "1000")
  .option("--direct", "Bypass Cortex and call getContext() directly")
  .action(async (opts: ContextOptions) => {
    const cel = new Cel();
    let bootedHere = false;

    if (!cel.isNativeAvailable) {
      console.error("Error: CEL native module not available.");
      process.exit(1);
    }

    if (!opts.direct && !cel.isCortexRunning()) {
      console.error("Booting default Cortex...");
      cel.bootCortex();
      bootedHere = true;
      await new Promise((resolve) => setTimeout(resolve, 700));
    }

    const printSnapshot = () => {
      const snapshot = buildSnapshot(cel, Boolean(opts.direct));
      if (opts.json) {
        if (opts.watch) {
          console.log(JSON.stringify(snapshot));
        } else {
          console.log(JSON.stringify(snapshot, null, 2));
        }
      } else {
        printHuman(snapshot);
      }
    };

    const cleanup = () => {
      if (bootedHere && cel.isCortexRunning()) {
        try {
          cel.stopCortex();
        } catch {
          // Best effort only.
        }
      }
    };

    if (opts.watch) {
      const interval = parseInt(opts.interval, 10);
      console.log(
        `Watching ${opts.direct ? "direct context" : "default Cortex"} every ${interval}ms. Ctrl+C to stop.\n`,
      );
      const timer = setInterval(() => {
        if (!opts.json && process.stdout.isTTY) {
          console.clear();
        }
        printSnapshot();
      }, interval);
      process.on("SIGINT", () => {
        clearInterval(timer);
        cleanup();
        process.exit(0);
      });
      printSnapshot();
    } else {
      try {
        printSnapshot();
      } finally {
        cleanup();
      }
    }
  });
