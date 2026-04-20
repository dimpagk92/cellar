/**
 * Notebook — lightweight cross-replan data persistence.
 *
 * Simple key-value store for data the agent discovers during execution
 * (prices, URLs, confirmation numbers, observations). Persists across
 * replans so that strategy changes don't lose discovered data.
 *
 * Injected into the LLM prompt as a single compact line when non-empty:
 *   SAVED DATA: cheapest=$149, dates=Mar15-17, booking_url=hotels.com/room/123
 *
 * Max 10 entries, capped at ~50 tokens in the prompt.
 */

// ─── Types ──────────────────────────────────────────────────────────────────

export type NotebookCategory = "data" | "url" | "observation" | "error";

export interface NotebookEntry {
  key: string;
  value: string;
  source: string;        // e.g. "step-5", "replan-2"
  category: NotebookCategory;
  timestamp: number;
}

// ─── Constants ──────────────────────────────────────────────────────────────

/** Maximum number of entries. Oldest entries evicted when exceeded. */
const MAX_ENTRIES = 10;

// ─── Notebook ───────────────────────────────────────────────────────────────

export class Notebook {
  private entries: Map<string, NotebookEntry> = new Map();

  /** Write or update an entry. Upserts by key. */
  write(key: string, value: string, source: string, category: NotebookCategory): void {
    this.entries.set(key, { key, value, source, category, timestamp: Date.now() });

    // Evict oldest if over max
    if (this.entries.size > MAX_ENTRIES) {
      let oldestKey: string | null = null;
      let oldestTime = Infinity;
      for (const [k, entry] of this.entries) {
        if (entry.timestamp < oldestTime) {
          oldestTime = entry.timestamp;
          oldestKey = k;
        }
      }
      if (oldestKey) this.entries.delete(oldestKey);
    }
  }

  /** Read a single entry by key. */
  read(key: string): string | undefined {
    return this.entries.get(key)?.value;
  }

  /** Get all entries as an array. */
  all(): NotebookEntry[] {
    return Array.from(this.entries.values());
  }

  /** Number of entries. */
  get size(): number {
    return this.entries.size;
  }

  /** Whether the notebook has any entries. */
  get isEmpty(): boolean {
    return this.entries.size === 0;
  }

  /**
   * Format for LLM prompt injection — single compact line.
   * Returns empty string if no entries.
   *
   * Example: "SAVED DATA: cheapest=$149, dates=Mar15-17, url=hotels.com/search"
   */
  toPromptContext(): string {
    if (this.entries.size === 0) return "";

    const pairs = Array.from(this.entries.values())
      .map(e => `${e.key}=${e.value}`)
      .join(", ");

    return `SAVED DATA: ${pairs}`;
  }

  /**
   * Format for GoalResult summary — grouped by category.
   */
  toSummary(): string {
    if (this.entries.size === 0) return "";

    const byCategory = new Map<NotebookCategory, NotebookEntry[]>();
    for (const entry of this.entries.values()) {
      const list = byCategory.get(entry.category) ?? [];
      list.push(entry);
      byCategory.set(entry.category, list);
    }

    const parts: string[] = [];
    for (const [category, items] of byCategory) {
      const label = category.charAt(0).toUpperCase() + category.slice(1);
      const formatted = items.map(e => `  ${e.key}: ${e.value}`).join("\n");
      parts.push(`${label}:\n${formatted}`);
    }

    return parts.join("\n");
  }

  /** Deep-copy entries for checkpoint storage. */
  snapshot(): NotebookEntry[] {
    return Array.from(this.entries.values()).map(e => ({ ...e }));
  }

  /** Restore from a checkpoint snapshot. */
  restoreFromSnapshot(entries: NotebookEntry[]): void {
    this.entries.clear();
    for (const entry of entries) {
      this.entries.set(entry.key, { ...entry });
    }
  }

  /** Clear all entries. */
  clear(): void {
    this.entries.clear();
  }
}
