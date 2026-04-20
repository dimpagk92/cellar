import { config } from "dotenv";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
config({ path: path.join(__dirname, "..", "..", "benchmarks", ".env") });

const apiKey = process.env.GOOGLE_GEMINI_API_KEY || "";
process.env.CEL_LLM_PROVIDER = "gemini";
process.env.CEL_LLM_MODEL = "gemini-2.5-flash";
process.env.CEL_LLM_API_KEY = apiKey;

import { Cel } from "../../agent/src/index.js";
import { BrowserAdapter } from "../../adapters/browser/src/index.js";

const MINIWOB_DIR = path.join(__dirname, "..", "..", "benchmarks", "miniwob-plusplus", "miniwob", "html", "miniwob");

async function main() {
  const cel = new Cel();
  const adapter = new BrowserAdapter({
    cel, browser: "chromium", useCdp: true, headless: true, stealth: false,
    viewport: { width: 320, height: 320 }, sanitize: true, incrementalUpdates: false,
  });
  await adapter.connect();

  for (const task of ["click-test", "enter-text", "login-user", "click-dialog"]) {
    console.log(`\n=== ${task} ===`);
    await adapter.navigate(`file://${path.join(MINIWOB_DIR, task + ".html")}`);
    await new Promise(r => setTimeout(r, 800));

    // Trigger episode
    try {
      await adapter.evaluate('if (typeof core !== "undefined") core.startEpisodeReal()');
    } catch {}
    await new Promise(r => setTimeout(r, 500));

    // Get goal
    const goal = await adapter.evaluate<string>('(document.getElementById("query")?.textContent || "").trim()');
    console.log("Goal:", goal);

    // Check what the DOM extractor sees for input elements
    const rawInputs = await adapter.evaluate<any>(`(() => {
      const els = document.querySelectorAll('input, textarea, select');
      return Array.from(els).map(el => ({
        tag: el.tagName.toLowerCase(),
        type: el.getAttribute('type'),
        id: el.id,
        role: el.getAttribute('role'),
        ariaRole: el.computedRole || null,
        bid: el.getAttribute('bid'),
        visible: el.offsetWidth > 0,
        rect: { x: el.getBoundingClientRect().x, y: el.getBoundingClientRect().y, w: el.offsetWidth, h: el.offsetHeight },
      }));
    })()`);
    console.log("Raw inputs:", JSON.stringify(rawInputs));

    // Get context via adapter
    const ctx = await adapter.getContext();
    console.log(`Elements: ${ctx.elements.length}`);
    for (const el of ctx.elements) {
      const b = el.bounds ? `${el.bounds.x},${el.bounds.y} ${el.bounds.width}x${el.bounds.height}` : "no-bounds";
      console.log(`  [${el.id}] ${el.element_type} label='${(el.label ?? "").slice(0, 40)}' value='${(el.value ?? "").slice(0, 20)}' css=${el.properties?.css_selector ?? "none"} bounds=${b} actions=${el.actions?.join(",") ?? "none"}`);
    }
  }

  await adapter.disconnect();
}
main().catch(e => { console.error(e); process.exit(1); });
