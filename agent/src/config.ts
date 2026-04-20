/**
 * Centralized configuration for CEL agent.
 *
 * All environment variables are loaded and validated here. Consumers
 * import `celConfig` instead of reading `process.env` directly.
 */

import { z } from "zod";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

const CelConfigSchema = z.object({
  /** Database path for cel-store. */
  dbPath: z.string().default("~/.cellar/cel-store.db"),

  /** Default LLM provider (openai, gemini, anthropic). */
  llmProvider: z.string().optional(),

  /** LLM model for planning. */
  llmPlannerModel: z.string().optional(),

  /** LLM model for escalation (used after consecutive failures). */
  llmEscalationModel: z.string().optional(),

  /** LLM model for orchestrator decomposition. */
  llmOrchestratorModel: z.string().optional(),

  /** LLM model for validation. */
  llmValidatorModel: z.string().optional(),

  /** Workflows directory. */
  workflowsDir: z.string().default("~/.cellar/workflows"),

  /** Log level (debug, info, warn, error). */
  logLevel: z.enum(["debug", "info", "warn", "error"]).default("info"),

  /** Home directory. */
  homeDir: z.string().default(process.env.HOME ?? ""),
});

export type CelConfig = z.infer<typeof CelConfigSchema>;

/** Shape of the `[llm]` section in `~/.cellar/config.toml`. */
interface ConfigFileLlm {
  provider?: string;
  model?: string;
}

/**
 * Read `~/.cellar/config.toml` for the provider/model defaults written by `cellar init`.
 * Only parses the `[llm]` section's `provider` and `model` fields — keeps the reader
 * trivial so we don't need a TOML dep on the TS side.
 */
function readConfigFileLlm(): ConfigFileLlm {
  const path = join(homedir(), ".cellar", "config.toml");
  if (!existsSync(path)) return {};
  let inLlm = false;
  const out: ConfigFileLlm = {};
  try {
    for (const raw of readFileSync(path, "utf8").split("\n")) {
      const line = raw.replace(/#.*$/, "").trim();
      if (!line) continue;
      const section = /^\[([^\]]+)\]$/.exec(line);
      if (section) { inLlm = section[1].trim() === "llm"; continue; }
      if (!inLlm) continue;
      const kv = /^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*"([^"]*)"$/.exec(line);
      if (!kv) continue;
      if (kv[1] === "provider") out.provider = kv[2];
      else if (kv[1] === "model") out.model = kv[2];
    }
  } catch {
    // Best-effort — fall through to env-only detection
  }
  return out;
}

const configFileLlm = readConfigFileLlm();

/** Detect the best available LLM provider from environment or config file. */
function detectDefaultProvider(): string | undefined {
  if (process.env.CEL_LLM_PROVIDER) return process.env.CEL_LLM_PROVIDER;
  // Prefer Gemini Flash for planning (15x cheaper than Claude Sonnet)
  if (process.env.GEMINI_API_KEY) return "gemini";
  if (process.env.ANTHROPIC_API_KEY) return "anthropic";
  if (process.env.OPENAI_API_KEY) return "openai";
  if (configFileLlm.provider) return configFileLlm.provider;
  return undefined;
}

/** Detect the best default planner model for the chosen provider. */
function detectDefaultPlannerModel(provider?: string): string | undefined {
  if (process.env.CEL_LLM_PLANNER_MODEL) return process.env.CEL_LLM_PLANNER_MODEL;
  if (configFileLlm.model && configFileLlm.provider === provider) return configFileLlm.model;
  switch (provider) {
    case "gemini": return "gemini-2.5-flash";
    case "anthropic": return "claude-sonnet-4-20250514";
    case "openai": return "gpt-4o";
    case "ollama": return "gemma4:e4b";
    default: return undefined;
  }
}

/** Load and validate config from environment variables. */
function loadConfig(): CelConfig {
  const detectedProvider = detectDefaultProvider();
  const raw = {
    dbPath: process.env.CELLAR_DB_PATH ?? process.env.CEL_DB_PATH,
    llmProvider: detectedProvider,
    llmPlannerModel: detectDefaultPlannerModel(detectedProvider),
    llmEscalationModel: process.env.CEL_LLM_PLANNER_ESCALATION_MODEL ?? process.env.CEL_LLM_ESCALATION_MODEL,
    llmOrchestratorModel: process.env.CEL_LLM_ORCHESTRATOR_MODEL,
    llmValidatorModel: process.env.CEL_LLM_VALIDATOR_MODEL,
    workflowsDir: process.env.CELLAR_WORKFLOWS_DIR,
    logLevel: process.env.CEL_LOG_LEVEL,
    homeDir: process.env.HOME,
  };

  // Remove undefined values so Zod defaults apply
  const cleaned = Object.fromEntries(
    Object.entries(raw).filter(([, v]) => v !== undefined),
  );

  return CelConfigSchema.parse(cleaned);
}

/** Validated, typed configuration. Loaded once at import time. */
export const celConfig = loadConfig();

/** Resolve ~ to home directory in a path. */
export function resolvePath(p: string): string {
  return p.replace(/^~/, celConfig.homeDir);
}
