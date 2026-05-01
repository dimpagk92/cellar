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
import { execFileSync } from "node:child_process";

const CelConfigSchema = z.object({
  /** Database path for cel-store. */
  dbPath: z.string().default("~/.cellar/cel-store.db"),

  /** Default LLM provider (openai, gemini, anthropic). */
  llmProvider: z.string().optional(),

  /** LLM model for planning. */
  llmPlannerModel: z.string().optional(),

  /** LLM API key. */
  llmApiKey: z.string().optional(),

  /** LLM endpoint override. */
  llmEndpoint: z.string().optional(),

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

let cachedClaudeCodeOauthTokens: string[] | undefined;

/** Shape of the `[llm]` section in `~/.cellar/config.toml`. */
interface ConfigFileLlm {
  provider?: string;
  model?: string;
  apiKey?: string;
  endpoint?: string;
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
      else if (kv[1] === "api_key") out.apiKey = kv[2];
      else if (kv[1] === "endpoint") out.endpoint = kv[2];
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
    llmApiKey: process.env.CEL_LLM_API_KEY ?? configFileLlm.apiKey,
    llmEndpoint: process.env.CEL_LLM_ENDPOINT ?? configFileLlm.endpoint,
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

/**
 * Native role-aware LLM calls currently resolve configuration from env vars.
 * Mirror values detected from `~/.cellar/config.toml` into `process.env`
 * so LangGraph paths can use the same defaults as the rest of the repo.
 */
export function hydrateLlmEnvFromConfig(config: CelConfig = celConfig): void {
  if (config.llmProvider && !process.env.CEL_LLM_PROVIDER) {
    process.env.CEL_LLM_PROVIDER = config.llmProvider;
  }
  if (config.llmPlannerModel && !process.env.CEL_LLM_MODEL) {
    process.env.CEL_LLM_MODEL = config.llmPlannerModel;
  }
  if (config.llmApiKey && !process.env.CEL_LLM_API_KEY) {
    process.env.CEL_LLM_API_KEY = config.llmApiKey;
  }
  if (config.llmEndpoint && !process.env.CEL_LLM_ENDPOINT) {
    process.env.CEL_LLM_ENDPOINT = config.llmEndpoint;
  }
  if (
    config.llmProvider === "anthropic" &&
    !process.env.CLAUDE_CODE_OAUTH_TOKEN &&
    !process.env.ANTHROPIC_API_KEY &&
    !process.env.CEL_LLM_API_KEY
  ) {
    const token = discoverClaudeCodeOauthTokens()[0];
    if (token) {
      process.env.CLAUDE_CODE_OAUTH_TOKEN = token;
    }
  }
}

export function hasConfiguredLlmAuth(config: CelConfig = celConfig): boolean {
  if (config.llmApiKey) {
    return true;
  }

  switch (config.llmProvider) {
    case "anthropic":
      return Boolean(
        process.env.CEL_LLM_API_KEY ||
        process.env.ANTHROPIC_API_KEY ||
        process.env.CLAUDE_CODE_OAUTH_TOKEN ||
        discoverClaudeCodeOauthTokens()[0],
      );
    case "openai":
      return Boolean(
        process.env.CEL_LLM_API_KEY ||
        process.env.OPENAI_API_KEY,
      );
    case "gemini":
      return Boolean(
        process.env.CEL_LLM_API_KEY ||
        process.env.GEMINI_API_KEY ||
        process.env.GOOGLE_GEMINI_API_KEY ||
        process.env.GOOGLE_API_KEY,
      );
    case "ollama":
      return true;
    case "huggingface":
      return Boolean(
        process.env.CEL_LLM_API_KEY ||
        process.env.HUGGINGFACE_API_KEY ||
        process.env.HF_API_KEY,
      );
    case "custom":
      return Boolean(process.env.CEL_LLM_API_KEY || config.llmEndpoint);
    default:
      return false;
  }
}

export function discoverClaudeCodeOauthTokens(): string[] {
  if (cachedClaudeCodeOauthTokens !== undefined) {
    return [...cachedClaudeCodeOauthTokens];
  }

  try {
    const ps = execFileSync("ps", ["-o", "pid=,etime=,command=", "-ax"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    const candidatePids = ps
      .split("\n")
      .map(parseClaudeCodeProcessLine)
      .filter((entry): entry is { pid: string; etimes: number } => Boolean(entry))
      .sort((left, right) => left.etimes - right.etimes)
      .map((entry) => entry.pid)
      .slice(0, 8);

    const tokens: string[] = [];

    for (const pid of candidatePids) {
      const envDump = execFileSync("ps", ["eww", "-p", pid], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      });
      const match = /CLAUDE_CODE_OAUTH_TOKEN=([^ ]+)/.exec(envDump);
      const token = match?.[1]?.trim();
      if (token && !tokens.includes(token)) {
        tokens.push(token);
      }
    }

    if (tokens.length > 0) {
      cachedClaudeCodeOauthTokens = tokens;
      return [...tokens];
    }
  } catch {
    // Best-effort only. On some systems `ps eww` may be unavailable or blocked.
  }

  return [];
}

function parseClaudeCodeProcessLine(line: string): { pid: string; etimes: number } | null {
  const trimmed = line.trim();
  if (!trimmed) {
    return null;
  }
  const match = /^(\d+)\s+(\S+)\s+(.*)$/.exec(trimmed);
  if (!match) {
    return null;
  }
  const [, pid, elapsedRaw, command] = match;
  if (
    !command.includes("/claude-code/") ||
    !command.includes("/Contents/MacOS/claude")
  ) {
    return null;
  }
  return {
    pid,
    etimes: parsePsElapsedSeconds(elapsedRaw),
  };
}

function parsePsElapsedSeconds(raw: string): number {
  const trimmed = raw.trim();
  if (!trimmed) {
    return Number.MAX_SAFE_INTEGER;
  }

  const [daysPart, timePart] = trimmed.includes("-")
    ? trimmed.split("-", 2)
    : [null, trimmed];
  const segments = timePart.split(":").map((value) => Number.parseInt(value, 10));
  if (segments.some((value) => Number.isNaN(value))) {
    return Number.MAX_SAFE_INTEGER;
  }

  let seconds = 0;
  if (segments.length === 3) {
    seconds += segments[0] * 3600 + segments[1] * 60 + segments[2];
  } else if (segments.length === 2) {
    seconds += segments[0] * 60 + segments[1];
  } else if (segments.length === 1) {
    seconds += segments[0];
  } else {
    return Number.MAX_SAFE_INTEGER;
  }

  if (daysPart) {
    const days = Number.parseInt(daysPart, 10);
    if (!Number.isNaN(days)) {
      seconds += days * 24 * 3600;
    }
  }

  return seconds;
}

/** Resolve ~ to home directory in a path. */
export function resolvePath(p: string): string {
  return p.replace(/^~/, celConfig.homeDir);
}
