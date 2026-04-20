import { describe, it, expect, beforeEach, afterEach } from "vitest";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";
import {
  saveWorkflow,
  loadWorkflow,
  listWorkflows,
  deleteWorkflow,
  exportWorkflow,
  importWorkflow,
} from "./workflow-io.js";
import type { Workflow } from "./types.js";

function makeWorkflow(name: string): Workflow {
  return {
    name,
    description: `Workflow: ${name}`,
    app: "test-app",
    version: "1.0.0",
    steps: [
      {
        id: "s1",
        description: "Click OK",
        action: { type: "click", target: "ok-btn" },
      },
      {
        id: "s2",
        description: "Type name",
        action: { type: "type", target: "name-input", text: "John" },
      },
    ],
    created_at: "2024-01-01T00:00:00Z",
    updated_at: "2024-01-01T00:00:00Z",
  };
}

describe("workflow-io", () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cellar-test-"));
  });

  afterEach(() => {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  describe("saveWorkflow", () => {
    it("should save workflow as JSON", () => {
      const wf = makeWorkflow("save-test");
      const filePath = saveWorkflow(wf, tmpDir);
      expect(fs.existsSync(filePath)).toBe(true);
      const content = JSON.parse(fs.readFileSync(filePath, "utf-8"));
      expect(content.name).toBe("save-test");
      expect(content.steps.length).toBe(2);
    });

    it("should create directory if it doesn't exist", () => {
      const nested = path.join(tmpDir, "nested", "deep");
      const wf = makeWorkflow("nested-test");
      saveWorkflow(wf, nested);
      expect(fs.existsSync(path.join(nested, "nested-test.json"))).toBe(true);
    });
  });

  describe("loadWorkflow", () => {
    it("should load a saved workflow", () => {
      const wf = makeWorkflow("load-test");
      const filePath = saveWorkflow(wf, tmpDir);
      const loaded = loadWorkflow(filePath);
      expect(loaded.name).toBe("load-test");
      expect(loaded.app).toBe("test-app");
      expect(loaded.steps.length).toBe(2);
    });

    it("should throw on invalid path", () => {
      expect(() => loadWorkflow("/nonexistent/path.json")).toThrow();
    });
  });

  describe("listWorkflows", () => {
    it("should list all workflows in directory", () => {
      saveWorkflow(makeWorkflow("wf-a"), tmpDir);
      saveWorkflow(makeWorkflow("wf-b"), tmpDir);
      saveWorkflow(makeWorkflow("wf-c"), tmpDir);
      const workflows = listWorkflows(tmpDir);
      expect(workflows.length).toBe(3);
      const names = workflows.map((w) => w.name).sort();
      expect(names).toEqual(["wf-a", "wf-b", "wf-c"]);
    });

    it("should return empty array for empty directory", () => {
      const workflows = listWorkflows(tmpDir);
      expect(workflows.length).toBe(0);
    });
  });

  describe("deleteWorkflow", () => {
    it("should delete an existing workflow", () => {
      saveWorkflow(makeWorkflow("delete-me"), tmpDir);
      expect(deleteWorkflow("delete-me", tmpDir)).toBe(true);
      expect(listWorkflows(tmpDir).length).toBe(0);
    });

    it("should return false for non-existent workflow", () => {
      expect(deleteWorkflow("no-such-wf", tmpDir)).toBe(false);
    });
  });

  describe("exportWorkflow", () => {
    it("should export to .dilipod format", () => {
      const wf = makeWorkflow("export-test");
      const outputPath = path.join(tmpDir, "export-test.dilipod");
      exportWorkflow(wf, outputPath);

      const data = JSON.parse(fs.readFileSync(outputPath, "utf-8"));
      expect(data.format).toBe("dilipod-workflow");
      expect(data.version).toBe("1.0");
      expect(data.exported_at).toBeDefined();
      expect(data.workflow.name).toBe("export-test");
    });
  });

  describe("importWorkflow", () => {
    it("should import from .dilipod format", () => {
      const wf = makeWorkflow("import-test");
      const outputPath = path.join(tmpDir, "import-test.dilipod");
      exportWorkflow(wf, outputPath);

      const imported = importWorkflow(outputPath);
      expect(imported.name).toBe("import-test");
      expect(imported.steps.length).toBe(2);
    });

    it("should reject invalid format", () => {
      const badPath = path.join(tmpDir, "bad.dilipod");
      fs.writeFileSync(
        badPath,
        JSON.stringify({ format: "wrong-format", workflow: {} })
      );
      expect(() => importWorkflow(badPath)).toThrow("Invalid .dilipod file");
    });

    it("should handle round-trip export/import", () => {
      const original = makeWorkflow("roundtrip");
      const dilipodPath = path.join(tmpDir, "roundtrip.dilipod");
      exportWorkflow(original, dilipodPath);
      const imported = importWorkflow(dilipodPath);
      expect(imported.name).toBe(original.name);
      expect(imported.steps.length).toBe(original.steps.length);
      expect(imported.app).toBe(original.app);
    });
  });
});
