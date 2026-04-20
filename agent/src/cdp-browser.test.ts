import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_CEL_CDP_PORT,
  discoverCanonicalCdpTargets,
  getPreferredCelCdpPort,
  isCelOwnedUserDataDir,
  selectPreferredCdpTarget,
} from "./cdp-browser.js";

describe("cdp-browser", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("defaults the preferred CEL CDP port to 9333", () => {
    expect(getPreferredCelCdpPort(undefined)).toBe(DEFAULT_CEL_CDP_PORT);
    expect(getPreferredCelCdpPort("not-a-port")).toBe(DEFAULT_CEL_CDP_PORT);
  });

  it("recognizes CEL-owned browser profiles", () => {
    expect(isCelOwnedUserDataDir("/Users/test/.cellar/cdp-profiles/google-chrome")).toBe(true);
    expect(isCelOwnedUserDataDir("/Users/test/Library/Application Support/Google/Chrome")).toBe(false);
  });

  it("prefers the dedicated CEL port when multiple browser instances share the same app name", () => {
    const target = selectPreferredCdpTarget(
      [
        {
          app_name: "Google Chrome",
          port: 9222,
          ws_url: "ws://127.0.0.1:9222/devtools/page/legacy",
        },
        {
          app_name: "Google Chrome",
          port: 9333,
          ws_url: "ws://127.0.0.1:9333/devtools/page/cel",
        },
      ],
      "Google Chrome",
    );

    expect(target?.port).toBe(9333);
  });

  it("falls back to fuzzy app matching when the focused browser name is broader than the target name", () => {
    const target = selectPreferredCdpTarget(
      [
        {
          app_name: "Arc",
          port: 9222,
          ws_url: "ws://127.0.0.1:9222/devtools/page/arc",
        },
        {
          app_name: "Google Chrome",
          port: 9444,
          ws_url: "ws://127.0.0.1:9444/devtools/page/chrome",
        },
      ],
      "Arc Browser",
      9555,
    );

    expect(target?.app_name).toBe("Arc");
  });

  it("merges raw and HTTP target discovery into one canonical target list", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({
      ok: true,
      json: async () => ([{
        type: "page",
        title: "Example Domain",
        url: "https://example.com",
        webSocketDebuggerUrl: "ws://127.0.0.1:9333/devtools/page/example",
      }]),
    })) as unknown as typeof fetch);

    const targets = await discoverCanonicalCdpTargets({
      discoverCdpTargets: () => [{
        app_name: "Google Chrome",
        pid: 4242,
        port: 9333,
        ws_url: "ws://127.0.0.1:9333/devtools/page/example",
      }],
    });

    expect(targets).toHaveLength(1);
    expect(targets[0]?.source).toBe("merged");
    expect(targets[0]?.pid).toBe(4242);
    expect(targets[0]?.title).toBe("Example Domain");
  });

  it("falls back to HTTP discovery when the native target list is empty", async () => {
    vi.stubGlobal("fetch", vi.fn(async (url: string | URL) => {
      const href = String(url);
      if (href.endsWith("/json/version")) {
        return {
          ok: true,
          json: async () => ({ Browser: "Google Chrome/136.0" }),
        } as any;
      }
      return {
        ok: true,
        json: async () => ([{
          type: "page",
          title: "Example Domain",
          url: "https://example.com",
          webSocketDebuggerUrl: "ws://127.0.0.1:9333/devtools/page/example",
        }]),
      } as any;
    }) as unknown as typeof fetch);

    const targets = await discoverCanonicalCdpTargets({
      discoverCdpTargets: () => [],
    });

    expect(targets).toHaveLength(1);
    expect(targets[0]?.source).toBe("http");
    expect(targets[0]?.app_name).toBe("Google Chrome");
  });
});
