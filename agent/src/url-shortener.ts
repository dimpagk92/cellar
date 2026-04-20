/**
 * URL shortener — truncates long query strings/fragments to save tokens.
 *
 * If a URL's query string exceeds 100 characters, it is truncated to the
 * first 50 chars + "..." + an 8-character hash. The mapping is stored for
 * reverse lookup via expand().
 */

/** Max length of query string before truncation kicks in. */
const QUERY_THRESHOLD = 100;
/** How many characters of the query to keep. */
const KEEP_PREFIX = 50;
/** Length of the hash suffix. */
const HASH_LENGTH = 8;

export class UrlShortener {
  private mapping = new Map<string, string>();

  /**
   * Shorten a URL by truncating query strings/fragments with a hash.
   * Short URLs are returned unchanged.
   */
  shorten(url: string): string {
    let base: string;
    let queryAndFragment: string;

    const queryStart = url.indexOf("?");
    const hashStart = url.indexOf("#");

    // Find where query/fragment begins
    const splitAt =
      queryStart >= 0 && hashStart >= 0
        ? Math.min(queryStart, hashStart)
        : queryStart >= 0
          ? queryStart
          : hashStart >= 0
            ? hashStart
            : -1;

    if (splitAt < 0) {
      // No query string or fragment
      return url;
    }

    base = url.slice(0, splitAt);
    queryAndFragment = url.slice(splitAt);

    if (queryAndFragment.length <= QUERY_THRESHOLD) {
      return url;
    }

    const hash = simpleHashHex(queryAndFragment);
    const shortened = `${base}${queryAndFragment.slice(0, KEEP_PREFIX)}...${hash}`;

    this.mapping.set(shortened, url);
    return shortened;
  }

  /**
   * Reverse a shortened URL back to its full form.
   * Returns undefined if the URL was not shortened by this instance.
   */
  expand(shortened: string): string | undefined {
    return this.mapping.get(shortened);
  }
}

/**
 * Simple non-cryptographic hash producing an 8-char hex string.
 * Not MD5 but sufficient for uniqueness within a session.
 */
function simpleHashHex(str: string): string {
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let i = 0; i < str.length; i++) {
    const ch = str.charCodeAt(i);
    h1 = Math.imul(h1 ^ ch, 2654435761);
    h2 = Math.imul(h2 ^ ch, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  const combined = (h2 >>> 0) * 0x100000000 + (h1 >>> 0);
  return combined.toString(16).padStart(HASH_LENGTH, "0").slice(0, HASH_LENGTH);
}
