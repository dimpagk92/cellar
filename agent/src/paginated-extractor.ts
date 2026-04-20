/**
 * Paginated extraction with deduplication — chunks large markdown content
 * and formats extraction prompts with dedup context.
 */

export interface ExtractionConfig {
  /** JSON schema for structured extraction. */
  schema?: Record<string, unknown>;
  /** Max chars per chunk. Default: 100000. */
  maxChunkChars?: number;
  /** Already collected item identifiers for dedup. */
  alreadyCollected?: string[];
}

export interface PaginatedExtractionResult {
  data: unknown;
  nextStartChar?: number;
  isComplete: boolean;
}

const DEFAULT_MAX_CHUNK_CHARS = 100_000;

/**
 * Chunk markdown content by structure (headers, tables, code blocks).
 *
 * Splits at structural boundaries (## headers, blank lines before code fences,
 * table boundaries) to avoid breaking semantic units. Falls back to paragraph
 * boundaries, then hard character splits.
 */
export function chunkMarkdown(content: string, maxChars: number): string[] {
  if (content.length <= maxChars) {
    return [content];
  }

  const chunks: string[] = [];
  let remaining = content;

  while (remaining.length > 0) {
    if (remaining.length <= maxChars) {
      chunks.push(remaining);
      break;
    }

    // Try to find a structural boundary within maxChars
    const slice = remaining.slice(0, maxChars);
    let splitIndex = findStructuralBoundary(slice, maxChars);

    if (splitIndex <= 0) {
      // Fallback: split at last paragraph boundary
      splitIndex = findParagraphBoundary(slice);
    }

    if (splitIndex <= 0) {
      // Hard split at maxChars
      splitIndex = maxChars;
    }

    chunks.push(remaining.slice(0, splitIndex).trimEnd());
    remaining = remaining.slice(splitIndex).trimStart();
  }

  return chunks;
}

/**
 * Find the last structural boundary (header, code fence, table separator)
 * within the given text, searching backwards from the end.
 */
function findStructuralBoundary(text: string, _maxChars: number): number {
  // Look for the last markdown header (## ...) preceded by a blank line
  const headerPattern = /\n(?=#{1,6}\s)/g;
  let lastMatch = -1;
  let match: RegExpExecArray | null;

  // Find the last header boundary in the last 30% of the text
  const searchStart = Math.floor(text.length * 0.7);
  headerPattern.lastIndex = searchStart;

  while ((match = headerPattern.exec(text)) !== null) {
    lastMatch = match.index;
  }

  if (lastMatch > 0) return lastMatch;

  // Look for code fence boundary (```)
  const fencePattern = /\n```\s*\n/g;
  fencePattern.lastIndex = searchStart;
  while ((match = fencePattern.exec(text)) !== null) {
    // Split after the closing fence
    lastMatch = match.index + match[0].length;
  }

  if (lastMatch > 0) return lastMatch;

  // Look for table boundary (empty line after table)
  const tableEndPattern = /\n\s*\n(?!\|)/g;
  tableEndPattern.lastIndex = searchStart;
  while ((match = tableEndPattern.exec(text)) !== null) {
    lastMatch = match.index;
  }

  return lastMatch;
}

/**
 * Find the last paragraph boundary (double newline) in the text.
 */
function findParagraphBoundary(text: string): number {
  const lastDoubleNewline = text.lastIndexOf("\n\n");
  if (lastDoubleNewline > 0) return lastDoubleNewline;
  const lastNewline = text.lastIndexOf("\n");
  return lastNewline > 0 ? lastNewline : -1;
}

/**
 * Format an extraction prompt with dedup context and optional schema.
 */
export function formatExtractionPrompt(
  content: string,
  config: ExtractionConfig,
): string {
  const maxChars = config.maxChunkChars ?? DEFAULT_MAX_CHUNK_CHARS;
  const parts: string[] = [];

  parts.push("Extract structured data from the following content.");

  if (config.schema) {
    parts.push("");
    parts.push("## Output Schema");
    parts.push("```json");
    parts.push(JSON.stringify(config.schema, null, 2));
    parts.push("```");
  }

  if (config.alreadyCollected && config.alreadyCollected.length > 0) {
    parts.push("");
    parts.push("## Already Collected (skip duplicates)");
    parts.push(
      config.alreadyCollected
        .map((id) => `- ${id}`)
        .join("\n"),
    );
  }

  parts.push("");
  parts.push("## Content");

  // Truncate if content exceeds max chunk size
  if (content.length > maxChars) {
    parts.push(content.slice(0, maxChars));
    parts.push("");
    parts.push(`[Content truncated at ${maxChars} chars. ${content.length - maxChars} chars remaining.]`);
  } else {
    parts.push(content);
  }

  return parts.join("\n");
}
