import {
  ResourceTemplate,
  type McpServer,
} from "@modelcontextprotocol/sdk/server/mcp.js";
import type {
  ListResourcesResult,
  ReadResourceResult,
  Resource,
} from "@modelcontextprotocol/sdk/types.js";
import type { Cel, WindowInfo } from "@cellar/agent";

export const CURRENT_SCREEN_RESOURCE_URI = "cellar://screen/current";
export const APP_SCREEN_RESOURCE_TEMPLATE = "cellar://screen/{app_name}";
export const SCREENSHOT_MIME_TYPE = "image/png";
export const SCREENSHOT_TRANSPORT_NOTE =
  "PNG screenshots are captured on demand and can be multiple MB over stdio.";

type ScreenResourceCel = Pick<
  Cel,
  "captureScreen" | "captureWindow" | "listCaptureWindows"
>;

function assertScreenResourceSupport(
  cel: Partial<ScreenResourceCel>,
): asserts cel is ScreenResourceCel {
  if (
    typeof cel.captureScreen !== "function" ||
    typeof cel.captureWindow !== "function" ||
    typeof cel.listCaptureWindows !== "function"
  ) {
    throw new Error(
      "Screen resources require cel-napi support for captureScreen, " +
        "captureWindow, and listCaptureWindows.",
    );
  }
}

function normalizeAppName(appName: string): string {
  return appName.trim().toLowerCase();
}

function isVisibleWindow(window: WindowInfo): boolean {
  return (
    Boolean(window.app_name?.trim()) &&
    !window.is_minimized &&
    window.width > 0 &&
    window.height > 0
  );
}

function getCaptureWindows(cel: ScreenResourceCel): WindowInfo[] {
  assertScreenResourceSupport(cel);

  try {
    return cel.listCaptureWindows();
  } catch {
    return [];
  }
}

function findWindowForApp(
  cel: ScreenResourceCel,
  appName: string,
): WindowInfo | undefined {
  const normalized = normalizeAppName(appName);
  return getCaptureWindows(cel)
    .filter(isVisibleWindow)
    .sort((a, b) => (a.title || a.app_name).localeCompare(b.title || b.app_name))
    .find((window) => normalizeAppName(window.app_name) === normalized);
}

export function screenResourceUriForApp(appName: string): string {
  return `cellar://screen/${encodeURIComponent(appName)}`;
}

export function appNameFromScreenResourceUri(uri: URL): string {
  if (uri.protocol !== "cellar:" || uri.hostname !== "screen") {
    throw new Error(`Unsupported screen resource URI: ${uri.toString()}`);
  }

  const encodedAppName = uri.pathname.replace(/^\/+/, "");
  if (!encodedAppName || encodedAppName === "current") {
    throw new Error(`Expected app screen resource URI: ${APP_SCREEN_RESOURCE_TEMPLATE}`);
  }
  return decodeURIComponent(encodedAppName);
}

export function listScreenResources(cel: ScreenResourceCel): Resource[] {
  assertScreenResourceSupport(cel);

  const resources: Resource[] = [
    {
      uri: CURRENT_SCREEN_RESOURCE_URI,
      name: "current-screen",
      title: "Current screen",
      description: "Latest PNG screenshot of the primary display.",
      mimeType: SCREENSHOT_MIME_TYPE,
      _meta: {
        capture_scope: "primary_display",
        freshness: "uncached",
        size_note: SCREENSHOT_TRANSPORT_NOTE,
      },
    },
  ];

  const seenApps = new Set<string>();
  const appResources = getCaptureWindows(cel)
    .filter(isVisibleWindow)
    .filter((window) => {
      const key = normalizeAppName(window.app_name);
      if (seenApps.has(key)) return false;
      seenApps.add(key);
      return true;
    })
    .sort((a, b) => a.app_name.localeCompare(b.app_name))
    .map<Resource>((window) => ({
      uri: screenResourceUriForApp(window.app_name),
      name: `screen-${normalizeAppName(window.app_name).replace(/[^a-z0-9_-]+/g, "-")}`,
      title: `${window.app_name} screen`,
      description: `Latest PNG screenshot for the visible ${window.app_name} window.`,
      mimeType: SCREENSHOT_MIME_TYPE,
      _meta: {
        capture_scope: "visible_app_window",
        freshness: "uncached",
        size_note: SCREENSHOT_TRANSPORT_NOTE,
        id_source: "listCaptureWindows",
        app_name: window.app_name,
        window_id: window.id,
        window_title: window.title,
        bounds: {
          x: window.x,
          y: window.y,
          width: window.width,
          height: window.height,
        },
      },
    }));

  resources.push(...appResources);
  return resources;
}

export function readCurrentScreenResource(
  cel: Pick<Cel, "captureScreen">,
  uri = CURRENT_SCREEN_RESOURCE_URI,
): ReadResourceResult {
  const buffer = cel.captureScreen();
  return {
    contents: [
      {
        uri,
        mimeType: SCREENSHOT_MIME_TYPE,
        blob: buffer.toString("base64"),
      },
    ],
  };
}

export function readAppScreenResource(
  cel: ScreenResourceCel,
  uri: URL,
): ReadResourceResult {
  assertScreenResourceSupport(cel);

  const appName = appNameFromScreenResourceUri(uri);
  const window = findWindowForApp(cel, appName);
  if (!window) {
    throw new Error(`No visible window found for app "${appName}"`);
  }

  const buffer = cel.captureWindow(window.id);
  return {
    contents: [
      {
        uri: uri.toString(),
        mimeType: SCREENSHOT_MIME_TYPE,
        blob: buffer.toString("base64"),
      },
    ],
  };
}

export function registerScreenResources(server: McpServer, cel: Cel): void {
  server.registerResource(
    "current-screen",
    CURRENT_SCREEN_RESOURCE_URI,
    {
      title: "Current screen",
      description: "Latest PNG screenshot of the primary display.",
      mimeType: SCREENSHOT_MIME_TYPE,
    },
    async (uri): Promise<ReadResourceResult> =>
      readCurrentScreenResource(cel, uri.toString()),
  );

  server.registerResource(
    "app-screen",
    new ResourceTemplate(APP_SCREEN_RESOURCE_TEMPLATE, {
      list: async (): Promise<ListResourcesResult> => ({
        resources: listScreenResources(cel).filter(
          (resource) => resource.uri !== CURRENT_SCREEN_RESOURCE_URI,
        ),
      }),
    }),
    {
      title: "App screen",
      description: "Latest PNG screenshot for a visible app window.",
      mimeType: SCREENSHOT_MIME_TYPE,
    },
    async (uri): Promise<ReadResourceResult> => readAppScreenResource(cel, uri),
  );
}
