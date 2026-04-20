/**
 * Cache Storage Backends
 *
 * Provides filesystem and in-memory storage for action and agent caches.
 * The interface is simple: get/set/delete/has by string key.
 */

import * as fs from "node:fs";
import * as path from "node:path";

/** A cached action sequence. */
export interface CachedAction {
  /** The planned action (click, type, key, etc.) */
  action: Record<string, unknown>;
  /** Variable placeholders found in the action (e.g. { "%username%": "username" }) */
  variablePlaceholders: Record<string, string>;
}

/** A single cache entry stored on disk or in memory. */
export interface CacheEntry {
  version: 1;
  key: string;
  instruction: string;
  stateFingerprint: string;
  variableKeys: string[];
  actions: CachedAction[];
  createdAt: number;
  lastUsedAt: number;
  hitCount: number;
}

/** Storage backend interface for caches. */
export interface CacheStorage {
  get(key: string): Promise<CacheEntry | null>;
  set(key: string, entry: CacheEntry): Promise<void>;
  delete(key: string): Promise<void>;
  has(key: string): Promise<boolean>;
}

// ─── Filesystem Storage ──────────────────────────────────────────────────────

/**
 * Filesystem-based cache storage.
 * Stores entries as JSON files at `{baseDir}/{hash[0:2]}/{hash}.json`.
 * The two-character prefix directory prevents filesystem issues with many files.
 */
export class FilesystemCacheStorage implements CacheStorage {
  private baseDir: string;

  constructor(baseDir?: string) {
    const home = process.env.HOME ?? ".";
    this.baseDir = baseDir ?? path.join(home, ".cellar", "cache", "act");
  }

  private entryPath(key: string): string {
    const prefix = key.slice(0, 2);
    return path.join(this.baseDir, prefix, `${key}.json`);
  }

  async get(key: string): Promise<CacheEntry | null> {
    const filePath = this.entryPath(key);
    try {
      const data = fs.readFileSync(filePath, "utf-8");
      const entry = JSON.parse(data) as CacheEntry;
      // Update lastUsedAt and hitCount on access
      entry.lastUsedAt = Date.now();
      entry.hitCount++;
      fs.writeFileSync(filePath, JSON.stringify(entry, null, 2), "utf-8");
      return entry;
    } catch {
      return null;
    }
  }

  async set(key: string, entry: CacheEntry): Promise<void> {
    const filePath = this.entryPath(key);
    const dir = path.dirname(filePath);
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(filePath, JSON.stringify(entry, null, 2), "utf-8");
  }

  async delete(key: string): Promise<void> {
    const filePath = this.entryPath(key);
    try {
      fs.unlinkSync(filePath);
    } catch {
      // File doesn't exist — ignore
    }
  }

  async has(key: string): Promise<boolean> {
    return fs.existsSync(this.entryPath(key));
  }
}

// ─── In-Memory Storage ───────────────────────────────────────────────────────

/**
 * In-memory LRU cache storage.
 * Evicts oldest entries when maxSize is exceeded.
 */
export class MemoryCacheStorage implements CacheStorage {
  private store = new Map<string, CacheEntry>();
  private maxSize: number;

  constructor(maxSize = 500) {
    this.maxSize = maxSize;
  }

  async get(key: string): Promise<CacheEntry | null> {
    const entry = this.store.get(key);
    if (!entry) return null;
    // LRU: move to end
    this.store.delete(key);
    entry.lastUsedAt = Date.now();
    entry.hitCount++;
    this.store.set(key, entry);
    return entry;
  }

  async set(key: string, entry: CacheEntry): Promise<void> {
    // Evict oldest if at capacity
    if (this.store.size >= this.maxSize && !this.store.has(key)) {
      const oldest = this.store.keys().next().value;
      if (oldest !== undefined) {
        this.store.delete(oldest);
      }
    }
    this.store.set(key, entry);
  }

  async delete(key: string): Promise<void> {
    this.store.delete(key);
  }

  async has(key: string): Promise<boolean> {
    return this.store.has(key);
  }

  /** Get current cache size (for testing). */
  get size(): number {
    return this.store.size;
  }
}
