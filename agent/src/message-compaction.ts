/**
 * Message compaction — summarizes long conversation histories to stay within
 * token budgets. Based on Browser-use's approach of periodically compacting
 * older history items into a summary while keeping recent items verbatim.
 */

import type { PlannerStepRecord } from "./types.js";

export interface CompactionConfig {
  /** Trigger compaction after this many steps. Default: 15. */
  compactEveryNSteps?: number;
  /** Only compact if history text exceeds this char count. Default: 40000. */
  triggerCharCount?: number;
  /** Max chars for compacted summary. Default: 4000. */
  summaryMaxChars?: number;
  /** Number of recent items to keep verbatim after compaction. Default: 3. */
  keepLastItems?: number;
}

export interface CompactedHistory {
  summary: string;
  keptItems: PlannerStepRecord[];
  omittedCount: number;
}

const DEFAULT_CONFIG: Required<CompactionConfig> = {
  compactEveryNSteps: 15,
  triggerCharCount: 40_000,
  summaryMaxChars: 4_000,
  keepLastItems: 3,
};

/**
 * Format history items as human-readable text suitable for LLM summarization.
 */
export function formatHistoryForCompaction(history: PlannerStepRecord[]): string {
  return history
    .map((record) => {
      const status = record.success ? "OK" : `FAIL: ${record.error ?? "unknown"}`;
      const actionStr = formatAction(record.action);
      return `Step ${record.step_index}: [${status}] ${actionStr}`;
    })
    .join("\n");
}

function formatAction(action: PlannerStepRecord["action"]): string {
  switch (action.type) {
    case "click":
      return `click(${action.target_id})`;
    case "type":
      return `type(${action.target_id}, "${truncate(action.text, 40)}")`;
    case "set_value":
      return `set_value(${action.target_id}, "${truncate(action.value, 40)}")`;
    case "key":
      return `key(${action.key})`;
    case "key_combo":
      return `key_combo(${action.keys.join("+")})`;
    case "scroll":
      return `scroll(dx=${action.dx}, dy=${action.dy})`;
    case "drag":
      return `drag(${action.from_x},${action.from_y} -> ${action.to_x},${action.to_y})`;
    case "wait":
      return `wait(${action.ms}ms)`;
    case "custom":
      return `custom(${action.adapter}.${action.action})`;
    case "done":
      return `done("${truncate(action.summary, 60)}")`;
    case "fail":
      return `fail("${truncate(action.reason, 60)}")`;
    default:
      return JSON.stringify(action);
  }
}

function truncate(str: string, maxLen: number): string {
  return str.length > maxLen ? str.slice(0, maxLen) + "..." : str;
}

/**
 * Check if compaction should trigger and, if so, compact the history.
 *
 * @param history - The full step history.
 * @param config - Compaction configuration (partial; defaults applied).
 * @param compactFn - Injected LLM summarizer. Receives formatted text +
 *   max chars, returns a summary string.
 * @returns The compacted history, or null if compaction was not triggered.
 */
export async function compactHistory(
  history: PlannerStepRecord[],
  config: CompactionConfig,
  compactFn: (text: string, maxChars: number) => Promise<string>,
): Promise<CompactedHistory | null> {
  const cfg = { ...DEFAULT_CONFIG, ...config };

  // Check step count trigger
  if (history.length < cfg.compactEveryNSteps) {
    return null;
  }

  // Check char count trigger
  const fullText = formatHistoryForCompaction(history);
  if (fullText.length < cfg.triggerCharCount) {
    return null;
  }

  // Split into items to compact vs. items to keep
  const keepCount = Math.min(cfg.keepLastItems, history.length);
  const toCompact = history.slice(0, history.length - keepCount);
  const keptItems = history.slice(history.length - keepCount);

  if (toCompact.length === 0) {
    return null;
  }

  const textToCompact = formatHistoryForCompaction(toCompact);
  const summary = await compactFn(textToCompact, cfg.summaryMaxChars);

  return {
    summary,
    keptItems,
    omittedCount: toCompact.length,
  };
}
