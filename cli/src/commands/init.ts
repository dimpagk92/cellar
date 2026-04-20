import { Command } from "commander";
import { createInterface, Interface } from "node:readline/promises";
import { stdin as input, stdout as output } from "node:process";
import { writeFileSync, existsSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";
import { spawn } from "node:child_process";

const CONFIG_DIR = join(homedir(), ".cellar");
const CONFIG_PATH = join(CONFIG_DIR, "config.toml");
const OLLAMA_URL = "http://localhost:11434";
const DEFAULT_GEMMA = "gemma4:e4b";

interface LlmConfig {
  provider: string;
  model?: string;
  apiKey?: string;
  endpoint?: string;
}

export const initCommand = new Command("init")
  .description("Interactive first-run setup — pick an LLM provider or install Gemma 4 locally")
  .action(async () => {
    const rl = createInterface({ input, output });
    try {
      console.log("CEL first-run setup");
      console.log("===================\n");

      if (existsSync(CONFIG_PATH)) {
        const ans = await rl.question(`${CONFIG_PATH} already exists. Overwrite? [y/N] `);
        if (!/^y(es)?$/i.test(ans.trim())) {
          console.log("Aborted.");
          return;
        }
      }

      const ollamaUp = await pingOllama();
      console.log(ollamaUp
        ? "Detected: Ollama running at localhost:11434\n"
        : "Ollama not detected.\n");

      console.log("Pick a provider:");
      console.log("  [1] Gemini    — cloud, fast, cheap         (needs GEMINI_API_KEY)");
      console.log("  [2] Anthropic — cloud, strong reasoning    (needs ANTHROPIC_API_KEY)");
      console.log("  [3] OpenAI    — cloud                      (needs OPENAI_API_KEY)");
      console.log(`  [4] Ollama + Gemma 4 E4B — local, ~4GB    (${ollamaUp ? "ready" : "requires Ollama install"})`);
      console.log("  [5] Cancel");

      const choice = (await rl.question("\nChoice [1-5]: ")).trim();
      let cfg: LlmConfig | undefined;
      switch (choice) {
        case "1": cfg = await setupApi(rl, "gemini"); break;
        case "2": cfg = await setupApi(rl, "anthropic"); break;
        case "3": cfg = await setupApi(rl, "openai"); break;
        case "4": cfg = await setupOllama(rl, ollamaUp); break;
        default:
          console.log("Cancelled.");
          return;
      }

      if (!cfg) {
        console.log("Setup incomplete — no config written.");
        return;
      }

      writeConfig(cfg);
      console.log(`\nWrote ${CONFIG_PATH}`);
      console.log("Next: `cellar status` to verify, or `cellar mcp` to start the MCP server.");
    } finally {
      rl.close();
    }
  });

async function pingOllama(): Promise<boolean> {
  try {
    const res = await fetch(`${OLLAMA_URL}/api/tags`, { signal: AbortSignal.timeout(2000) });
    return res.ok;
  } catch {
    return false;
  }
}

async function setupApi(rl: Interface, provider: string): Promise<LlmConfig | undefined> {
  const envVar = provider === "gemini" ? "GEMINI_API_KEY"
    : provider === "anthropic" ? "ANTHROPIC_API_KEY"
    : "OPENAI_API_KEY";

  const existing = process.env[envVar];
  if (existing) {
    const keep = (await rl.question(`Found ${envVar} in env. Use it? [Y/n] `)).trim();
    if (!/^n(o)?$/i.test(keep)) {
      return { provider };
    }
  }

  const key = (await rl.question(`Paste your ${envVar} (or blank to cancel): `)).trim();
  if (!key) return undefined;
  return { provider, apiKey: key };
}

async function setupOllama(rl: Interface, ollamaUp: boolean): Promise<LlmConfig | undefined> {
  if (!ollamaUp) {
    console.log("\nOllama isn't running. Install it first:");
    console.log("  brew install ollama && brew services start ollama");
    console.log("Then re-run `cellar init`.");
    return undefined;
  }

  const tags = await listOllamaModels();
  const gemmaTags = tags.filter(t => t.startsWith("gemma"));

  if (gemmaTags.length > 0) {
    console.log(`Found existing Gemma model(s): ${gemmaTags.join(", ")}`);
    const pick = gemmaTags.find(t => t.startsWith("gemma4:")) ?? gemmaTags[0];
    return { provider: "ollama", model: pick };
  }

  console.log(`\nGemma 4 E4B (~4GB) is not installed locally.`);
  const ans = (await rl.question(`Pull ${DEFAULT_GEMMA} now? [Y/n] `)).trim();
  if (/^n(o)?$/i.test(ans)) return undefined;

  const ok = await pullOllamaModel(DEFAULT_GEMMA);
  if (!ok) {
    console.log("Pull failed.");
    return undefined;
  }
  return { provider: "ollama", model: DEFAULT_GEMMA };
}

async function listOllamaModels(): Promise<string[]> {
  try {
    const res = await fetch(`${OLLAMA_URL}/api/tags`);
    const data = await res.json() as { models?: { name: string }[] };
    return (data.models ?? []).map(m => m.name);
  } catch {
    return [];
  }
}

function pullOllamaModel(model: string): Promise<boolean> {
  return new Promise((resolve) => {
    console.log(`\nRunning: ollama pull ${model}`);
    const proc = spawn("ollama", ["pull", model], { stdio: "inherit" });
    proc.on("exit", (code) => resolve(code === 0));
    proc.on("error", () => {
      console.log("Failed to spawn `ollama`. Is it installed and on PATH?");
      resolve(false);
    });
  });
}

function writeConfig(cfg: LlmConfig) {
  if (!existsSync(CONFIG_DIR)) mkdirSync(CONFIG_DIR, { recursive: true });
  const lines: string[] = ["# Written by `cellar init`", "", "[llm]", `provider = "${cfg.provider}"`];
  if (cfg.model) lines.push(`model = "${cfg.model}"`);
  if (cfg.endpoint) lines.push(`endpoint = "${cfg.endpoint}"`);
  if (cfg.apiKey) lines.push(`api_key = "${cfg.apiKey}"`);
  writeFileSync(CONFIG_PATH, lines.join("\n") + "\n", { mode: 0o600 });
}
