import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_CEL_CDP_PORT,
  cleanupBlankCdpTabs,
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

  it("closes only about:blank and empty-url page targets", async () => {
    const closed: string[] = [];
    const fetchMock = vi.fn(async (url: string | URL) => {
      const href = String(url);
      if (href.endsWith("/json/list")) {
        return {
          ok: true,
          json: async () => ([
            {
              id: "real-1",
              type: "page",
              title: "Example",
              url: "https://example.com",
              webSocketDebuggerUrl: "ws://127.0.0.1:9333/devtools/page/real-1",
            },
            {
              id: "blank-1",
              type: "page",
              title: "",
              url: "about:blank",
              webSocketDebuggerUrl: "ws://127.0.0.1:9333/devtools/page/blank-1",
            },
            {
              id: "blank-2",
              type: "page",
              title: "",
              url: "",
              webSocketDebuggerUrl: "ws://127.0.0.1:9333/devtools/page/blank-2",
            },
            {
              id: "worker-1",
              type: "service_worker",
              url: "about:blank",
            },
          ]),
        } as any;
      }
      const closeMatch = href.match(/\/json\/close\/([^/]+)$/);
      if (closeMatch) {
        closed.push(closeMatch[1] ?? "");
        return { ok: true, json: async () => ({}) } as any;
      }
      return { ok: false, json: async () => ({}) } as any;
    });
    vi.stubGlobal("fetch", fetchMock as unknown as typeof fetch);

    const result = await cleanupBlankCdpTabs(9333);

    expect(result.closed).toBe(2);
    expect(closed).toEqual(["blank-1", "blank-2"]);
    expect(closed).not.toContain("real-1");
    expect(closed).not.toContain("worker-1");
  });

  it("returns zero closed tabs when /json/list is unreachable", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => {
      throw new Error("connection refused");
    }) as unknown as typeof fetch);

    const result = await cleanupBlankCdpTabs(9333);
    expect(result.closed).toBe(0);
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
