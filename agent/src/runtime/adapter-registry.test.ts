import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  AdapterRegistry,
  type AdapterInstance,
  type AdapterManifest,
} from "./adapter-registry.js";
import type { AdapterCapabilities } from "./types.js";
import type { ScreenContext, PlannedAction } from "../types.js";

// ── Test Helpers ───────────────────────────────────────────────────────────

function createMockManifest(overrides: Partial<AdapterManifest> = {}): AdapterManifest {
  return {
    name: "test-adapter",
    displayName: "Test Adapter",
    platforms: ["macos"],
    supportedActionTypes: new Set(["click", "type"]),
    ...overrides,
  };
}

function createMockCapabilities(): AdapterCapabilities {
  return {
    readContext: vi.fn().mockResolvedValue({
      app: "TestApp", window: "Test", elements: [], timestamp_ms: Date.now(),
    } as ScreenContext),
    executeStructured: vi.fn().mockResolvedValue(true),
    resolveSemantic: vi.fn().mockResolvedValue(null),
    captureScreenshot: vi.fn().mockResolvedValue(Buffer.from([])),
  };
}

function createMockAdapter(overrides: Partial<AdapterManifest> = {}): AdapterInstance {
  const caps = createMockCapabilities();
  return {
    manifest: createMockManifest(overrides),
    state: "disconnected",
    connect: vi.fn(async function (this: AdapterInstance) { this.state = "connected"; }),
    disconnect: vi.fn(async function (this: AdapterInstance) { this.state = "disconnected"; }),
    probe: vi.fn().mockResolvedValue(true),
    buildCapabilities: vi.fn().mockReturnValue(caps),
    healthCheck: vi.fn().mockResolvedValue(true),
  };
}

// ── Tests ──────────────────────────────────────────────────────────────────

describe("AdapterRegistry", () => {
  let registry: AdapterRegistry;

  beforeEach(() => {
    registry = new AdapterRegistry();
  });

  describe("registration", () => {
    it("should register and retrieve an adapter", () => {
      const adapter = createMockAdapter({ name: "browser" });
      registry.register(adapter);

      expect(registry.get("browser")).toBe(adapter);
      expect(registry.list()).toEqual(["browser"]);
    });

    it("should throw on duplicate registration", () => {
      registry.register(createMockAdapter({ name: "browser" }));
      expect(() => registry.register(createMockAdapter({ name: "browser" })))
        .toThrow('Adapter "browser" is already registered');
    });

    it("should unregister and disconnect", async () => {
      const adapter = createMockAdapter({ name: "browser" });
      adapter.state = "connected";
      registry.register(adapter);

      await registry.unregister("browser");

      expect(registry.get("browser")).toBeUndefined();
      expect(adapter.disconnect).toHaveBeenCalled();
    });

    it("should no-op on unregistering unknown adapter", async () => {
      await registry.unregister("nonexistent"); // no throw
    });

    it("should list all adapters with metadata", () => {
      registry.register(createMockAdapter({ name: "browser", requiresApp: undefined }));
      registry.register(createMockAdapter({ name: "excel", requiresApp: "Microsoft Excel" }));

      const all = registry.listAll();
      expect(all).toHaveLength(2);
      expect(all[0]).toMatchObject({ name: "browser", state: "disconnected" });
      expect(all[1]).toMatchObject({ name: "excel", app: "Microsoft Excel" });
    });
  });

  describe("capability discovery", () => {
    it("should find adapters by action type", () => {
      registry.register(createMockAdapter({
        name: "browser",
        supportedActionTypes: new Set(["click", "type", "navigate"]),
      }));
      registry.register(createMockAdapter({
        name: "excel",
        supportedActionTypes: new Set(["click", "read_cell", "write_cell"]),
      }));

      const clickAdapters = registry.findByActionType("click");
      expect(clickAdapters).toHaveLength(2);

      const cellAdapters = registry.findByActionType("read_cell");
      expect(cellAdapters).toHaveLength(1);
      expect(cellAdapters[0].manifest.name).toBe("excel");

      const navAdapters = registry.findByActionType("navigate");
      expect(navAdapters).toHaveLength(1);
      expect(navAdapters[0].manifest.name).toBe("browser");
    });

    it("should find adapter by app name (requiresApp)", () => {
      registry.register(createMockAdapter({ name: "excel", requiresApp: "Microsoft Excel" }));
      registry.register(createMockAdapter({ name: "browser" }));

      const found = registry.findByApp("Microsoft Excel");
      expect(found?.manifest.name).toBe("excel");
    });

    it("should find adapter by app pattern", () => {
      registry.register(createMockAdapter({
        name: "browser",
        appPatterns: [/chrome/i, /brave/i, /arc/i],
      }));

      expect(registry.findByApp("Google Chrome")?.manifest.name).toBe("browser");
      expect(registry.findByApp("Brave Browser")?.manifest.name).toBe("browser");
      expect(registry.findByApp("Arc")?.manifest.name).toBe("browser");
      expect(registry.findByApp("Microsoft Excel")).toBeUndefined();
    });

    it("should find best adapter for an action", () => {
      const browser = createMockAdapter({
        name: "browser",
        supportedActionTypes: new Set(["click", "type", "navigate"]),
        appPatterns: [/chrome/i],
      });
      browser.state = "connected";
      const excel = createMockAdapter({
        name: "excel",
        supportedActionTypes: new Set(["click", "read_cell"]),
        requiresApp: "Microsoft Excel",
      });

      registry.register(browser);
      registry.register(excel);

      // Action with app context
      const found = registry.findForAction(
        { type: "click", target_id: "btn" } as PlannedAction,
        "Microsoft Excel",
      );
      expect(found?.manifest.name).toBe("excel");

      // Action without app context — prefer connected
      const found2 = registry.findForAction(
        { type: "click", target_id: "btn" } as PlannedAction,
      );
      expect(found2?.manifest.name).toBe("browser"); // connected takes priority
    });
  });

  describe("active adapter lifecycle", () => {
    it("should set active and return capabilities", async () => {
      const adapter = createMockAdapter({ name: "browser" });
      registry.register(adapter);

      const caps = await registry.setActive("browser");

      expect(registry.getActiveName()).toBe("browser");
      expect(registry.getActive()).toBe(adapter);
      expect(adapter.connect).toHaveBeenCalled();
      expect(adapter.state).toBe("connected");
      expect(caps).toBeDefined();
      expect(caps.readContext).toBeDefined();
    });

    it("should not reconnect if already connected", async () => {
      const adapter = createMockAdapter({ name: "browser" });
      adapter.state = "connected";
      registry.register(adapter);

      await registry.setActive("browser");
      expect(adapter.connect).not.toHaveBeenCalled();
    });

    it("should throw when setting active with unknown adapter", async () => {
      await expect(registry.setActive("nonexistent"))
        .rejects.toThrow('Adapter "nonexistent" is not registered');
    });

    it("should get active capabilities", async () => {
      const adapter = createMockAdapter({ name: "browser" });
      registry.register(adapter);
      await registry.setActive("browser");

      const caps = registry.getActiveCapabilities();
      expect(caps.readContext).toBeDefined();
    });

    it("should throw when no active adapter", () => {
      expect(() => registry.getActiveCapabilities())
        .toThrow("No active adapter");
    });
  });

  describe("cross-app hot-swap", () => {
    it("should swap active adapter based on app name", async () => {
      const browser = createMockAdapter({
        name: "browser",
        appPatterns: [/chrome/i],
      });
      const excel = createMockAdapter({
        name: "excel",
        requiresApp: "Microsoft Excel",
      });

      registry.register(browser);
      registry.register(excel);
      await registry.setActive("browser");

      // Simulate crossAppShift to Excel
      const caps = await registry.swapForApp("Microsoft Excel");
      expect(caps).not.toBeNull();
      expect(registry.getActiveName()).toBe("excel");
      expect(excel.connect).toHaveBeenCalled();
    });

    it("should return null if no adapter matches the app", async () => {
      registry.register(createMockAdapter({ name: "browser" }));
      await registry.setActive("browser");

      const caps = await registry.swapForApp("Unknown App");
      expect(caps).toBeNull();
      expect(registry.getActiveName()).toBe("browser"); // unchanged
    });

    it("should no-op when swapping to already-active adapter", async () => {
      const browser = createMockAdapter({
        name: "browser",
        appPatterns: [/chrome/i],
      });
      registry.register(browser);
      await registry.setActive("browser");

      const caps = await registry.swapForApp("Google Chrome");
      expect(caps).not.toBeNull();
      expect(registry.getActiveName()).toBe("browser");
    });
  });

  describe("dispose", () => {
    it("should disconnect all adapters and clear registry", async () => {
      const a1 = createMockAdapter({ name: "browser" });
      const a2 = createMockAdapter({ name: "excel" });
      a1.state = "connected";
      a2.state = "connected";

      registry.register(a1);
      registry.register(a2);
      await registry.setActive("browser");

      await registry.dispose();

      expect(registry.list()).toEqual([]);
      expect(registry.getActiveName()).toBeNull();
      expect(a1.disconnect).toHaveBeenCalled();
      expect(a2.disconnect).toHaveBeenCalled();
    });
  });

  describe("kernel compatibility", () => {
    it("should produce capabilities the kernel can consume", async () => {
      const adapter = createMockAdapter({ name: "browser" });
      registry.register(adapter);
      await registry.setActive("browser");

      const caps = registry.getActiveCapabilities();

      // These are the 4 required + 1 optional methods in AdapterCapabilities
      expect(typeof caps.readContext).toBe("function");
      expect(typeof caps.executeStructured).toBe("function");
      expect(typeof caps.resolveSemantic).toBe("function");
      expect(typeof caps.captureScreenshot).toBe("function");

      // readContext should return a ScreenContext
      const ctx = await caps.readContext();
      expect(ctx).toHaveProperty("app");
      expect(ctx).toHaveProperty("elements");
      expect(ctx).toHaveProperty("timestamp_ms");
    });
  });
});
