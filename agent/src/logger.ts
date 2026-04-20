/**
 * Structured logger for CEL agent.
 *
 * Lightweight logger with configurable levels and structured JSON output.
 * Replaces bare console.log/warn/error throughout the codebase.
 *
 * Usage:
 *   import { log } from "./logger.js";
 *   log.info("Step completed", { stepIndex: 5, action: "click" });
 *   log.warn("Vision fallback triggered", { reason: "sparse context" });
 *   log.error("CDP connection failed", { error: err.message });
 *   log.debug("Element resolved", { id: "a11y:42", label: "Submit" });
 */

type LogLevel = "debug" | "info" | "warn" | "error";

const LEVEL_ORDER: Record<LogLevel, number> = {
  debug: 0,
  info: 1,
  warn: 2,
  error: 3,
};

/** Structured log entry. */
interface LogEntry {
  level: LogLevel;
  msg: string;
  ts: string;
  module?: string;
  [key: string]: unknown;
}

const envLevel = process.env.CEL_LOG_LEVEL;

let currentLevel: LogLevel =
  envLevel === "debug" || envLevel === "info" || envLevel === "warn" || envLevel === "error"
    ? envLevel
    : "info";

/** Set the minimum log level. Messages below this level are suppressed. */
export function setLogLevel(level: LogLevel): void {
  currentLevel = level;
}

/** Get current log level. */
export function getLogLevel(): LogLevel {
  return currentLevel;
}

function shouldLog(level: LogLevel): boolean {
  return LEVEL_ORDER[level] >= LEVEL_ORDER[currentLevel];
}

function emit(level: LogLevel, msg: string, data?: Record<string, unknown>): void {
  if (!shouldLog(level)) return;

  const entry: LogEntry = {
    level,
    msg,
    ts: new Date().toISOString(),
    ...data,
  };

  const output = JSON.stringify(entry);

  // Route based on level: error/warn to stderr, info/debug to stderr
  // (stdout is reserved for MCP protocol in stdio mode)
  if (level === "error") {
    console.error(output);
  } else if (level === "warn") {
    console.warn(output);
  } else {
    console.error(output); // stderr to avoid interfering with MCP stdout
  }
}

/** Create a scoped logger with a module name. */
export function createLogger(module: string) {
  return {
    debug: (msg: string, data?: Record<string, unknown>) =>
      emit("debug", msg, { module, ...data }),
    info: (msg: string, data?: Record<string, unknown>) =>
      emit("info", msg, { module, ...data }),
    warn: (msg: string, data?: Record<string, unknown>) =>
      emit("warn", msg, { module, ...data }),
    error: (msg: string, data?: Record<string, unknown>) =>
      emit("error", msg, { module, ...data }),
  };
}

/** Default logger (no module scope). */
export const log = {
  debug: (msg: string, data?: Record<string, unknown>) => emit("debug", msg, data),
  info: (msg: string, data?: Record<string, unknown>) => emit("info", msg, data),
  warn: (msg: string, data?: Record<string, unknown>) => emit("warn", msg, data),
  error: (msg: string, data?: Record<string, unknown>) => emit("error", msg, data),
};
