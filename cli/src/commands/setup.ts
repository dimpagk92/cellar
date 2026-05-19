import { Command } from "commander";
import { Cel } from "@cellar/agent/runtime";

export const setupCommand = new Command("setup")
  .description("Configure CEL for maximum context depth on this machine")
  .action(async () => {
    const cel = new Cel();

    console.log("CEL Setup");
    console.log("=========\n");

    // 1. Check Accessibility permission
    console.log("1. Accessibility permission");
    if (cel.isNativeAvailable) {
      const ctx = cel.getContext();
      if (ctx.elements.length > 1) {
        console.log("   OK — Accessibility enabled, reading screen context");
      } else {
        console.log("   WARNING — Native module loaded but no elements detected");
        console.log("   → Open System Settings > Privacy & Security > Accessibility");
        console.log("   → Add the app running CEL (e.g., Claude.app, Terminal.app)");
      }
    } else {
      console.log("   SKIP — Native module not built (run: cargo build -p cel-napi)");
    }

    // 2. Install CDP LaunchAgent for Electron apps
    console.log("\n2. CDP for Electron apps (Claude, VS Code, Slack, etc.)");
    if (cel.isNativeAvailable) {
      try {
        const result = (cel as any).native?.cdpSetupInstall?.();
        if (result === "installed") {
          console.log("   INSTALLED — LaunchAgent created");
          console.log("   → Electron apps will have CDP enabled on next launch");
          console.log("   → Restart any currently running Electron apps for full page content");
        } else if (result === "already_installed") {
          console.log("   OK — Already installed");
        } else {
          console.log("   SKIP — CDP setup not available (native module may need rebuild)");
        }
      } catch (e) {
        console.log(`   ERROR — ${e}`);
      }
    } else {
      console.log("   SKIP — Requires native module");
    }

    // 3. Summary
    console.log("\n3. CDP target discovery");
    if (cel.isNativeAvailable) {
      try {
        const targetsJson = (cel as any).native?.cdpDiscoverTargets?.();
        if (targetsJson) {
          const targets = JSON.parse(targetsJson);
          if (targets.length > 0) {
            console.log(`   Found ${targets.length} CDP target(s):`);
            for (const t of targets) {
              console.log(`   → ${t.app_name} (port ${t.port})`);
            }
          } else {
            console.log("   No CDP targets found yet");
            console.log("   → Targets appear after restarting Electron apps");
          }
        }
      } catch {
        console.log("   SKIP — Discovery not available");
      }
    }

    console.log("\nSetup complete.");
    console.log("Next steps:");
    console.log("  1. Run 'cellar browser ensure' to launch CEL's dedicated browser instance.");
    console.log("  2. Run 'cellar browser status' to confirm CDP targets are visible.");
    console.log("  3. Run 'cellar context' to see what CEL detects.");
  })
  .addCommand(
    new Command("uninstall")
      .description("Remove CEL CDP configuration")
      .action(() => {
        const cel = new Cel();
        if (cel.isNativeAvailable) {
          try {
            const result = (cel as any).native?.cdpSetupUninstall?.();
            if (result === "uninstalled") {
              console.log("CDP LaunchAgent removed.");
            } else {
              console.log("CDP LaunchAgent was not installed.");
            }
          } catch (e) {
            console.error(`Error: ${e}`);
          }
        }
      })
  );
