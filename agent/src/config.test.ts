import { describe, it, expect } from "vitest";
import { resolvePath } from "./config.js";

describe("config", () => {
  it("should export celConfig with defaults", async () => {
    // Dynamic import to avoid module-level side effects
    const { celConfig } = await import("./config.js");
    expect(celConfig).toBeDefined();
    expect(celConfig.logLevel).toBe("info");
    expect(celConfig.dbPath).toContain("cel-store.db");
    expect(celConfig.workflowsDir).toContain("workflows");
  });

  it("resolvePath should replace ~ with home dir", () => {
    const result = resolvePath("~/test/path");
    expect(result).not.toContain("~");
    expect(result).toContain("test/path");
  });

  it("resolvePath should not modify absolute paths", () => {
    const result = resolvePath("/absolute/path");
    expect(result).toBe("/absolute/path");
  });
});
