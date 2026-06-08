import { describe, expect, it } from "vitest";
import { celRememberSchema } from "../src/tools/cel-remember.js";
import { celRecallSchema } from "../src/tools/cel-recall.js";
import { celForgetSchema } from "../src/tools/cel-forget.js";

describe("cel_remember schema", () => {
  it("accepts the minimal payload (content only)", () => {
    const parsed = celRememberSchema.parse({ content: "hello" });
    expect(parsed.kind).toBe("chat");
    expect(parsed.shareable).toBe(false);
    expect(parsed.pinned).toBe(false);
  });

  it("rejects empty content", () => {
    const r = celRememberSchema.safeParse({ content: "" });
    expect(r.success).toBe(false);
  });

  it("rejects unknown kinds", () => {
    const r = celRememberSchema.safeParse({ content: "x", kind: "haunted" });
    expect(r.success).toBe(false);
  });

  it("clamps importance to [0,1] (rejects out-of-range)", () => {
    expect(celRememberSchema.safeParse({ content: "x", importance: -0.1 }).success).toBe(false);
    expect(celRememberSchema.safeParse({ content: "x", importance: 1.1 }).success).toBe(false);
    expect(celRememberSchema.parse({ content: "x", importance: 0.7 }).importance).toBe(0.7);
  });

  it("honors shareable and tags", () => {
    const p = celRememberSchema.parse({
      content: "user prefers MM-DD-YYYY",
      shareable: true,
      tags: ["pref", "dates"],
    });
    expect(p.shareable).toBe(true);
    expect(p.tags).toEqual(["pref", "dates"]);
  });
});

describe("cel_recall schema", () => {
  it("accepts the minimal payload (query only)", () => {
    const parsed = celRecallSchema.parse({ query: "q4 report" });
    expect(parsed.limit).toBe(8);
    expect(parsed.scope).toBe("own");
  });

  it("rejects empty query", () => {
    const r = celRecallSchema.safeParse({ query: "" });
    expect(r.success).toBe(false);
  });

  it("rejects unknown scope", () => {
    expect(celRecallSchema.safeParse({ query: "x", scope: "everyone" }).success).toBe(false);
  });

  it("caps limit at 50", () => {
    expect(celRecallSchema.safeParse({ query: "x", limit: 100 }).success).toBe(false);
    expect(celRecallSchema.parse({ query: "x", limit: 30 }).limit).toBe(30);
  });

  it("accepts an array kind filter", () => {
    const p = celRecallSchema.parse({
      query: "auth",
      kind: ["correction", "observation"],
    });
    expect(p.kind).toEqual(["correction", "observation"]);
  });

  it("accepts own_plus_shared scope", () => {
    expect(celRecallSchema.parse({ query: "x", scope: "own_plus_shared" }).scope).toBe(
      "own_plus_shared",
    );
  });
});

describe("cel_forget schema", () => {
  it("accepts a chunk_ids payload", () => {
    const parsed = celForgetSchema.parse({ chunk_ids: ["a", "b"] });
    expect(parsed.chunk_ids).toEqual(["a", "b"]);
  });

  it("accepts a predicate payload", () => {
    const parsed = celForgetSchema.parse({
      predicate: { kind: ["chat"], tag: "draft" },
    });
    expect(parsed.predicate?.kind).toEqual(["chat"]);
  });

  it("rejects when neither chunk_ids nor predicate is supplied", () => {
    const r = celForgetSchema.safeParse({});
    expect(r.success).toBe(false);
  });

  it("rejects when both chunk_ids and predicate are supplied", () => {
    const r = celForgetSchema.safeParse({
      chunk_ids: ["a"],
      predicate: { tag: "draft" },
    });
    expect(r.success).toBe(false);
  });

  it("rejects when chunk_ids contains an empty string", () => {
    const r = celForgetSchema.safeParse({ chunk_ids: [""] });
    expect(r.success).toBe(false);
  });

  it("rejects an empty predicate (no fields set)", () => {
    const r = celForgetSchema.safeParse({ predicate: {} });
    expect(r.success).toBe(false);
  });
});
