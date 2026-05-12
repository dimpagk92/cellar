import { describe, expect, it, vi } from "vitest";
import {
  CURRENT_SCREEN_RESOURCE_URI,
  appNameFromScreenResourceUri,
  listScreenResources,
  readAppScreenResource,
  readCurrentScreenResource,
  screenResourceUriForApp,
} from "../src/resources.js";
import { screenResourcesEnabled } from "../src/server.js";
import type { WindowInfo } from "@cellar/agent";

function window(overrides: Partial<WindowInfo>): WindowInfo {
  return {
    id: 1,
    app_name: "Finder",
    title: "Documents",
    x: 0,
    y: 0,
    width: 800,
    height: 600,
    is_minimized: false,
    ...overrides,
  };
}

describe("screen resources", () => {
  it("lists the current screen plus one visible resource per app", () => {
    const cel = {
      captureScreen: vi.fn(),
      captureWindow: vi.fn(),
      listCaptureWindows: vi.fn(() => [
        window({ id: 10, app_name: "Finder", title: "Downloads" }),
        window({ id: 11, app_name: "Finder", title: "Documents" }),
        window({ id: 12, app_name: "Notes", title: "Todo" }),
        window({ id: 13, app_name: "Hidden", is_minimized: true }),
        window({ id: 14, app_name: "", title: "Untitled" }),
      ]),
    };

    const resources = listScreenResources(cel);

    expect(resources.map((resource) => resource.uri)).toEqual([
      CURRENT_SCREEN_RESOURCE_URI,
      "cellar://screen/Finder",
      "cellar://screen/Notes",
    ]);
    expect(resources[1]?._meta).toMatchObject({
      app_name: "Finder",
      window_id: 10,
      window_title: "Downloads",
      capture_scope: "visible_app_window",
      freshness: "uncached",
      id_source: "listCaptureWindows",
    });
  });

  it("encodes app names as URI-safe path segments", () => {
    const uri = screenResourceUriForApp("Foo/Bar Beta");

    expect(uri).toBe("cellar://screen/Foo%2FBar%20Beta");
    expect(appNameFromScreenResourceUri(new URL(uri))).toBe("Foo/Bar Beta");
  });

  it("reads the current screen as a PNG blob resource", () => {
    const cel = {
      captureScreen: vi.fn(() => Buffer.from("screen")),
    };

    expect(readCurrentScreenResource(cel)).toEqual({
      contents: [
        {
          uri: CURRENT_SCREEN_RESOURCE_URI,
          mimeType: "image/png",
          blob: Buffer.from("screen").toString("base64"),
        },
      ],
    });
  });

  it("captures the matching app window when reading an app resource", () => {
    const cel = {
      captureScreen: vi.fn(() => Buffer.from("full-screen")),
      captureWindow: vi.fn(() => Buffer.from("window-shot")),
      listCaptureWindows: vi.fn(() => [
        window({ id: 21, app_name: "Calendar", title: "May" }),
        window({ id: 22, app_name: "Notes", title: "Todo" }),
      ]),
    };

    const result = readAppScreenResource(cel, new URL("cellar://screen/Notes"));

    expect(cel.captureWindow).toHaveBeenCalledWith(22);
    expect(cel.captureScreen).not.toHaveBeenCalled();
    expect(result.contents[0]).toMatchObject({
      uri: "cellar://screen/Notes",
      mimeType: "image/png",
      blob: Buffer.from("window-shot").toString("base64"),
    });
  });

  it("rejects app resources when no visible matching window exists", () => {
    const cel = {
      captureScreen: vi.fn(() => Buffer.from("screen")),
      captureWindow: vi.fn(() => Buffer.from("window-shot")),
      listCaptureWindows: vi.fn(() => [window({ id: 21, app_name: "Calendar" })]),
    };

    expect(() => readAppScreenResource(cel, new URL("cellar://screen/Notes"))).toThrow(
      'No visible window found for app "Notes"',
    );
  });

  it("requires capture-backend window ids for app resources", () => {
    const cel = {
      captureScreen: vi.fn(() => Buffer.from("full-screen")),
      listCaptureWindows: vi.fn(() => [window({ id: 22, app_name: "Notes" })]),
    };

    expect(() =>
      readAppScreenResource(cel as never, new URL("cellar://screen/Notes")),
    ).toThrow("Screen resources require cel-napi support");
    expect(cel.captureScreen).not.toHaveBeenCalled();
  });

  it("gates resource registration behind an explicit environment opt-in", () => {
    expect(screenResourcesEnabled({})).toBe(false);
    expect(screenResourcesEnabled({ CELLAR_ENABLE_SCREEN_RESOURCES: "0" })).toBe(false);
    expect(screenResourcesEnabled({ CELLAR_ENABLE_SCREEN_RESOURCES: "1" })).toBe(true);
    expect(screenResourcesEnabled({ CELLAR_ENABLE_SCREEN_RESOURCES: "true" })).toBe(true);
    expect(screenResourcesEnabled({ CELLAR_ENABLE_SCREEN_RESOURCES: "yes" })).toBe(true);
  });
});
