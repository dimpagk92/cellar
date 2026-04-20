import { Command } from "commander";
import {
  Cel,
  discoverCanonicalCdpTargets,
  ensureDedicatedCdpBrowser,
  getCanonicalCdpState,
  getPreferredCelCdpPort,
  selectPreferredCdpTarget,
} from "@cellar/agent";

type BrowserStatusOptions = {
  json?: boolean;
};

type BrowserEnsureOptions = {
  url?: string;
  json?: boolean;
};

function printJson(value: unknown): void {
  console.log(JSON.stringify(value, null, 2));
}

async function printTargets(cel: Cel): Promise<void> {
  const targets = await discoverCanonicalCdpTargets(cel);
  const preferredPort = getPreferredCelCdpPort();
  const frontmostApp = cel.getQuickContext().app;
  const preferred = selectPreferredCdpTarget(targets, frontmostApp, preferredPort);

  if (targets.length === 0) {
    console.log("No CDP targets discovered.");
    return;
  }

  console.log(`Discovered ${targets.length} CDP target(s):`);
  for (const target of targets) {
    const markers = [
      target.port === preferredPort ? "cel-port" : null,
      preferred?.ws_url === target.ws_url ? "selected" : null,
    ].filter(Boolean);
    const marker = markers.length > 0 ? ` [${markers.join(", ")}]` : "";
    console.log(`  - ${target.app_name} pid=${target.pid} port=${target.port}${marker}`);
  }
}

export const browserCommand = new Command("browser")
  .description("Manage CEL's dedicated CDP browser instance");

browserCommand
  .command("status")
  .description("Show the status of the dedicated CEL browser and all discovered CDP targets")
  .option("--json", "Output raw JSON")
  .action(async (opts: BrowserStatusOptions) => {
    const cel = new Cel();
    const canonical = await getCanonicalCdpState(cel);
    const status = canonical.status;
    const targets = canonical.targets;
    const preferred = canonical.preferredTarget;

    if (opts.json) {
      printJson({
        status,
        raw_target_count: canonical.rawTargetCount,
        mismatch: canonical.mismatch,
        preferred_target: preferred ?? null,
        targets,
      });
      return;
    }

    console.log("CEL Browser Status");
    console.log("==================");
    console.log(`  Port:         ${status.port}`);
    console.log(`  Running:      ${status.running ? "yes" : "no"}`);
    console.log(`  Ready:        ${status.ready ? "yes" : "no"}`);
    console.log(`  CEL-owned:    ${status.ownedByCel ? "yes" : "no"}`);
    console.log(`  Targets:      ${status.targetCount}`);
    if (status.browserVersion) {
      console.log(`  Browser:      ${status.browserVersion}`);
    }
    if (status.userDataDir) {
      console.log(`  User data:    ${status.userDataDir}`);
    } else {
      console.log(`  Profile root: ${status.profileRoot}`);
    }
    if (preferred) {
      console.log(`  Preferred:    ${preferred.app_name} on ${preferred.port}`);
    }
    if (canonical.mismatch) {
      console.log(`  Raw/native:   ${canonical.rawTargetCount} target(s); canonical view merged to ${targets.length}`);
    }
    console.log("");
    await printTargets(cel);
  });

browserCommand
  .command("ensure")
  .description("Launch or reuse the dedicated CEL browser instance on the preferred CDP port")
  .option("--url <url>", "Open this URL after the CEL browser is ready")
  .option("--json", "Output raw JSON")
  .action(async (opts: BrowserEnsureOptions) => {
    const cel = new Cel();
    const result = await ensureDedicatedCdpBrowser({
      cel,
      url: opts.url,
    });

    if (opts.json) {
      printJson(result);
      return;
    }

    console.log(result.message);
    console.log(`Port: ${result.status.port}`);
    console.log(`Ready: ${result.status.ready ? "yes" : "no"}`);
    console.log(`CEL-owned: ${result.status.ownedByCel ? "yes" : "no"}`);
    console.log(`Targets: ${result.status.targetCount}`);

    if (result.browser) {
      console.log(`Browser: ${result.browser.appName}`);
    } else if (result.status.browserVersion) {
      console.log(`Browser: ${result.status.browserVersion}`);
    }

    if (!result.ok) {
      process.exitCode = 1;
      return;
    }

    if (opts.url) {
      console.log(`URL: ${opts.url}`);
    }
  });

browserCommand
  .command("targets")
  .description("List discovered CDP targets and show which one CEL would prefer")
  .option("--json", "Output raw JSON")
  .action(async (opts: BrowserStatusOptions) => {
    const cel = new Cel();
    const targets = await discoverCanonicalCdpTargets(cel);
    const preferred = selectPreferredCdpTarget(
      targets,
      cel.getQuickContext().app,
      getPreferredCelCdpPort(),
    );

    if (opts.json) {
      printJson({
        preferred_target: preferred ?? null,
        targets,
      });
      return;
    }

    await printTargets(cel);
  });
