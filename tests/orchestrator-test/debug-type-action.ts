import { config } from "dotenv";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
config({ path: path.join(__dirname, "..", "..", "benchmarks", ".env") });

process.env.CEL_LLM_PROVIDER = "gemini";
process.env.CEL_LLM_MODEL = "gemini-2.5-flash";
process.env.CEL_LLM_API_KEY = process.env.GOOGLE_GEMINI_API_KEY || "";

import { Cel, runGoal, type GoalRunnerCallbacks, type ScreenContext, type PlannedAction } from "../../agent/src/index.js";
import { BrowserAdapter } from "../../adapters/browser/src/index.js";

const MINIWOB_DIR = path.join(__dirname, "..", "..", "benchmarks", "miniwob-plusplus", "miniwob", "html", "miniwob");

async function executeBrowserAction(adapter: BrowserAdapter, action: PlannedAction, context: ScreenContext): Promise<boolean> {
  console.log(`    [exec] action=${action.type} target=${"target_id" in action ? action.target_id : "none"}`);
  const start = Date.now();

  switch (action.type) {
    case "click": {
      const el = context.elements.find((e) => e.id === action.target_id);
      if (!el) { console.log(`    [exec] Element not found: ${action.target_id}`); throw new Error(`Element not found: ${action.target_id}`); }
      console.log(`    [exec] click via ${el.properties?.css_selector ? "CSS" : el.bounds ? "bounds" : "???"}: css=${el.properties?.css_selector} bounds=${el.bounds?.x},${el.bounds?.y}`);
      const r = await adapter.executeAction("click", {
        css_selector: el.properties?.css_selector,
        ...(el.bounds ? { x: el.bounds.x + Math.floor(el.bounds.width / 2), y: el.bounds.y + Math.floor(el.bounds.height / 2) } : {}),
      });
      console.log(`    [exec] click done in ${Date.now() - start}ms, result=${r}`);
      return r;
    }
    case "type": {
      const el = action.target_id ? context.elements.find((e) => e.id === action.target_id) : null;
      console.log(`    [exec] type target=${action.target_id} text="${action.text}" el_type=${el?.element_type} css=${el?.properties?.css_selector} bounds=${el?.bounds?.x},${el?.bounds?.y}`);
      if (el?.properties?.css_selector) {
        console.log(`    [exec] typing via CSS selector: ${el.properties.css_selector}`);
        const r = await adapter.executeAction("type", { selector: el.properties.css_selector, text: action.text, clearFirst: true });
        console.log(`    [exec] type done in ${Date.now() - start}ms, result=${r}`);
        return r;
      }
      if (el?.bounds) {
        console.log(`    [exec] typing via bounds: ${el.bounds.x},${el.bounds.y}`);
        const r = await adapter.executeAction("type", { x: el.bounds.x + Math.floor(el.bounds.width / 2), y: el.bounds.y + Math.floor(el.bounds.height / 2), text: action.text, clearFirst: true });
        console.log(`    [exec] type done in ${Date.now() - start}ms, result=${r}`);
        return r;
      }
      console.log(`    [exec] typing without target (keyboard)`);
      const r = await adapter.executeAction("type", { text: action.text });
      console.log(`    [exec] type done in ${Date.now() - start}ms, result=${r}`);
      return r;
    }
    case "key": return adapter.executeAction("press_key", { key: action.key });
    case "key_combo": return adapter.executeAction("key_combo", { keys: action.keys });
    case "done": case "fail": case "extract": return true;
    default: return true;
  }
}

async function main() {
  const cel = new Cel();
  const adapter = new BrowserAdapter({
    cel, browser: "chromium", useCdp: true, headless: true, stealth: false,
    viewport: { width: 320, height: 320 }, sanitize: true, incrementalUpdates: false,
  });
  await adapter.connect();

  for (const task of ["enter-password", "login-user"]) {
    console.log(`\n=== ${task} ===`);
    await adapter.navigate(`file://${path.join(MINIWOB_DIR, task + ".html")}`);
    await new Promise(r => setTimeout(r, 800));
    try { await adapter.evaluate('core.startEpisodeReal()'); } catch {}
    await new Promise(r => setTimeout(r, 500));

    const goal = await adapter.evaluate<string>('(document.getElementById("query")?.textContent || "").trim()');
    console.log(`Goal: ${goal}`);

    const callbacks: GoalRunnerCallbacks = {
      getContext: () => adapter.getContext(),
      screenshot: async () => adapter.screenshot(),
      stateFingerprint: () => adapter.getPageUrl(),
      executeAction: (action, ctx) => executeBrowserAction(adapter, action, ctx),
      verifyGoal: async () => {
        try {
          const done = await adapter.evaluate<boolean>("window.WOB_DONE_GLOBAL === true");
          if (!done) return false;
          const reward = await adapter.evaluate<number>("window.WOB_REWARD_GLOBAL || 0");
          return reward > 0.5;
        } catch { return false; }
      },
      onStepPlanned: (step, i) => {
        console.log(`    [plan] Step ${i + 1}: ${step.action.type} | confidence=${step.confidence} | ${step.reasoning.slice(0, 80)}`);
      },
    };

    const start = Date.now();
    const result = await runGoal(cel, {
      goal, maxSteps: 8, taskTimeout: 30_000, enableVision: false, selfHeal: true, skipRouter: true,
    }, callbacks);

    let reward = 0;
    try { reward = await adapter.evaluate<number>("window.WOB_RAW_REWARD_GLOBAL || 0"); } catch {}
    console.log(`Result: ${result.status} | reward=${reward} | steps=${result.totalSteps} | ${Date.now() - start}ms`);
  }

  await adapter.disconnect();
}
main().catch(e => { console.error(e); process.exit(1); });
