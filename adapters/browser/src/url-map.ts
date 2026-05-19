/**
 * URL Map — Anti-Hallucination for LLM Extraction
 *
 * Collects URLs from page elements and replaces them with stable numeric
 * identifiers ([URL_1], [URL_2], etc.) before sending context to the LLM.
 * After LLM response, restores the real URLs.
 *
 * This prevents LLMs from hallucinating, mangling, or truncating URLs
 * in extraction and planning tasks.
 *
 * Inspired by Stagehand v3's URL anti-hallucination in extract().
 */

import type { ContextElement } from "@cellar/agent/runtime";

/** URL pattern to match in text. */
const URL_PATTERN = /https?:\/\/[^\s"'<>]+/g;

/**
 * Bidirectional URL map for substitution and restoration.
 */
export class UrlMap {
  private idToUrl = new Map<number, string>();
  private urlToId = new Map<string, number>();
  private nextId = 1;

  /** Build a URL map from context elements. */
  static build(elements: ContextElement[]): UrlMap {
    const map = new UrlMap();

    for (const el of elements) {
      // Extract URLs from element values, labels, and descriptions
      const sources = [el.value, el.label, el.description].filter(Boolean);
      for (const source of sources) {
        const matches = source!.match(URL_PATTERN);
        if (matches) {
          for (const url of matches) {
            map.add(url);
          }
        }
      }
    }

    return map;
  }

  /** Add a URL to the map. Returns its numeric ID. */
  add(url: string): number {
    const existing = this.urlToId.get(url);
    if (existing !== undefined) return existing;

    const id = this.nextId++;
    this.idToUrl.set(id, url);
    this.urlToId.set(url, id);
    return id;
  }

  /** Replace all URLs in text with [URL_N] placeholders. */
  substitute(text: string): string {
    if (this.urlToId.size === 0) return text;

    // Sort URLs by length descending to avoid partial matches
    const urls = [...this.urlToId.entries()].sort(
      (a, b) => b[0].length - a[0].length,
    );

    let result = text;
    for (const [url, id] of urls) {
      result = result.replaceAll(url, `[URL_${id}]`);
    }
    return result;
  }

  /** Restore [URL_N] placeholders back to real URLs. */
  restore(text: string): string {
    return text.replace(/\[URL_(\d+)\]/g, (_match, idStr) => {
      const id = parseInt(idStr, 10);
      return this.idToUrl.get(id) ?? `[URL_${id}]`;
    });
  }

  /** Get the URL for a numeric ID. */
  getUrl(id: number): string | undefined {
    return this.idToUrl.get(id);
  }

  /** Get the numeric ID for a URL. */
  getId(url: string): number | undefined {
    return this.urlToId.get(url);
  }

  /** Number of URLs in the map. */
  get size(): number {
    return this.idToUrl.size;
  }

  /** Serialize to a plain object for JSON transport. */
  toJSON(): Record<number, string> {
    const obj: Record<number, string> = {};
    for (const [id, url] of this.idToUrl) {
      obj[id] = url;
    }
    return obj;
  }

  /** Create from a serialized JSON object. */
  static fromJSON(obj: Record<number, string>): UrlMap {
    const map = new UrlMap();
    for (const [idStr, url] of Object.entries(obj)) {
      const id = parseInt(idStr, 10);
      map.idToUrl.set(id, url);
      map.urlToId.set(url, id);
      if (id >= map.nextId) map.nextId = id + 1;
    }
    return map;
  }
}
