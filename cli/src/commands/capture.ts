import { Command } from "commander";
import * as fs from "node:fs";
import * as path from "node:path";
import { Cel } from "@cellar/agent";

export const captureCommand = new Command("capture")
  .description("Capture a screenshot")
  .option("-o, --output <path>", "Output file path", "screenshot.png")
  .option("--monitor <id>", "Monitor ID to capture")
  .option("--window <id>", "Window ID to capture")
  .option("--list-monitors", "List available monitors")
  .option("--list-windows", "List visible windows")
  .action((opts) => {
    const cel = new Cel();

    if (!cel.isNativeAvailable) {
      console.error("Error: CEL native module not available.");
      console.error("Build with: cd cel/cel-napi && napi build --release");
      process.exit(1);
    }

    if (opts.listMonitors) {
      const monitors = cel.listMonitors();
      console.log("Available monitors:");
      for (const m of monitors) {
        const primary = m.is_primary ? " (primary)" : "";
        console.log(`  [${m.id}] ${m.name} — ${m.width}x${m.height}${primary}`);
      }
      return;
    }

    if (opts.listWindows) {
      const windows = cel.listWindows();
      console.log("Visible windows:");
      for (const w of windows) {
        console.log(`  [${w.id}] ${w.app_name}: ${w.title} — ${w.width}x${w.height}`);
      }
      return;
    }

    try {
      const png = cel.captureScreen();
      const outputPath = path.resolve(opts.output);
      fs.writeFileSync(outputPath, png);
      console.log(`Screenshot saved to ${outputPath} (${png.length} bytes)`);
    } catch (err) {
      console.error(`Capture failed: ${err}`);
      process.exit(1);
    }
  });
