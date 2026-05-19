import { Command } from "commander";
import {
  Cel,
  getCanonicalCdpState,
  normalizeCortexModel,
} from "@cellar/agent/runtime";

export const statusCommand = new Command("status")
  .description("Show CEL runtime status")
  .action(async () => {
    const cel = new Cel();
    const nativeStatus = cel.isNativeAvailable ? "loaded" : "not available";

    console.log("CEL Runtime Status");
    console.log("==================");
    console.log(`  Version:    cellar ${cel.version()}`);
    console.log(`  Native:     ${nativeStatus}`);
    console.log(`  Platform:   ${process.platform}`);
    console.log(`  Node.js:    ${process.version}`);
    console.log(`  Cortex:     ${cel.isCortexRunning() ? "running" : "stopped"}`);

    if (cel.isNativeAvailable) {
      if (cel.isCortexRunning()) {
        try {
          const model = normalizeCortexModel(cel.readCortexModel());
          if (model) {
            console.log(`  Confidence: ${(model.confidence * 100).toFixed(0)}%`);
            console.log(`  Freshness:  ${model.freshness?.state ?? "unknown"} (${model.ageMs}ms old)`);
            console.log(`  Context:    ${model.currentContext.app || "(unknown)"} / ${model.currentContext.window || "(unknown)"}`);
            if (model.semantic) {
              console.log(`  Activity:   ${model.semantic.currentActivity}`);
              console.log(`  Phase:      ${model.semantic.taskPhase}`);
              if (model.semantic.likelyBlocker) {
                console.log(`  Blocker:    ${model.semantic.likelyBlocker}`);
              }
              if (model.semantic.suggestedNextStep) {
                console.log(`  Next step:  ${model.semantic.suggestedNextStep}`);
              }
            }
            if (model.streamStatus) {
              console.log(
                `  Streams:    a11y=${model.streamStatus.accessibility ? "on" : "off"} ` +
                  `display=${model.streamStatus.display ? "on" : "off"} ` +
                  `signals=${model.streamStatus.signals ? "on" : "off"} ` +
                  `network=${model.streamStatus.network ? "on" : "off"} ` +
                  `vision=${model.streamStatus.vision ? "on" : "off"} ` +
                  `audio=${model.streamStatus.audioCapture ? "on" : "off"}`,
              );
            }
            console.log(
              `  Context+:   windows=${model.currentContext.window_list?.length ?? 0} ` +
                `apps=${model.currentContext.running_apps?.length ?? 0} ` +
                `tcp=${model.currentContext.network_events?.length ?? 0} ` +
                `http=${model.currentContext.http_events?.length ?? 0} ` +
                `recent_files=${model.currentContext.recent_files?.length ?? 0} ` +
                `transcripts=${model.currentContext.transcripts?.length ?? 0}`,
            );
            if (model.sourceSummary) {
              console.log(
                `  Sources:    a11y=${model.sourceSummary.accessibility} ` +
                  `native=${model.sourceSummary.nativeApi} ` +
                  `vision=${model.sourceSummary.vision} ` +
                  `merged=${model.sourceSummary.merged} ` +
                  `adapter=${model.sourceSummary.adapterBacked}`,
              );
            }
            console.log(
              `  Adapters:   ${model.activeAdapters?.length ? model.activeAdapters.join(", ") : "(none)"}`,
            );
          }
        } catch {
          console.log("  Cortex:     (read failed)");
        }
      }

      try {
        const monitors = cel.listMonitors();
        console.log(`  Monitors:   ${monitors.length}`);
        for (const m of monitors) {
          const primary = m.is_primary ? " (primary)" : "";
          console.log(`    - ${m.name}: ${m.width}x${m.height}${primary}`);
        }
      } catch {
        console.log("  Monitors:   (query failed)");
      }

      try {
        const windows = cel.listWindows();
        console.log(`  Windows:    ${windows.length} visible`);
      } catch {
        console.log("  Windows:    (query failed)");
      }

      try {
        const browserState = await getCanonicalCdpState(cel);
        const browserStatus = browserState.status;
        const preferred = browserState.preferredTarget;
        console.log(
          `  CDP:        ${browserStatus.ready ? "ready" : browserStatus.running ? "running" : "stopped"} ` +
            `(port ${browserStatus.port}, targets ${browserStatus.targetCount})`,
        );
        if (preferred) {
          console.log(`  CDP Pick:   ${preferred.app_name} @ ${preferred.port}`);
        }
        if (browserState.mismatch) {
          console.log(
            `  CDP Merge:  raw/native saw ${browserState.rawTargetCount}; canonical view sees ${browserState.targets.length}`,
          );
        }
      } catch {
        console.log("  CDP:        (query failed)");
      }
    }
  });
