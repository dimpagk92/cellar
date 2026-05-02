/**
 * Browser Adapter — DOM-based context provider for web applications.
 *
 * Hybrid architecture (inspired by browser-use's CDP migration):
 * - Playwright for lifecycle management (launch, connect, cleanup)
 * - Raw CDP for the hot path (evaluate, network, screenshots)
 * - Playwright Locators as fallback for complex interactions
 * - Multi-watchdog system (popup, download, security, storage)
 *
 * Supports direct CDP mode (no Playwright at all) via cdpUrl config.
 *
 * Key advantages over browser-use:
 * - Shadow DOM traversal (they fail at shadow boundaries)
 * - Incremental updates via MutationObserver (~50ms vs their 5-30s)
 * - Network event capture via CDP Network domain
 * - Prompt injection sanitization (they inject raw DOM into LLM)
 * - Optional multi-browser support via Playwright (they're Chromium-only)
 * - 4 specialized watchdogs (popup, download, security, storage)
 *
 * License: MIT
 */

import type { ContextElement, NetworkEvent, PlannedAction, ScreenContext } from "@cellar/agent";
import type { Cel } from "@cellar/agent";
import { CdpClient, type CdpClientConfig } from "./cdp-client.js";
import { extractDOMAllFrames, extractDOMLightweight, CLOSED_SHADOW_PATCH } from "./dom-extractor.js";
import { mapElements } from "./element-mapper.js";
import { sanitizeElements } from "./sanitizer.js";
import { MutationTracker } from "./mutation-tracker.js";
import { ActionHandler } from "./action-handler.js";
import { detectOverlay, dismissOverlay, type BlockingOverlay, type DismissResult } from "./overlay-detector.js";
import { NetworkTap } from "./network-tap.js";
import { createHybridSnapshot, type HybridSnapshot } from "./hybrid-snapshot.js";
import { UrlMap } from "./url-map.js";
import { PopupWatchdog, type PopupWatchdogConfig, type PopupEvent } from "./popup-watchdog.js";
import { DownloadWatchdog, type DownloadWatchdogConfig, type DownloadEvent } from "./download-watchdog.js";
import { SecurityWatchdog, type SecurityWatchdogConfig, type SecurityEvent } from "./security-watchdog.js";
import { StorageWatchdog, type StorageWatchdogConfig, type StorageState } from "./storage-watchdog.js";

const shouldDebugBrowserAdapter =
  process.env.CEL_LOG_LEVEL === "debug" || process.env.CEL_DEBUG_BROWSER === "1";

function debugBrowser(message: string): void {
  if (shouldDebugBrowserAdapter) {
    console.error(message);
  }
}

/** Stealth script — injected before page scripts to evade bot detection. */
const STEALTH_SCRIPT = `
  // ─── Core: webdriver flag ──────────────────────────────────────────
  Object.defineProperty(navigator, 'webdriver', { get: () => undefined });
  try { delete navigator.__proto__.webdriver; } catch {}

  // ─── Navigator properties ──────────────────────────────────────────
  Object.defineProperty(navigator, 'languages', { get: () => ['en-US', 'en'] });
  // Match platform to the actual user agent (avoid mismatch detection)
  Object.defineProperty(navigator, 'platform', {
    get: () => navigator.userAgent.includes('Macintosh') ? 'MacIntel' : 'Linux x86_64'
  });
  Object.defineProperty(navigator, 'hardwareConcurrency', { get: () => 8 });
  Object.defineProperty(navigator, 'deviceMemory', { get: () => 8 });
  Object.defineProperty(navigator, 'maxTouchPoints', { get: () => 0 });
  Object.defineProperty(navigator, 'vendor', { get: () => 'Google Inc.' });

  // ─── Plugins (headless has 0 — dead giveaway) ─────────────────────
  Object.defineProperty(navigator, 'plugins', {
    get: () => {
      const plugins = [
        { name: 'Chrome PDF Plugin', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'Chrome PDF Viewer', filename: 'mhjfbmdgcfjbbpaeojofohoefgiehjai', description: 'Portable Document Format' },
        { name: 'Native Client', filename: 'internal-nacl-plugin', description: '' },
      ];
      const arr = Object.create(PluginArray.prototype);
      plugins.forEach((p, i) => {
        const plugin = Object.create(Plugin.prototype);
        Object.defineProperties(plugin, {
          name: { get: () => p.name }, filename: { get: () => p.filename },
          description: { get: () => p.description }, length: { get: () => 1 },
          0: { get: () => ({ type: 'application/pdf', suffixes: 'pdf', description: 'PDF' }) },
        });
        Object.defineProperty(arr, i, { get: () => plugin });
        Object.defineProperty(arr, p.name, { get: () => plugin });
      });
      Object.defineProperty(arr, 'length', { get: () => plugins.length });
      arr[Symbol.iterator] = function*() { for (let i = 0; i < plugins.length; i++) yield this[i]; };
      return arr;
    }
  });
  Object.defineProperty(navigator, 'mimeTypes', {
    get: () => {
      const arr = Object.create(MimeTypeArray.prototype);
      Object.defineProperty(arr, 'length', { get: () => 2 });
      return arr;
    }
  });

  // ─── Chrome runtime (missing in headless) ─────────────────────────
  if (!window.chrome) window.chrome = {};
  if (!window.chrome.runtime) {
    window.chrome.runtime = {
      connect: () => {}, sendMessage: () => {}, id: undefined,
      onMessage: { addListener: () => {}, removeListener: () => {} },
      onConnect: { addListener: () => {}, removeListener: () => {} },
    };
  }
  if (!window.chrome.loadTimes) window.chrome.loadTimes = () => ({});
  if (!window.chrome.csi) window.chrome.csi = () => ({});
  window.chrome.app = { isInstalled: false, InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' }, RunningState: { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' } };

  // ─── Permissions API ──────────────────────────────────────────────
  const origQuery = window.navigator.permissions?.query?.bind(window.navigator.permissions);
  if (origQuery) {
    Object.defineProperty(navigator, 'permissions', {
      get: () => ({
        query: (p) => p.name === 'notifications'
          ? Promise.resolve({ state: Notification.permission })
          : origQuery(p).catch(() => Promise.resolve({ state: 'prompt' }))
      })
    });
  }

  // ─── Screen dimensions ────────────────────────────────────────────
  Object.defineProperty(screen, 'width', { get: () => 1920 });
  Object.defineProperty(screen, 'height', { get: () => 1080 });
  Object.defineProperty(screen, 'availWidth', { get: () => 1920 });
  Object.defineProperty(screen, 'availHeight', { get: () => 1080 });
  Object.defineProperty(screen, 'colorDepth', { get: () => 24 });
  Object.defineProperty(screen, 'pixelDepth', { get: () => 24 });
  Object.defineProperty(window, 'outerWidth', { get: () => 1920 });
  Object.defineProperty(window, 'outerHeight', { get: () => 1080 });
  Object.defineProperty(window, 'devicePixelRatio', { value: 1, writable: true });

  // ─── WebGL renderer (Cloudflare checks this) ──────────────────────
  const origGetParam = WebGLRenderingContext.prototype.getParameter;
  WebGLRenderingContext.prototype.getParameter = function(param) {
    if (param === 37445) return 'Google Inc. (Intel)';
    if (param === 37446) return 'ANGLE (Intel, Mesa Intel(R) UHD Graphics 630 (CFL GT2), OpenGL 4.6)';
    return origGetParam.call(this, param);
  };
  try {
    const origGetParam2 = WebGL2RenderingContext.prototype.getParameter;
    WebGL2RenderingContext.prototype.getParameter = function(param) {
      if (param === 37445) return 'Google Inc. (Intel)';
      if (param === 37446) return 'ANGLE (Intel, Mesa Intel(R) UHD Graphics 630 (CFL GT2), OpenGL 4.6)';
      return origGetParam2.call(this, param);
    };
  } catch {}

  // ─── iframe contentWindow (Cloudflare uses cross-origin iframe checks)
  try {
    const origContentWindow = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'contentWindow');
    if (origContentWindow) {
      Object.defineProperty(HTMLIFrameElement.prototype, 'contentWindow', {
        get: function() {
          const win = origContentWindow.get.call(this);
          if (!win) return win;
          // Patch the iframe's navigator.webdriver too
          try { Object.defineProperty(win.navigator, 'webdriver', { get: () => undefined }); } catch {}
          return win;
        }
      });
    }
  } catch {}

  // ─── Notification (headless returns 'denied' which is suspicious) ──
  try {
    Object.defineProperty(Notification, 'permission', { get: () => 'default' });
  } catch {}

  // ─── Prevent Playwright automation detection via stack traces ──────
  const origError = Error;
  const errHandler = {
    construct(target, args) {
      const err = new target(...args);
      if (err.stack) {
        err.stack = err.stack.replace(/\\n.*playwright.*$/gm, '')
                            .replace(/\\n.*__playwright.*$/gm, '')
                            .replace(/\\n.*pptr:.*$/gm, '');
      }
      return err;
    }
  };
  window.Error = new Proxy(origError, errHandler);

  // ─── Connection rtt (0 in headless) ───────────────────────────────
  try {
    if (navigator.connection) {
      Object.defineProperty(navigator.connection, 'rtt', { get: () => 50 });
    }
  } catch {}
`;

/** Descriptor for an observable action on a page element. */
export interface ObservedAction {
  elementId: string;
  label: string;
  elementType: string;
  availableActions: string[];
  bounds?: { x: number; y: number; width: number; height: number };
}

/** Derive available actions from an element's type and state. */
function deriveAvailableActions(
  elementType: string,
  state?: { enabled?: boolean; visible?: boolean; focused?: boolean; selected?: boolean },
): string[] {
  if (state?.visible === false) return [];
  if (state?.enabled === false) return ["scroll_to"];

  const actions: string[] = [];

  switch (elementType) {
    case "a":
    case "link":
      actions.push("click", "hover", "focus");
      break;
    case "button":
      actions.push("click", "hover", "focus");
      break;
    case "input":
    case "textbox":
    case "searchbox":
      actions.push("click", "type", "clear", "focus");
      break;
    case "textarea":
      actions.push("click", "type", "clear", "focus");
      break;
    case "select":
    case "combobox":
    case "listbox":
      actions.push("click", "select_option", "focus");
      break;
    case "checkbox":
    case "switch":
      actions.push("click", "toggle", "focus");
      break;
    case "radio":
      actions.push("click", "focus");
      break;
    case "slider":
    case "spinbutton":
      actions.push("click", "set_value", "focus");
      break;
    case "tab":
      actions.push("click", "focus");
      break;
    case "menuitem":
    case "menuitemcheckbox":
    case "menuitemradio":
      actions.push("click", "hover");
      break;
    case "option":
    case "treeitem":
      actions.push("click", "select");
      break;
    case "details":
    case "summary":
      actions.push("click", "toggle");
      break;
    case "img":
    case "video":
    case "audio":
    case "canvas":
      actions.push("click");
      break;
    default:
      // For any element that made it through extraction, click is usually possible
      actions.push("click");
      break;
  }

  actions.push("scroll_to");
  return actions;
}

export interface BrowserAdapterConfig {
  /** Browser engine to use. */
  browser: "chromium" | "firefox" | "webkit";
  /** Whether to use CDP (Chrome DevTools Protocol). */
  useCdp: boolean;
  /** WebSocket endpoint to connect via Playwright's connectOverCDP. */
  wsEndpoint?: string;
  /** Raw CDP WebSocket URL — bypasses Playwright entirely. */
  cdpUrl?: string;
  /** Launch in headless mode (default: false — visible browser). */
  headless?: boolean;
  /** Viewport dimensions. */
  viewport?: { width: number; height: number };
  /** Enable prompt injection sanitization (default: true). */
  sanitize?: boolean;
  /** Enable MutationObserver-based incremental updates (default: true). */
  incrementalUpdates?: boolean;
  /** Use CDP Accessibility.getFullAXTree instead of DOM walk.
   * Default: false. DOM walk is 100x faster (5ms vs 500ms) and returns
   * more elements on complex sites. Use A11y only for native adapters
   * (desktop, SAP, etc.) where there is no DOM to walk. */
  useHybridSnapshot?: boolean;
  /** Wait for network quiet before snapshot (default: true). */
  networkQuietWait?: boolean;
  /** CEL bindings for Rust-native context processing. When provided, getContext()
   * routes raw elements through Rust for unified confidence scoring + normalization. */
  cel?: Cel;
  /** Extra Chromium launch arguments. */
  args?: string[];
  /** User data directory — enables persistent context (required for extensions). */
  userDataDir?: string;
  /** Playwright channel — use "chrome" for system Chrome instead of bundled Chromium. */
  channel?: string;
  /** Custom user agent string. */
  userAgent?: string;
  /** Semantic backend used when structured routing is insufficient. */
  semanticBackend?: "llm" | "heuristic";
  /**
   * Enable stealth mode (default: false).
   * When true, applies anti-detection measures:
   * - Uses --headless=new (real Chrome UA, not "HeadlessChrome")
   * - Hides navigator.webdriver
   * - Spoofs plugins, languages, WebGL renderer
   * - Sets realistic screen dimensions
   * - Adds anti-automation Chrome args
   * - Sets a real-looking user agent
   */
  stealth?: boolean;

  // --- Watchdog configs ---
  /** Popup/dialog handling config. */
  popups?: PopupWatchdogConfig;
  /** Download detection config. */
  downloads?: DownloadWatchdogConfig;
  /** Security/domain restriction config. */
  security?: SecurityWatchdogConfig;
  /** Storage state persistence config. */
  storage?: StorageWatchdogConfig;
}

export class BrowserAdapter {
  private client: CdpClient;
  private mutationTracker: MutationTracker;
  private actionHandler: ActionHandler | null = null;
  private networkTap: NetworkTap;
  private popupWatchdog: PopupWatchdog;
  private downloadWatchdog: DownloadWatchdog;
  private securityWatchdog: SecurityWatchdog;
  private storageWatchdog: StorageWatchdog;
  private config: BrowserAdapterConfig;
  /** Last hybrid snapshot (available after getContext if hybrid mode enabled). */
  private _lastSnapshot: HybridSnapshot | null = null;
  /** CEL bindings for routing context through Rust. */
  private _cel: Cel | null;

  constructor(config: BrowserAdapterConfig) {
    this.config = config;
    this._cel = config.cel ?? null;
    this.client = new CdpClient({
      browser: config.browser,
      wsEndpoint: config.wsEndpoint,
      cdpUrl: config.cdpUrl,
      headless: config.headless,
      viewport: config.viewport,
      args: config.args,
      userDataDir: config.userDataDir,
      channel: config.channel,
      userAgent: config.userAgent,
      stealth: config.stealth ?? true,
    });
    this.mutationTracker = new MutationTracker({
      sanitize: config.sanitize ?? true,
    });
    this.networkTap = new NetworkTap();
    this.popupWatchdog = new PopupWatchdog(config.popups);
    this.downloadWatchdog = new DownloadWatchdog(config.downloads);
    this.securityWatchdog = new SecurityWatchdog(config.security);
    this.storageWatchdog = new StorageWatchdog(config.storage);
  }

  /** Connect to the browser and attach all watchdogs. */
  async connect(): Promise<void> {
    await this.client.connect();

    // Inject closed shadow DOM capture patch before any page loads.
    try {
      if (this.client.cdp.connected) {
        await this.client.cdp.send("Page.addScriptToEvaluateOnNewDocument", {
          source: CLOSED_SHADOW_PATCH,
        });
        await this.client.cdp.evaluate(CLOSED_SHADOW_PATCH);
      } else if (this.client.hasPage) {
        await this.client.page.addInitScript(CLOSED_SHADOW_PATCH);
        await this.client.page.evaluate(CLOSED_SHADOW_PATCH);
      }
    } catch {
      // Best effort — closed shadow DOM capture is an optimization
    }

    // Stealth: inject anti-detection scripts before any page loads
    if (this.config.stealth && this.client.hasPage) {
      try {
        await this.client.page.addInitScript(STEALTH_SCRIPT);
      } catch { /* best effort */ }
    }

    // Set up action handler (Playwright mode only — needs Page)
    if (this.client.hasPage) {
      this.actionHandler = new ActionHandler(this.client.page);
    }

    // Attach all watchdogs — prefer CDP, fall back to Playwright
    if (this.client.cdp.connected) {
      await this.networkTap.attachCdp(this.client.cdp);
      await this.popupWatchdog.attachCdp(this.client.cdp);
      await this.downloadWatchdog.attachCdp(this.client.cdp);
      await this.securityWatchdog.attachCdp(this.client.cdp);
      // Auto-restore storage state if configured
      await this.storageWatchdog.autoRestore(this.client.cdp);
    } else if (this.client.hasPage) {
      this.networkTap.attach(this.client.page);
      this.popupWatchdog.attach(this.client.page);
      this.downloadWatchdog.attach(this.client.page);
      this.securityWatchdog.attach(this.client.page);
    }
  }

  /** Reconnect CDP to the current active tab.
   * Used when the page navigated via address bar (not CDP) and the
   * old CDP connection returns empty/stale results. */
  async reconnect(): Promise<void> {
    if (!this.config.cdpUrl) return;
    try {
      // Discover current tabs via HTTP endpoint
      const port = new URL(this.config.cdpUrl).port || "9222";
      const response = await fetch(`http://localhost:${port}/json/list`);
      const tabs = await response.json() as Array<{ webSocketDebuggerUrl: string; type: string; url: string }>;
      // Find the active page tab (not devtools, not extension)
      const pageTab = tabs.find(t => t.type === "page" && !t.url.startsWith("chrome-extension://") && !t.url.startsWith("devtools://"));
      if (pageTab?.webSocketDebuggerUrl && pageTab.webSocketDebuggerUrl !== this.config.cdpUrl) {
        // Disconnect old connection and reconnect to new tab
        await this.client.cdp.disconnect?.();
        await this.client.cdp.connectViaWebSocket(pageTab.webSocketDebuggerUrl);
        await this.client.cdp.enableDomain("Page");
        await this.client.cdp.enableDomain("Runtime");
      }
    } catch { /* reconnect failed — stay on current connection */ }
  }

  /** Disconnect and clean up. Save storage state if configured. */
  async disconnect(): Promise<void> {
    // Save storage state before disconnecting
    if (this.client.cdp.connected) {
      try {
        await this.storageWatchdog.captureFromCdp(this.client.cdp);
      } catch {
        // Best-effort — don't block disconnect
      }
    }

    this.mutationTracker.reset();
    this.networkTap.clear();
    this.popupWatchdog.clear();
    this.downloadWatchdog.clear();
    this.securityWatchdog.clear();
    this.actionHandler = null;
    await this.client.disconnect();
  }

  /** Whether the adapter is connected. */
  get isConnected(): boolean {
    return this.client.isConnected;
  }

  /** Whether we're in direct CDP mode (no Playwright). */
  get isDirectCdp(): boolean {
    return this.client.isDirectCdp;
  }

  /**
   * Get DOM elements as ContextElements.
   *
   * Two modes:
   * 1. Hybrid snapshot (default for Chromium): Uses CDP Accessibility.getFullAXTree
   *    for 80-90% smaller context with richer semantic information.
   * 2. Legacy DOM walk: Single Runtime.evaluate call with MutationObserver incremental updates.
   */
  async getElements(): Promise<ContextElement[]> {
    if (!this.client.isConnected) return [];

    // Use hybrid A11y snapshot only when explicitly opted in.
    // DOM walk is 100x faster (5ms vs 500ms) and finds more elements on complex sites.
    const useHybrid = this.config.useHybridSnapshot === true && this.client.cdp.connected;

    if (useHybrid) {
      return this.getElementsHybrid();
    }

    return this.getElementsLegacy();
  }

  /** Hybrid snapshot path: CDP Accessibility.getFullAXTree + bounds + URL map. */
  private async getElementsHybrid(): Promise<ContextElement[]> {
    try {
      // Wait for network quiet before snapshot
      if (this.config.networkQuietWait !== false) {
        await this.waitForNetworkQuiet();
      }

      const snapshot = await createHybridSnapshot(
        this.client.cdp,
        this.client.hasPage ? this.client.page : undefined,
      );
      this._lastSnapshot = snapshot;

      let elements = snapshot.elements;
      if (this.config.sanitize !== false) {
        elements = sanitizeElements(elements);
      }
      return elements;
    } catch (e) {
      // Hybrid snapshot failed — fall back to legacy DOM extraction
      console.warn("[browser-adapter] Hybrid snapshot failed, falling back to DOM walk:", String(e));
      return this.getElementsLegacy();
    }
  }

  /** Legacy DOM walk path: single Runtime.evaluate with MutationObserver. */
  private async getElementsLegacy(): Promise<ContextElement[]> {
    const evaluator = this.client.cdp.connected
      ? this.client.cdp
      : this.client.hasPage
        ? this.client.page
        : null;

    if (!evaluator) return [];

    const currentUrl = await this.client.getPageUrlAsync();

    if (this.config.incrementalUpdates !== false) {
      return this.mutationTracker.getElements(evaluator, currentUrl);
    }

    const rawElements = await extractDOMAllFrames(
      this.client.hasPage ? this.client.page : evaluator as any,
    );
    let elements = mapElements(rawElements);

    if (this.config.sanitize !== false) {
      elements = sanitizeElements(elements);
    }

    return elements;
  }

  /** Wait for network to go quiet (no pending requests for 500ms). */
  private async waitForNetworkQuiet(timeoutMs = 5000): Promise<void> {
    const start = Date.now();
    while (Date.now() - start < timeoutMs) {
      const pending = this.networkTap.getPendingCount();
      if (pending === 0) {
        // Wait 500ms more to confirm quiet
        await new Promise((r) => setTimeout(r, 500));
        if (this.networkTap.getPendingCount() === 0) return;
      }
      await new Promise((r) => setTimeout(r, 100));
    }
  }

  /**
   * Fast context: page text + interactive elements only.
   * ~5-10x faster than full getContext() — avoids full DOM tree walk.
   * Used for initial context and data extraction goals.
   */
  async getContextFast(): Promise<ScreenContext> {
    const title = await this.client.getPageTitle();
    const networkEvents = this.networkTap.getEvents() as any[];

    // Get page text (the most important signal for data extraction).
    // Prefer main content area over full body to skip nav/sidebar/filter noise.
    let pageTextElement: ContextElement | null = null;
    try {
      const bodyText = await this.evaluate<string>(
        `(() => {
          const main = document.querySelector('main, [role="main"], #content, #main, .main-content, [data-testid="search-results"], article');
          if (main && main.innerText.length > 200) return main.innerText.slice(0, 8000);
          return document.body?.innerText?.slice(0, 6000) ?? "";
        })()`,
      );
      if (bodyText && bodyText.length > 20) {
        pageTextElement = {
          id: "page-text",
          element_type: "text",
          label: "Page content",
          value: bodyText,
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: [],
          confidence: 0.95,
          source: "merged" as const,
        };
      }
    } catch { /* best effort */ }

    // Get only interactive elements via lightweight script
    let elements: ContextElement[] = [];
    try {
      const evaluator = this.client.cdp.connected ? this.client.cdp : this.client.hasPage ? this.client.page : null;
      if (evaluator) {
        const rawElements = await extractDOMLightweight(evaluator);
        elements = sanitizeElements(mapElements(rawElements));
      }
    } catch { /* fall through with empty elements */ }

    if (pageTextElement) elements.unshift(pageTextElement);

    // Extract inline links from main content area — makes article links
    // clickable instead of buried in page-text blob.
    const contentLinks = await this.extractInlineLinks(elements);
    elements.push(...contentLinks);

    return {
      app: "Browser",
      window: title,
      elements,
      network_events: [],
      http_events: networkEvents,
      timestamp_ms: Date.now(),
    };
  }

  /**
   * Get a full ScreenContext for the current page.
   *
   * When CEL bindings are available, routes raw elements through the Rust
   * pipeline for unified confidence scoring, element type normalization,
   * noise filtering, and sorting. This ensures Rust is the single source
   * of truth for context assembly.
   *
   * Falls back to manual TypeScript assembly when Rust is unavailable.
   */
  async getContext(): Promise<ScreenContext> {
    const [rawElements, title] = await Promise.all([
      this.getElements(),
      this.client.getPageTitle(),
    ]);

    const networkEvents = this.networkTap.getEvents() as any[];

    // Extract page text content — enables data extraction without scrolling.
    // The LLM needs to "see" prices, stats, headlines, etc. — not just buttons.
    let pageTextElement: ContextElement | null = null;
    try {
      const bodyText = await this.evaluate<string>(
        `document.body?.innerText?.slice(0, 8000) ?? ""`,
      );
      if (bodyText && bodyText.length > 20) {
        pageTextElement = {
          id: "page-text",
          element_type: "text",
          label: "Page content",
          value: bodyText,
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: [],
          confidence: 0.95,
          source: "merged" as const,
        };
      }
    } catch { /* best effort */ }

    // Extract structured table data — enables comparison/pricing tasks.
    // Tables on pricing pages (GitHub, HuggingFace) often have plan details
    // in columns that the accessibility tree misses (inactive tab content).
    let tableElements: ContextElement[] = [];
    try {
      const tableData = await this.evaluate<string>(`
        (function() {
          const tables = document.querySelectorAll('table');
          if (tables.length === 0) return '';
          const results = [];
          for (let t = 0; t < Math.min(tables.length, 3); t++) {
            const table = tables[t];
            const rows = table.querySelectorAll('tr');
            const data = [];
            for (let r = 0; r < Math.min(rows.length, 20); r++) {
              const cells = rows[r].querySelectorAll('th, td');
              const row = [];
              for (const cell of cells) {
                const text = (cell.textContent || '').trim().replace(/\\s+/g, ' ');
                if (text) row.push(text);
              }
              if (row.length > 0) data.push(row.join(' | '));
            }
            if (data.length > 0) results.push(data.join('\\n'));
          }
          return results.join('\\n---\\n');
        })()
      `);
      if (tableData && tableData.length > 30) {
        tableElements.push({
          id: "page-tables",
          element_type: "text",
          label: "Table data",
          value: tableData.slice(0, 4000),
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: [],
          confidence: 0.9,
          source: "merged" as const,
          content_role: "content" as const,
        } as ContextElement);
      }
    } catch { /* best effort */ }

    // Also extract comparison/pricing card data (div-based, not table-based)
    try {
      const cardData = await this.evaluate<string>(`
        (function() {
          const cards = document.querySelectorAll('[class*="pricing"], [class*="plan"], [class*="tier"], [data-testid*="plan"], [class*="comparison"]');
          if (cards.length === 0) return '';
          const results = [];
          for (let i = 0; i < Math.min(cards.length, 6); i++) {
            const text = (cards[i].textContent || '').trim().replace(/\\s+/g, ' ').slice(0, 500);
            if (text.length > 20) results.push(text);
          }
          return results.join('\\n---\\n');
        })()
      `);
      if (cardData && cardData.length > 30 && tableElements.length === 0) {
        tableElements.push({
          id: "page-pricing-cards",
          element_type: "text",
          label: "Pricing/plan data",
          value: cardData.slice(0, 4000),
          state: { focused: false, enabled: true, visible: true, selected: false },
          actions: [],
          confidence: 0.85,
          source: "merged" as const,
          content_role: "content" as const,
        } as ContextElement);
      }
    } catch { /* best effort */ }

    // Assemble context directly from TS-mapped elements.
    // The TS mapper (element-mapper.ts) already normalizes types, scores confidence,
    // assigns actions, and builds CSS selectors. Routing through Rust's
    // buildContextFromElements re-processes and drops elements (noise filter removes
    // structural context the LLM needs to understand page layout).
    // Rust is still used for planning (planStep) — just not for context assembly.
    const elements = [...rawElements];
    if (pageTextElement) elements.push(pageTextElement);
    elements.push(...tableElements);
    const fallbackContentLinks = await this.extractInlineLinks(elements);
    elements.push(...fallbackContentLinks);

    return {
      app: "Browser",
      window: title,
      elements,
      network_events: [],           // TCP-level events (browser can't capture these)
      http_events: networkEvents,    // HTTP events from CDP NetworkTap
      timestamp_ms: Date.now(),
    };
  }

  /**
   * Extract inline links from the main content area of the page.
   * Returns clickable ContextElements for links that are otherwise
   * buried inside the page-text blob.
   */
  private async extractInlineLinks(existingElements: ContextElement[]): Promise<ContextElement[]> {
    try {
      const inlineLinks = await this.evaluate<Array<{text: string; href: string; x: number; y: number; w: number; h: number}>>(`(() => {
        const main = document.querySelector('article, main, [role="main"], .post-content, .entry-content, .article-body')
          || document.querySelector('#content, #main, .content')
          || document.body;
        const anchors = main.querySelectorAll('a[href]');
        const result = [];
        const seen = new Set();
        for (const a of anchors) {
          const text = (a.textContent || '').trim();
          const href = a.href;
          if (!text || text.length < 2 || text.length > 100) continue;
          if (!href || href.startsWith('javascript:') || href === '#') continue;
          if (a.closest('nav, header, footer, [role="navigation"], [role="banner"]')) continue;
          // Skip ToC, citation, and edit links (Wikipedia noise)
          if (a.closest('.toc, .reflist, .references, .mw-editsection')) continue;
          if (href.includes('#cite_') || href.includes('#ref_') || text === '[edit]') continue;
          const rect = a.getBoundingClientRect();
          if (rect.width === 0 || rect.height === 0) continue;
          if (seen.has(href)) continue;
          seen.add(href);
          result.push({ text, href, x: Math.round(rect.x), y: Math.round(rect.y), w: Math.round(rect.width), h: Math.round(rect.height) });
          if (result.length >= 50) break;
        }
        return result;
      })()`);
      if (!inlineLinks?.length) return [];
      // Don't dedup — content-link elements have clear text labels that help
      // the LLM connect article text ("Machine learning") to a clickable element,
      // even if a dom:lw-X element with the same href exists (its label may be opaque).
      return inlineLinks
        .map((link, i) => ({
          id: `content-link:${i}`,
          element_type: "link",
          label: link.text,
          bounds: { x: link.x, y: link.y, width: link.w, height: link.h },
          state: { focused: false, enabled: true, visible: true, selected: false },
          confidence: 0.6,
          source: "merged" as const,
          properties: { href: link.href, css_selector: `a[href="${link.href.replace(/"/g, '\\"')}"]` },
        }));
    } catch { return []; }
  }

  /**
   * Tier 1 context: URL, title, scroll position, text preview. ~5ms.
   * Used when the LLM says it doesn't need full DOM observation.
   */
  async getContextTier1(): Promise<ScreenContext> {
    const evaluator = this.client.cdp.connected ? this.client.cdp : this.client.hasPage ? this.client.page : null;
    if (!evaluator) return this.emptyContext();

    const info = await this.evaluate<{
      url: string; title: string; scrollY: number;
      scrollHeight: number; viewportHeight: number; textPreview: string;
    }>(`(() => ({
      url: window.location.href,
      title: document.title,
      scrollY: window.scrollY,
      scrollHeight: document.documentElement ? document.documentElement.scrollHeight : 0,
      viewportHeight: window.innerHeight,
      textPreview: document.body?.innerText?.substring(0, 2000) || '',
    }))()`).catch(() => ({ url: '', title: '', scrollY: 0, scrollHeight: 0, viewportHeight: 0, textPreview: '' }));

    return {
      app: 'Browser',
      window: info.title || info.url,
      elements: [{
        id: 'page-text',
        label: info.textPreview,
        element_type: 'text',
        state: { visible: true, enabled: true, focused: false, selected: false },
        actions: [],
        confidence: 1.0,
        source: 'native_api' as const,
        properties: {
          url: info.url,
          scroll_y: String(info.scrollY),
          scroll_height: String(info.scrollHeight),
          viewport_height: String(info.viewportHeight),
        },
      }],
      network_events: [],
      http_events: [],
      timestamp_ms: Date.now(),
    };
  }

  /**
   * Tier 2 context: Interactive elements only (lightweight DOM extraction). ~50ms.
   * Delegates to getContextFast() which already does lightweight extraction.
   */
  async getContextTier2(): Promise<ScreenContext> {
    return this.getContextFast();
  }

  /** Empty context — used as fallback when no page is available. */
  private emptyContext(): ScreenContext {
    return {
      app: 'Browser',
      window: '',
      elements: [],
      network_events: [],
      http_events: [],
      timestamp_ms: Date.now(),
    };
  }

  /**
   * Evaluate JavaScript in the browser context.
   * Uses CDP Runtime.evaluate when available.
   */
  async evaluate<T = unknown>(script: string): Promise<T> {
    // Retry on "Execution context was destroyed" — happens when the page
    // navigates mid-evaluation (e.g., Yahoo Finance client-side redirects).
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        return await this.client.evaluate<T>(script);
      } catch (err) {
        const msg = String(err);
        if (msg.includes("Execution context was destroyed") || msg.includes("Target page, context or browser has been closed")) {
          if (attempt < 2) {
            await new Promise(r => setTimeout(r, 1500));
            continue;
          }
        }
        throw err;
      }
    }
    return this.client.evaluate<T>(script); // final attempt, let it throw
  }

  /** Navigate to a URL (enforces security watchdog). */
  async navigate(url: string): Promise<void> {
    // Security check before navigation
    if (!this.securityWatchdog.validateNavigation(url)) {
      const blocked = this.securityWatchdog.getBlocked();
      const reason = blocked[blocked.length - 1]?.reason ?? "blocked by security policy";
      throw new Error(`Navigation blocked: ${reason}`);
    }

    this.mutationTracker.reset();
    this.networkTap.clear();
    await this.client.navigate(url);
  }

  /**
   * Execute a browser-specific action.
   * Uses Playwright Locators for complex interactions, falls back to CDP.
   */
  async executeAction(
    action: string,
    params: Record<string, unknown>,
  ): Promise<boolean> {
    // Security check for navigate actions
    if (action === "navigate" && params.url) {
      if (!this.securityWatchdog.validateNavigation(params.url as string)) {
        throw new Error(
          `Navigation to ${params.url} blocked by security policy`,
        );
      }
    }

    // Reset mutation tracker on navigation actions
    if (action === "navigate" || action === "reload") {
      this.mutationTracker.reset();
    }

    // Use Playwright action handler if available, fall through to CDP on failure
    debugBrowser(
      `[executeAction] action=${action} hasActionHandler=${!!this.actionHandler} hasCdp=${this.client.cdp.connected}`,
    );
    if (this.actionHandler) {
      const result = await this.actionHandler.execute(action, params);
      if (result.success) return true;
      // Playwright failed — try CDP fallback before throwing
      if (this.client.cdp.connected) {
        try {
          return await this.executeCdpAction(action, params);
        } catch {
          // CDP also failed — throw original Playwright error
        }
      }
      throw new Error(`Browser action "${action}" failed: ${result.error}`);
    }

    // Direct CDP fallback for basic actions
    if (this.client.cdp.connected) {
      return this.executeCdpAction(action, params);
    }

    throw new Error("BrowserAdapter not connected");
  }

  /** Execute basic actions via raw CDP (when no Playwright). */
  private async executeCdpAction(
    action: string,
    params: Record<string, unknown>,
  ): Promise<boolean> {
    const cdp = this.client.cdp;
    switch (action) {
      case "navigate":
        await cdp.navigate(params.url as string);
        return true;
      case "click":
        // Selector-based click via CDP JS
        if (params.css_selector || params.selector) {
          const sel = (params.css_selector ?? params.selector) as string;
          await cdp.evaluate(`(() => {
            const el = document.querySelector(${JSON.stringify(sel)});
            if (el) { el.scrollIntoView({block:'center'}); el.click(); }
          })()`);
          return true;
        }
        if (params.backend_node_id) {
          try {
            const resolved = await cdp.send("DOM.resolveNode", { backendNodeId: Number(params.backend_node_id) }) as any;
            if (resolved?.object?.objectId) {
              await cdp.send("Runtime.callFunctionOn", {
                objectId: resolved.object.objectId,
                functionDeclaration: "function() { this.scrollIntoView({block:'center'}); this.click(); }",
              } as any);
              return true;
            }
          } catch { /* fall through to coordinates */ }
        }
        if (params.x !== undefined && params.y !== undefined) {
          await cdp.click(params.x as number, params.y as number);
          return true;
        }
        throw new Error("CDP click requires coordinates, css_selector, or backend_node_id");
      case "double_click":
        if (params.x !== undefined && params.y !== undefined) {
          await cdp.click(params.x as number, params.y as number);
          await cdp.click(params.x as number, params.y as number);
          return true;
        }
        throw new Error("CDP double_click requires x,y coordinates");
      case "type":
      case "input_text": {
        const text = (params.text ?? params.value) as string;
        // Selector-based type via CDP: focus element, clear, type
        // Selector-based type: set value directly via JS (most reliable for search boxes,
        // autocomplete fields, and custom components that intercept key events).
        if (params.selector || params.backend_node_id) {
          const sel = params.selector as string | undefined;
          const clearFirst = params.clearFirst !== false;
          try {
            // Strategy: find element, focus, set value directly, dispatch events.
            // This avoids character-by-character Input.dispatchKeyEvent which gets
            // intercepted by autocomplete and appends instead of replacing.
            const escapedText = JSON.stringify(text);
            const script = sel
              ? `(() => {
                  let el = document.querySelector(${JSON.stringify(sel)});
                  if (!el) {
                    const allInputs = document.querySelectorAll('input, textarea, [contenteditable], [role="combobox"], [role="searchbox"]');
                    for (const inp of allInputs) {
                      const label = inp.getAttribute('aria-label') || '';
                      if (label && ${JSON.stringify(sel || "")}.includes(label.slice(0, 10))) { el = inp; break; }
                    }
                  }
                  if (!el) return 'not_found';
                  const commitValue = (node, next) => {
                    const tag = (node.tagName || '').toUpperCase();
                    const proto = tag === 'TEXTAREA'
                      ? HTMLTextAreaElement.prototype
                      : tag === 'SELECT'
                        ? HTMLSelectElement.prototype
                        : HTMLInputElement.prototype;
                    const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set
                      || Object.getOwnPropertyDescriptor(Object.getPrototypeOf(node), 'value')?.set;
                    if (setter) setter.call(node, next);
                    else node.value = next;
                    try {
                      node.dispatchEvent(new InputEvent('input', {
                        bubbles: true,
                        composed: true,
                        inputType: 'insertReplacementText',
                        data: next,
                      }));
                    } catch (_) {
                      node.dispatchEvent(new Event('input', {bubbles: true, composed: true}));
                    }
                    node.dispatchEvent(new Event('change', {bubbles: true, composed: true}));
                  };
                  el.focus();
                  ${clearFirst ? "el.value = '';" : ""}
                  commitValue(el, ${escapedText});
                  return 'ok:' + el.value.slice(0, 30);
                })()`
              : null;

            if (script) {
              const result = await cdp.evaluate(script);
              debugBrowser(`[CDP-type] selector=${sel} result=${String(result).slice(0, 80)}`);
              if (result && String(result).startsWith("ok:")) {
                await new Promise(r => setTimeout(r, 300));
                return true;
              }
            }

            // Fallback: try via backend_node_id
            if (params.backend_node_id) {
              const resolved = await cdp.send("DOM.resolveNode", { backendNodeId: Number(params.backend_node_id) }) as any;
              if (resolved?.object?.objectId) {
                await cdp.send("Runtime.callFunctionOn", {
                  objectId: resolved.object.objectId,
                  functionDeclaration: `function() {
                    const commitValue = (node, next) => {
                      const tag = (node.tagName || '').toUpperCase();
                      const proto = tag === 'TEXTAREA'
                        ? HTMLTextAreaElement.prototype
                        : tag === 'SELECT'
                          ? HTMLSelectElement.prototype
                          : HTMLInputElement.prototype;
                      const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set
                        || Object.getOwnPropertyDescriptor(Object.getPrototypeOf(node), 'value')?.set;
                      if (setter) setter.call(node, next);
                      else node.value = next;
                      try {
                        node.dispatchEvent(new InputEvent('input', {
                          bubbles: true,
                          composed: true,
                          inputType: 'insertReplacementText',
                          data: next,
                        }));
                      } catch (_) {
                        node.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
                      }
                      node.dispatchEvent(new Event('change', { bubbles: true, composed: true }));
                    };
                    this.focus();
                    ${clearFirst ? "this.value = '';" : ""}
                    commitValue(this, ${escapedText});
                  }`,
                } as any);
                await new Promise(r => setTimeout(r, 300));
                return true;
              }
            }
          } catch { /* fall through to coordinate-based */ }
        }
        // Coordinate-based type: click to focus first
        if (params.x !== undefined && params.y !== undefined) {
          await cdp.click(params.x as number, params.y as number);
          await new Promise((r) => setTimeout(r, 150));
          // Clear existing text if requested
          if (params.clearFirst !== false) {
            // modifiers: 4 = Meta (Cmd on macOS), 2 = Control (Windows/Linux)
            const selectAllMod = process.platform === "darwin" ? 4 : 2;
            await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", key: "a", code: "KeyA", modifiers: selectAllMod });
            await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", key: "a", code: "KeyA", modifiers: selectAllMod });
            await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", key: "Backspace", code: "Backspace" });
            await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", key: "Backspace", code: "Backspace" });
            await new Promise((r) => setTimeout(r, 50));
          }
          // Type character by character with key events
          for (const char of text) {
            await cdp.send("Input.dispatchKeyEvent", {
              type: "keyDown", key: char, text: char, unmodifiedText: char,
            });
            await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", key: char });
            await new Promise((r) => setTimeout(r, 30));
          }
          await new Promise((r) => setTimeout(r, 300));
          return true;
        }
        await cdp.insertText(text);
        return true;
      }
      case "select_option": {
        const targetValue = String(params.value ?? params.label ?? "").trim();
        if (!targetValue) throw new Error("select_option requires value or label");

        const escapedTarget = JSON.stringify(targetValue);
        const selector = params.selector as string | undefined;

        const selectScript = `
          (() => {
            const target = ${escapedTarget}.toLowerCase();
            const findOption = (selectEl) => {
              const options = Array.from(selectEl.options || []);
              return options.find((opt) =>
                String(opt.value || '').toLowerCase() === target ||
                String(opt.label || opt.textContent || '').trim().toLowerCase() === target
              ) || null;
            };
            const apply = (el) => {
              if (!el) return 'not_found';
              const tag = (el.tagName || '').toLowerCase();
              if (tag === 'select') {
                const opt = findOption(el);
                if (!opt) return 'no_match';
                el.focus();
                el.value = opt.value;
                if (el.value !== opt.value) {
                  const idx = Array.from(el.options).indexOf(opt);
                  if (idx >= 0) el.selectedIndex = idx;
                }
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return 'ok:' + el.value;
              }
              const role = (el.getAttribute && el.getAttribute('role')) || '';
              if (role.toLowerCase() === 'combobox') {
                el.focus();
                if ('value' in el) el.value = ${escapedTarget};
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return 'ok:' + (el.value || '');
              }
              return 'unsupported';
            };
            ${selector
              ? `return apply(document.querySelector(${JSON.stringify(selector)}));`
              : `return 'selector_missing';`
            }
          })()
        `;

        if (selector) {
          const result = await cdp.evaluate(selectScript);
          if (String(result).startsWith("ok:")) {
            await new Promise(r => setTimeout(r, 200));
            return true;
          }
        }

        if (params.backend_node_id) {
          try {
            const resolved = await cdp.send("DOM.resolveNode", { backendNodeId: Number(params.backend_node_id) }) as any;
            if (resolved?.object?.objectId) {
              const result = await cdp.send("Runtime.callFunctionOn", {
                objectId: resolved.object.objectId,
                returnByValue: true,
                functionDeclaration: `function(value) {
                  const target = String(value || '').trim().toLowerCase();
                  const tag = (this.tagName || '').toLowerCase();
                  if (tag === 'select') {
                    const options = Array.from(this.options || []);
                    const opt = options.find((option) =>
                      String(option.value || '').toLowerCase() === target ||
                      String(option.label || option.textContent || '').trim().toLowerCase() === target
                    );
                    if (!opt) return 'no_match';
                    this.focus();
                    this.value = opt.value;
                    if (this.value !== opt.value) {
                      const idx = options.indexOf(opt);
                      if (idx >= 0) this.selectedIndex = idx;
                    }
                    this.dispatchEvent(new Event('input', { bubbles: true }));
                    this.dispatchEvent(new Event('change', { bubbles: true }));
                    return 'ok:' + this.value;
                  }
                  const role = (this.getAttribute && this.getAttribute('role')) || '';
                  if (String(role).toLowerCase() === 'combobox') {
                    this.focus();
                    if ('value' in this) this.value = value;
                    this.dispatchEvent(new Event('input', { bubbles: true }));
                    this.dispatchEvent(new Event('change', { bubbles: true }));
                    return 'ok:' + (this.value || '');
                  }
                  return 'unsupported';
                }`,
                arguments: [{ value: targetValue }],
              } as any) as any;
              if (String(result?.result?.value ?? "").startsWith("ok:")) {
                await new Promise(r => setTimeout(r, 200));
                return true;
              }
            }
          } catch { /* fall through */ }
        }

        throw new Error(`select_option failed for value "${targetValue}"`);
      }
      case "press_key":
      case "key_press":
        await cdp.send("Input.dispatchKeyEvent", {
          type: "keyDown", key: (params.key ?? params.value) as string,
        });
        await cdp.send("Input.dispatchKeyEvent", {
          type: "keyUp", key: (params.key ?? params.value) as string,
        });
        return true;
      case "key_combo": {
        const keys = params.keys as string[];
        for (const key of keys) {
          await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", key });
        }
        for (const key of [...keys].reverse()) {
          await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", key });
        }
        return true;
      }
      case "scroll_by":
        await cdp.evaluate(
          `window.scrollBy(${params.dx ?? 0}, ${params.dy ?? 0})`,
        );
        return true;
      case "screenshot":
        await cdp.screenshot("png");
        return true;
      case "reload":
        await cdp.send("Page.reload");
        return true;
      case "go_back":
        await cdp.evaluate("history.back()");
        return true;
      case "go_forward":
        await cdp.evaluate("history.forward()");
        return true;
      default:
        throw new Error(
          `Action "${action}" requires Playwright (not available in direct CDP mode). ` +
          `Use wsEndpoint instead of cdpUrl for full action support.`
        );
    }
  }

  // --- Watchdog accessors ---

  /** Get buffered network events. */
  getNetworkEvents(): any[] {
    return this.networkTap.getEvents();
  }

  /** Get popup/dialog events (alerts, confirms, prompts that were auto-handled). */
  getPopupEvents(): PopupEvent[] {
    return this.popupWatchdog.getEvents();
  }

  /** Get download events. */
  getDownloadEvents(): DownloadEvent[] {
    return this.downloadWatchdog.getEvents();
  }

  /** Whether any downloads are currently in progress. */
  get hasPendingDownloads(): boolean {
    return this.downloadWatchdog.hasPendingDownloads;
  }

  /** Get security events (blocked/allowed navigations). */
  getSecurityEvents(): SecurityEvent[] {
    return this.securityWatchdog.getEvents();
  }

  /** Manually save storage state (cookies + localStorage). */
  async saveStorageState(): Promise<StorageState | null> {
    if (this.client.cdp.connected) {
      return this.storageWatchdog.captureFromCdp(this.client.cdp);
    }
    return null;
  }

  /** Manually restore storage state. */
  async restoreStorageState(state?: StorageState): Promise<void> {
    if (this.client.cdp.connected) {
      await this.storageWatchdog.restoreToCdp(this.client.cdp, state ?? undefined);
    }
  }

  /** Get the current storage state (cookies + localStorage). */
  get storageState(): StorageState | null {
    return this.storageWatchdog.state;
  }

  // --- Existing methods ---

  /** Get the current page title. */
  async getPageTitle(): Promise<string> {
    return this.client.getPageTitle();
  }

  /** Get the current page URL. */
  getPageUrl(): string {
    return this.client.getPageUrl();
  }

  /**
   * Resolve a higher-level action into grounded browser adapter actions.
   * Uses the current normalized context as the source of truth.
   */
  async resolveSemanticAction(
    action: PlannedAction,
    context: ScreenContext,
  ): Promise<PlannedAction | null> {
    if (action.type === "act") {
      const { resolveActInstruction } = await import("./callback-builder.js");
      const resolved = resolveActInstruction(action.instruction, context);
      if (resolved.action === "type") {
        return { type: "type", target_id: resolved.targetId, text: resolved.text ?? "" };
      }
      if (resolved.targetId) return { type: "click", target_id: resolved.targetId };
      return null;
    }

    if (action.type !== "click" && action.type !== "type" && action.type !== "set_value") {
      return null;
    }

    const semanticAction = action;
    const backend = this.config.semanticBackend ?? (this._cel ? "llm" : "heuristic");
    return backend === "llm"
      ? this.resolveSemanticActionWithLlm(semanticAction, context)
      : this.resolveSemanticActionHeuristically(semanticAction, context);
  }

  /**
   * Register a script to run on every new document (before page scripts).
   * Uses CDP Page.addScriptToEvaluateOnNewDocument or Playwright addInitScript.
   * Critical for anti-detection: must run before site scripts can detect automation.
   */
  async addInitScript(script: string): Promise<void> {
    if (this.client.cdp.connected) {
      await this.client.cdp.send("Page.addScriptToEvaluateOnNewDocument", {
        source: script,
      });
    } else if (this.client.hasPage) {
      await this.client.page.addInitScript(script);
    }
    // Also execute immediately on the current page
    try {
      await this.evaluate(script);
    } catch {
      /* best effort */
    }
  }

  /** Take a screenshot as a PNG Buffer. */
  async screenshot(): Promise<Buffer> {
    return this.client.screenshot();
  }

  /** Force a full DOM re-extraction (bypasses incremental cache). */
  async forceRefresh(): Promise<ContextElement[]> {
    const evaluator = this.client.cdp.connected
      ? this.client.cdp
      : this.client.hasPage
        ? this.client.page
        : null;
    if (!evaluator) return [];
    return this.mutationTracker.fullExtraction(evaluator);
  }

  /**
   * Detect a blocking overlay (cookie consent, paywall, modal, etc.) on the
   * current page. Returns null if none. Pure observation — does not click.
   * Requires Playwright mode.
   */
  async detectOverlay(): Promise<BlockingOverlay | null> {
    if (!this.client.hasPage) return null;
    return detectOverlay(this.client.page);
  }

  /**
   * Detect and dismiss a blocking overlay. Tries TCF API → CMP selectors →
   * ARIA → positional X → text-match, in priority order. Privacy-preserving:
   * prefers reject over accept.
   */
  async dismissOverlay(): Promise<DismissResult> {
    if (!this.client.hasPage) return { success: false, detail: "no page (CDP-only mode)" };
    return dismissOverlay(this.client.page);
  }

  /**
   * Backwards-compatible alias — same shape (returns boolean) but now backed
   * by the overlay detector instead of action-handler's hardcoded selectors.
   * Existing callers (callback-builder, process-driver, cel-run) keep working.
   */
  async dismissCookieConsent(): Promise<boolean> {
    if (!this.client.hasPage) return false;
    const result = await dismissOverlay(this.client.page);
    return result.success;
  }

  private async resolveSemanticActionHeuristically(
    action: Extract<PlannedAction, { type: "click" | "type" | "set_value" }>,
    context: ScreenContext,
  ): Promise<PlannedAction | null> {
    const goalText = action.type === "click"
      ? `click ${action.target_id}`
      : action.type === "set_value"
        ? `fill ${action.target_id} with ${action.value}`
        : `type ${action.text}`;

    const lowerGoal = goalText.toLowerCase();
    let best: ContextElement | null = null;
    let bestScore = 0;

    for (const element of context.elements ?? []) {
      const text = [element.label, element.description, element.value, element.properties?.placeholder]
        .filter(Boolean)
        .join(" ")
        .toLowerCase();
      if (!text) continue;

      let score = 0;
      for (const word of lowerGoal.split(/\s+/).filter((w) => w.length > 2)) {
        if (text.includes(word)) score += 2;
      }
      if ((element.actions?.length ?? 0) > 0) score += 1;
      if (element.state?.visible) score += 0.5;
      if (element.state?.enabled) score += 0.5;

      if (score > bestScore) {
        best = element;
        bestScore = score;
      }
    }

    if (!best || bestScore < 2) return null;
    if (action.type === "click") return { type: "click", target_id: best.id };
    if (action.type === "set_value") return { type: "set_value", target_id: best.id, value: action.value };
    return { type: "type", target_id: best.id, text: action.text };
  }

  private async resolveSemanticActionWithLlm(
    action: Extract<PlannedAction, { type: "click" | "type" | "set_value" }>,
    context: ScreenContext,
  ): Promise<PlannedAction | null> {
    if (!this._cel) return this.resolveSemanticActionHeuristically(action, context);

    const candidates = (context.elements ?? [])
      .filter((element) => element.state?.visible !== false)
      .slice(0, 40)
      .map((element) => ({
        id: element.id,
        label: element.label ?? "",
        type: element.element_type,
        description: element.description ?? "",
        actions: element.actions ?? [],
      }));

    const userPrompt = [
      `Requested action: ${JSON.stringify(action)}`,
      `Window: ${context.window}`,
      "Pick the single best target element from the candidate list.",
      "Return JSON only: {\"target_id\":\"...\"}",
      JSON.stringify(candidates),
    ].join("\n");

    try {
      const raw = await this._cel.llmCompleteWithRole(
        "You resolve browser actions to grounded element ids. Respond with valid JSON only.",
        userPrompt,
        "validator",
        256,
      );
      const match = raw.match(/\{[\s\S]*\}/);
      const parsed = match ? JSON.parse(match[0]) : null;
      const targetId = typeof parsed?.target_id === "string" ? parsed.target_id : null;
      if (!targetId || !candidates.some((candidate) => candidate.id === targetId)) {
        return this.resolveSemanticActionHeuristically(action, context);
      }
      if (action.type === "click") return { type: "click", target_id: targetId };
      if (action.type === "set_value") return { type: "set_value", target_id: targetId, value: action.value };
      return { type: "type", target_id: targetId, text: action.text };
    } catch {
      return this.resolveSemanticActionHeuristically(action, context);
    }
  }

  /**
   * Wait for the page to stabilize after an action.
   * Also waits for pending downloads to complete.
   */
  async waitForStable(options?: { timeout?: number; idleTime?: number }): Promise<void> {
    const timeout = options?.timeout ?? 5000;
    const idleTime = options?.idleTime ?? 500;

    if (this.client.hasPage) {
      try {
        // Use domcontentloaded instead of networkidle — page text and DOM
        // are ready at this point. networkidle waits for ALL requests
        // (ads, trackers, analytics) which can take 60-70s on heavy sites.
        await this.client.page.waitForLoadState("domcontentloaded", { timeout });
      } catch {
        // Timeout is OK — some pages never reach full load
      }
    }

    // Wait for DOM to stop changing
    const evaluator = this.client.cdp.connected ? this.client.cdp : this.client.hasPage ? this.client.page : null;
    if (!evaluator) return;

    const start = Date.now();
    let lastMutationCount = -1;

    while (Date.now() - start < timeout) {
      const mutations = await (evaluator as any).evaluate(
        `(window.__cel_mutations || []).length`,
      ).catch(() => 0) as number;

      if (mutations === lastMutationCount && !this.downloadWatchdog.hasPendingDownloads) {
        break;
      }

      lastMutationCount = mutations;
      await new Promise((r) => setTimeout(r, idleTime));
    }
  }

  /** Access the raw CDP channel for advanced protocol operations. */
  get cdpChannel() {
    return this.client.cdp;
  }

  /** Get the last hybrid snapshot's XPath map (element ID → XPath). */
  get xpathMap(): Map<string, string> | null {
    return this._lastSnapshot?.xpathMap ?? null;
  }

  /** Get the last hybrid snapshot's URL map (for anti-hallucination). */
  get urlMap(): UrlMap | null {
    return this._lastSnapshot?.urlMap ?? null;
  }

  /** Get the last hybrid snapshot's frame tree. */
  get frameTree() {
    return this._lastSnapshot?.frameTree ?? null;
  }

  /**
   * Observe available actions on the current page. No LLM call needed —
   * enumerates what's possible based on element types and ARIA roles.
   */
  async observe(): Promise<ObservedAction[]> {
    const elements = await this.getElements();
    const results: ObservedAction[] = [];

    for (const el of elements) {
      const actions = deriveAvailableActions(el.element_type, el.state);
      if (actions.length === 0) continue;

      results.push({
        elementId: el.id,
        label: el.label ?? "",
        elementType: el.element_type,
        availableActions: actions,
        bounds: el.bounds,
      });
    }

    return results;
  }

  // --- Zero-cost inspection methods (no LLM tokens) ─────────────────────────

  /**
   * Search visible page text with a regex pattern. No LLM cost.
   * Returns matching text snippets with the closest element IDs.
   */
  async searchPage(
    pattern: string,
  ): Promise<Array<{ text: string; elementId: string | null }>> {
    const script = `(() => {
      const results = [];
      const regex = new RegExp(${JSON.stringify(pattern)}, 'gi');
      const walker = document.createTreeWalker(
        document.body,
        NodeFilter.SHOW_TEXT,
        null,
      );
      let node;
      while ((node = walker.nextNode()) && results.length < 50) {
        const text = node.textContent || '';
        const matches = text.match(regex);
        if (matches) {
          const el = node.parentElement;
          const id = el?.id ? 'dom:' + el.id : el?.getAttribute('data-cel-id') || null;
          for (const match of matches) {
            results.push({ text: match.slice(0, 200), elementId: id });
          }
        }
      }
      return results;
    })()`;

    try {
      return await this.evaluate<Array<{ text: string; elementId: string | null }>>(script);
    } catch {
      return [];
    }
  }

  /**
   * Query elements by CSS selector. No LLM cost.
   * Returns matching elements mapped to ContextElements.
   */
  async findElements(
    selector: string,
  ): Promise<ContextElement[]> {
    const script = `(() => {
      const results = [];
      const els = document.querySelectorAll(${JSON.stringify(selector)});
      for (let i = 0; i < Math.min(els.length, 100); i++) {
        const el = els[i];
        const rect = el.getBoundingClientRect();
        results.push({
          id: el.id ? 'dom:' + el.id : 'dom:' + el.tagName.toLowerCase() + ':' + i,
          tag: el.tagName.toLowerCase(),
          text: (el.innerText || el.textContent || '').trim().slice(0, 200),
          bounds: rect.width > 0 ? {
            x: Math.round(rect.x),
            y: Math.round(rect.y),
            width: Math.round(rect.width),
            height: Math.round(rect.height),
          } : null,
          visible: el.offsetParent !== null,
        });
      }
      return results;
    })()`;

    try {
      const raw = await this.evaluate<Array<{
        id: string;
        tag: string;
        text: string;
        bounds: { x: number; y: number; width: number; height: number } | null;
        visible: boolean;
      }>>(script);

      return raw.map((r) => ({
        id: r.id,
        label: r.text || undefined,
        element_type: r.tag,
        bounds: r.bounds || undefined,
        state: {
          focused: false,
          enabled: true,
          visible: r.visible,
          selected: false,
        },
        confidence: 0.8,
        source: "native_api" as const,
      }));
    } catch {
      return [];
    }
  }
}

// Re-export types for consumers
export type { RawDOMElement } from "./dom-extractor.js";
export type { ActionResult } from "./action-handler.js";
export type { PopupEvent } from "./popup-watchdog.js";
export type { DownloadEvent } from "./download-watchdog.js";
export type { SecurityEvent } from "./security-watchdog.js";
export type { StorageState } from "./storage-watchdog.js";
export { CdpChannel } from "./cdp-channel.js";
export { PopupWatchdog } from "./popup-watchdog.js";
export { DownloadWatchdog } from "./download-watchdog.js";
export { SecurityWatchdog } from "./security-watchdog.js";
export { StorageWatchdog } from "./storage-watchdog.js";
export { sanitizeElements } from "./sanitizer.js";
export { mapElements } from "./element-mapper.js";
export { extractDOM, extractDOMAllFrames, extractDOMWithViewport, CLOSED_SHADOW_PATCH } from "./dom-extractor.js";
export type { ViewportInfo, ExtractionResult } from "./dom-extractor.js";
export { createHybridSnapshot, type HybridSnapshot, type FrameInfo } from "./hybrid-snapshot.js";
export { extractA11yTree, flattenA11yTree, type A11yNode } from "./cdp-a11y-extractor.js";
export { UrlMap } from "./url-map.js";
export { DropdownHandler, type DropdownOption } from "./dropdown-handler.js";
export { celRun, runBrowserGoal, type CelRunConfig, type CelRunResult } from "./cel-run.js";
export {
  buildBrowserCallbacks, executeBrowserAction, resolveActInstruction,
  navigateAndPrepare, detectBotBlock,
  type BrowserCallbackOptions,
} from "./callback-builder.js";
