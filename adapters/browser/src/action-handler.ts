/**
 * Action Handler — browser-specific actions executed via CDP/Playwright.
 *
 * These map to the `custom` action type in WorkflowAction
 * when adapter === "browser".
 *
 * License: MIT
 */

import type { Page } from "playwright";

export interface ActionResult {
  success: boolean;
  error?: string;
  data?: unknown;
}

export class ActionHandler {
  constructor(private page: Page) {}

  /** Dispatch a browser action by name. */
  async execute(
    action: string,
    params: Record<string, unknown>,
  ): Promise<ActionResult> {
    switch (action) {
      case "navigate":
        return this.navigate(params.url as string);

      case "scroll_to":
        return this.scrollTo(params.selector as string);

      case "scroll_by":
        return this.scrollBy(
          (params.dx ?? 0) as number,
          (params.dy ?? 0) as number,
        );

      case "hover":
        if (typeof params.x === "number" && typeof params.y === "number") {
          return this.hoverAt(params.x, params.y);
        }
        return this.hover(params.selector as string);

      case "focus":
        return this.focus(params.selector as string);

      case "select":
        return this.select(
          params.selector as string,
          params.value as string,
        );

      case "select_option":
        return this.selectOption(
          params.selector as string | undefined,
          params.value as string | undefined,
          params.label as string | undefined,
        );

      case "fill":
        return this.fill(
          params.selector as string,
          params.value as string,
        );

      case "check":
        return this.check(
          params.selector as string,
          params.checked as boolean | undefined,
        );

      case "wait_for":
        return this.waitFor(
          params.selector as string,
          params.timeout as number | undefined,
        );

      case "screenshot":
        return this.screenshot();

      case "go_back":
        return this.goBack();

      case "go_forward":
        return this.goForward();

      case "reload":
        return this.reload();

      case "drag":
        return this.drag(
          params.fromX as number, params.fromY as number,
          params.toX as number, params.toY as number,
        );

      case "click":
        // Smart click cascade: href → locator → JS → coordinates
        if (params.href || params.css_selector || params.backend_node_id) {
          return this.smartClick(params as any);
        }
        // Legacy: coordinate-based or selector-based
        if (typeof params.x === "number" && typeof params.y === "number") {
          return this.clickAt(params.x, params.y);
        }
        return this.click(params.selector as string);

      case "double_click":
        if (typeof params.x === "number" && typeof params.y === "number") {
          return this.doubleClickAt(params.x, params.y);
        }
        return this.doubleClick(params.selector as string);

      case "dismiss_cookies":
        return this.dismissCookieConsent();

      // Common LLM planner aliases — map to canonical actions
      case "go_to_url":
      case "goto_url":
      case "goto":
      case "open_url":
      case "open_page":
        return this.navigate((params.url ?? params.href) as string);

      case "press_key":
      case "key_press":
        return this.keyPress((params.key ?? params.value) as string);

      case "key_combo":
        return this.keyCombo(params.keys as string[]);

      case "type":
      case "input_text":
        // Coordinate-based type: click to focus, then type
        if (typeof params.x === "number" && typeof params.y === "number") {
          return this.typeAt(
            params.x,
            params.y,
            (params.text ?? params.value) as string,
            params.clearFirst as boolean | undefined,
          );
        }
        return this.fill(
          params.selector as string,
          (params.text ?? params.value) as string,
        );

      case "text_select":
        return this.textSelect(
          params.fromX as number, params.fromY as number,
          params.toX as number, params.toY as number,
        );

      default:
        return { success: false, error: `Unknown browser action: ${action}` };
    }
  }

  /** Check if a popup window appeared after an action and switch focus to it. */
  async checkForPopup(): Promise<{ hasPopup: boolean; switched: boolean }> {
    try {
      const pages = this.page.context().pages();
      if (pages.length > 1) {
        const popup = pages[pages.length - 1];
        this.page = popup;
        return { hasPopup: true, switched: true };
      }
    } catch { /* ignore */ }
    return { hasPopup: false, switched: false };
  }

  private async navigate(url: string): Promise<ActionResult> {
    try {
      await this.page.goto(url, { waitUntil: "domcontentloaded" });
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async scrollTo(selector: string): Promise<ActionResult> {
    try {
      await this.page.locator(selector).first().scrollIntoViewIfNeeded();
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async hover(selector: string): Promise<ActionResult> {
    try {
      await this.page.locator(selector).first().hover();
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async focus(selector: string): Promise<ActionResult> {
    try {
      await this.page.locator(selector).first().focus();
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async select(
    selector: string,
    value: string,
  ): Promise<ActionResult> {
    try {
      await this.page.locator(selector).first().selectOption(value);
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async selectOption(
    selector: string | undefined,
    value: string | undefined,
    label: string | undefined,
  ): Promise<ActionResult> {
    if (!selector) {
      return { success: false, error: "select_option requires a selector" };
    }
    const target = value || label;
    if (!target) {
      return { success: false, error: "select_option requires a value or label" };
    }

    const loc = this.page.locator(selector).first();

    // Cascade: exact value → exact label → case-insensitive JS fallback.
    // LLM often sends "Warn" when the <option value="warn"> has lowercase value
    // but display text "Warn". Try all reasonable matches.
    // Short per-attempt timeout: when the option does not match, Playwright would
    // otherwise wait the default 30s before rejecting.
    const attemptTimeout = 1000;
    try {
      await loc.selectOption({ value: target }, { timeout: attemptTimeout });
      return { success: true };
    } catch { /* value didn't match exactly */ }

    try {
      await loc.selectOption({ label: target }, { timeout: attemptTimeout });
      return { success: true };
    } catch { /* label didn't match exactly */ }

    // Case-insensitive fallback via JS
    try {
      const matched = await this.page.evaluate(
        ([sel, val]) => {
          const el = document.querySelector(sel) as HTMLSelectElement | null;
          if (!el) return false;
          const lowerVal = val.toLowerCase();
          for (let oi = 0; oi < el.options.length; oi++) { const opt = el.options[oi];
            if (opt.value.toLowerCase() === lowerVal || opt.text.toLowerCase() === lowerVal) {
              el.value = opt.value;
              el.dispatchEvent(new Event("change", { bubbles: true }));
              return true;
            }
          }
          return false;
        },
        [selector, target] as [string, string],
      );
      if (matched) return { success: true };
    } catch { /* JS fallback failed */ }

    return { success: false, error: `select_option: no option matching "${target}" in ${selector}` };
  }

  private async fill(
    selector: string,
    value: string,
  ): Promise<ActionResult> {
    // Tier 1: Playwright fill (handles most inputs)
    try {
      await this.page.locator(selector).first().fill(value);
      return { success: true };
    } catch {
      // Tier 2: setValueDirect (handles readonly/datepicker inputs, React/Vue reactivity)
      const directResult = await this.setValueDirect(selector, value);
      if (directResult.success) return directResult;

      // Tier 3: Click then type (handles inputs that reject programmatic fill)
      try {
        const loc = this.page.locator(selector).first();
        await loc.click({ timeout: 3000 });
        await loc.selectText().catch(() => {}); // select existing text
        await this.page.keyboard.type(value);
        return { success: true };
      } catch { /* fall through */ }
    }
    return { success: false, error: `Fill cascade failed for: ${selector}` };
  }

  /**
   * Set value directly via JavaScript — handles readonly inputs, datepickers,
   * date/time/range inputs by removing readonly, setting .value via native
   * setter, and dispatching input + change events for framework reactivity.
   */
  async setValueDirect(
    selector: string,
    value: string,
  ): Promise<ActionResult> {
    try {
      await this.page.locator(selector).first().evaluate(
        (el, val) => {
          const input = el as HTMLInputElement;
          const type = (input.type || "").toLowerCase();

          // Remove readonly if present (common on datepickers, date inputs)
          const wasReadonly = input.hasAttribute("readonly");
          if (wasReadonly) input.removeAttribute("readonly");

          // For date/time/range inputs, ensure the value format is correct
          if (type === "date" && !/^\d{4}-\d{2}-\d{2}$/.test(val)) {
            // Try to parse common formats: MM/DD/YYYY, DD/MM/YYYY
            const parts = val.replace(/[\/\-\.]/g, "/").split("/");
            if (parts.length === 3) {
              const [a, b, c] = parts;
              // Assume MM/DD/YYYY if first part <= 12
              val = Number(a) <= 12
                ? `${c.padStart(4, "20")}-${a.padStart(2, "0")}-${b.padStart(2, "0")}`
                : `${c.padStart(4, "20")}-${b.padStart(2, "0")}-${a.padStart(2, "0")}`;
            }
          }

          // Normalize time: "2:30 PM" → "14:30", "12:26 PM" → "12:26"
          if (type === "time") {
            const m = val.match(/(\d{1,2}):(\d{2})\s*(AM|PM)?/i);
            if (m) {
              let h = parseInt(m[1], 10);
              const min = m[2];
              const ampm = (m[3] || "").toUpperCase();
              if (ampm === "PM" && h < 12) h += 12;
              if (ampm === "AM" && h === 12) h = 0;
              val = String(h).padStart(2, "0") + ":" + min;
            }
          }

          // Set the value via native setter to trigger React/Vue reactivity
          const nativeSetter = Object.getOwnPropertyDescriptor(
            HTMLInputElement.prototype, "value",
          )?.set;
          if (nativeSetter) {
            nativeSetter.call(input, val);
          } else {
            input.value = val;
          }

          // Dispatch events to notify frameworks
          input.dispatchEvent(new Event("input", { bubbles: true }));
          input.dispatchEvent(new Event("change", { bubbles: true }));

          // Restore readonly if it was originally set
          if (wasReadonly) input.setAttribute("readonly", "");
        },
        value,
      );
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async check(
    selector: string,
    checked?: boolean,
  ): Promise<ActionResult> {
    try {
      const locator = this.page.locator(selector).first();
      if (checked === false) {
        await locator.uncheck();
      } else {
        await locator.check();
      }
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async waitFor(
    selector: string,
    timeout?: number,
  ): Promise<ActionResult> {
    try {
      await this.page
        .locator(selector)
        .first()
        .waitFor({ state: "visible", timeout: timeout ?? 10000 });
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async screenshot(): Promise<ActionResult> {
    try {
      const buffer = await this.page.screenshot({ type: "png" });
      return { success: true, data: buffer.toString("base64") };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async goBack(): Promise<ActionResult> {
    try {
      await this.page.goBack({ waitUntil: "domcontentloaded" });
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async goForward(): Promise<ActionResult> {
    try {
      await this.page.goForward({ waitUntil: "domcontentloaded" });
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async reload(): Promise<ActionResult> {
    try {
      await this.page.reload({ waitUntil: "domcontentloaded" });
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async keyPress(key: string): Promise<ActionResult> {
    try {
      await this.page.keyboard.press(key);
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  private async click(selector: string): Promise<ActionResult> {
    // Tier 1: Playwright locator click (handles auto-scroll, overlay detection)
    try {
      await this.page.locator(selector).first().click({ timeout: 5000 });
      return { success: true };
    } catch {
      // Tier 2: JS click fallback (handles jQuery handlers, custom click events)
      try {
        await this.page.locator(selector).first().evaluate((el) => (el as unknown as { click(): void }).click());
        return { success: true };
      } catch {
        // Tier 3: Coordinate click (last resort)
        try {
          const box = await this.page.locator(selector).first().boundingBox();
          if (box) {
            await this.page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
            return { success: true };
          }
        } catch { /* fall through */ }
      }
    }
    return { success: false, error: `Click cascade failed for: ${selector}` };
  }

  /** Click at exact coordinates via Playwright mouse API. */
  private async clickAt(x: number, y: number): Promise<ActionResult> {
    try {
      await this.page.mouse.click(x, y);
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  /**
   * Smart click cascade — tries multiple targeting methods in order of reliability.
   * Inspired by Browser-Use's super-selector pattern.
   *
   * Cascade: href navigate → CSS locator → JS element.click() → coordinate click
   */
  async smartClick(params: {
    href?: string;
    css_selector?: string;
    backend_node_id?: string;
    x?: number;
    y?: number;
  }): Promise<ActionResult> {
    // 0. Strip href="#" before clicking — prevents URL hash change that breaks
    //    page state tracking and BrowserGym reward evaluation.
    if (params.css_selector) {
      try {
        await this.page.evaluate(`(() => {
          const el = document.querySelector(${JSON.stringify(params.css_selector)});
          if (el && el.getAttribute('href') === '#') el.removeAttribute('href');
        })()`);
      } catch { /* best-effort */ }
    }

    // 1. Link with href → navigate directly (100% reliable, no overlay issues)
    if (params.href && params.href.startsWith("http")) {
      try {
        await this.page.goto(params.href, { waitUntil: "domcontentloaded", timeout: 15000 });
        return { success: true, data: { method: "href-navigate" } };
      } catch (e) {
        // Navigation failed — try other methods
      }
    }

    // 2. CSS selector → Playwright locator.click() (handles overlays, auto-scrolls)
    if (params.css_selector) {
      try {
        const locator = this.page.locator(params.css_selector).first();
        await locator.click({ timeout: 5000 });

        // Post-click: fire JS .click() for jQuery/custom event handlers.
        // Playwright's click dispatches native MouseEvent but not jQuery's
        // delegated handlers (.on('click')) or custom event systems.
        // This is needed for real websites too (WordPress, legacy SPAs).
        await this.postClickJsFallback(params.css_selector);

        return { success: true, data: { method: "locator-click" } };
      } catch {
        // Locator failed — try JS click
        try {
          const clicked = await this.page.evaluate(`(() => {
            const el = document.querySelector(${JSON.stringify(params.css_selector)});
            if (el) { el.scrollIntoView({block:'center'}); el.click(); return true; }
            return false;
          })()`);
          if (clicked) {
            return { success: true, data: { method: "js-click" } };
          }
          // Element not found via JS — fall through to backend_node_id / coordinates
        } catch {
          // JS click also failed — fall through
        }
      }
    }

    // 3. Backend node ID → requires CDP (handled in executeCdpAction fallback)
    if (params.backend_node_id) {
      // ActionHandler doesn't have CDP access — return failure to trigger
      // the CDP fallback in BrowserAdapter.executeAction()
      return { success: false, error: "backend_node_id requires CDP fallback" };
    }

    // 4. Coordinate click (last resort)
    if (typeof params.x === "number" && typeof params.y === "number") {
      return this.clickAt(params.x, params.y);
    }

    return { success: false, error: "No targeting method available" };
  }

  /** Drag from one coordinate to another via Playwright mouse API.
   *  Falls back to synthetic HTML5 DragEvents for jQuery UI / native drag. */
  private async drag(
    fromX: number, fromY: number, toX: number, toY: number,
  ): Promise<ActionResult> {
    try {
      // Phase 1: Playwright mouse-based drag with smooth interpolation
      await this.page.mouse.move(fromX, fromY);
      await this.page.mouse.down();
      const steps = 10;
      for (let i = 1; i <= steps; i++) {
        const x = fromX + (toX - fromX) * (i / steps);
        const y = fromY + (toY - fromY) * (i / steps);
        await this.page.mouse.move(x, y);
        await new Promise(r => setTimeout(r, 20)); // 20ms between moves for frameworks tracking mousemove
      }
      await this.page.mouse.up();

      // Phase 2: Fire synthetic HTML5 DragEvents for apps using HTML5 Drag API / jQuery UI
      await this.page.evaluate(([fx, fy, tx, ty]) => {
        const source = document.elementFromPoint(fx, fy);
        const target = document.elementFromPoint(tx, ty);
        if (!source || !target || source === target) return;

        const dt = new DataTransfer();
        const opts = (el: Element, clientX: number, clientY: number) => ({
          bubbles: true, cancelable: true, clientX, clientY, dataTransfer: dt,
          screenX: clientX, screenY: clientY,
        });

        source.dispatchEvent(new DragEvent("dragstart", opts(source, fx, fy)));
        target.dispatchEvent(new DragEvent("dragenter", opts(target, tx, ty)));
        target.dispatchEvent(new DragEvent("dragover", opts(target, tx, ty)));
        target.dispatchEvent(new DragEvent("drop", opts(target, tx, ty)));
        source.dispatchEvent(new DragEvent("dragend", opts(source, fx, fy)));
      }, [fromX, fromY, toX, toY] as [number, number, number, number]);

      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  /** Select text by clicking and dragging from one coordinate to another. */
  private async textSelect(
    fromX: number, fromY: number, toX: number, toY: number,
  ): Promise<ActionResult> {
    try {
      await this.page.mouse.click(fromX, fromY);
      await this.page.mouse.down();
      await this.page.mouse.move(toX, toY);
      await this.page.mouse.up();
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  /** Double-click at exact coordinates. */
  private async doubleClickAt(x: number, y: number): Promise<ActionResult> {
    try {
      await this.page.mouse.dblclick(x, y);
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  /** Double-click on a selector. */
  private async doubleClick(selector: string): Promise<ActionResult> {
    try {
      await this.page.locator(selector).first().dblclick({ timeout: 5000 });
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  /** Hover at exact coordinates. */
  private async hoverAt(x: number, y: number): Promise<ActionResult> {
    try {
      await this.page.mouse.move(x, y);
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  /**
   * Click to focus at coordinates, optionally clear existing text, then type.
   * Uses keyboard.type() which dispatches proper keyDown/keyUp events
   * that React/Vue comboboxes need.
   */
  private async typeAt(
    x: number,
    y: number,
    text: string,
    clearFirst?: boolean,
  ): Promise<ActionResult> {
    try {
      // Click to focus
      await this.page.mouse.click(x, y);
      await this.page.waitForTimeout(150);

      // Clear existing text if requested
      if (clearFirst !== false) {
        const modifier = process.platform === "darwin" ? "Meta" : "Control";
        await this.page.keyboard.press(`${modifier}+a`);
        await this.page.keyboard.press("Backspace");
        await this.page.waitForTimeout(50);
      }

      // Type character by character with realistic delays
      await this.page.keyboard.type(text, { delay: 30 });

      // Wait for autocomplete dropdowns to appear
      await this.page.waitForTimeout(300);
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  /**
   * Scroll by pixel amount — container-aware.
   * If the page has an inner scrollable container (overflow-y: scroll/auto with
   * content exceeding its height), scroll THAT container instead of the page root.
   * This handles Gmail, Slack, email lists, and other panel-based UIs.
   */
  private async scrollBy(dx: number, dy: number): Promise<ActionResult> {
    try {
      // First try: find and scroll inner scrollable container
      const scrolledInner = await this.page.evaluate(`((dy) => {
        const containers = document.querySelectorAll('*');
        for (const el of containers) {
          const style = window.getComputedStyle(el);
          const overflowY = style.overflowY;
          if ((overflowY === 'scroll' || overflowY === 'auto') &&
              el.scrollHeight > el.clientHeight + 10 &&
              el.clientHeight > 50 && el.clientHeight < window.innerHeight * 0.9 &&
              el !== document.body && el !== document.documentElement) {
            el.scrollBy(0, dy);
            return true;
          }
        }
        return false;
      })(${dy})`);

      if (scrolledInner) {
        return { success: true, data: { method: "container-scroll" } };
      }

      // Fallback: scroll the page root
      await this.page.mouse.wheel(dx, dy);
      return { success: true, data: { method: "page-scroll" } };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  /** Press a key combination (e.g., ["Control", "a"]). */
  private async keyCombo(keys: string[]): Promise<ActionResult> {
    try {
      // Build Playwright key combo string: "Control+Shift+a"
      const combo = keys.join("+");
      await this.page.keyboard.press(combo);
      return { success: true };
    } catch (error) {
      return { success: false, error: String(error) };
    }
  }

  /**
   * Post-click JS fallback — fires native el.click() on elements where
   * Playwright's mouse-event-based click doesn't trigger jQuery or custom
   * event handlers. Targets:
   * - Empty spans/icons (common in icon-button UIs)
   * - List items with action classes (menu items)
   * - Links with href="#" (jQuery SPA patterns)
   * - Hidden/collapsed elements (menus revealed by prior click)
   */
  private async postClickJsFallback(selector: string): Promise<void> {
    try {
      await this.page.evaluate(`((sel) => {
        const el = document.querySelector(sel);
        if (!el) return;
        const tag = el.tagName.toLowerCase();
        const text = (el.textContent || '').trim();
        const classes = el.className || '';
        const href = el.getAttribute('href');

        // Fire JS .click() if the element looks like it needs it:
        const needsJsClick =
          // Empty span/icon with CSS classes (icon buttons)
          (tag === 'span' && text.length === 0 && classes.length > 0) ||
          // List item with action classes (menu items)
          (tag === 'li' && classes.length > 0) ||
          // Link with href="#" (jQuery SPA)
          (tag === 'a' && href === '#') ||
          // Any element that was hidden (display:none or visibility:hidden)
          (el.offsetParent === null && el.offsetHeight === 0);

        if (needsJsClick) {
          el.click();
        }
      })(${JSON.stringify(selector)})`);
    } catch { /* best-effort, don't fail the main click */ }
  }

  /**
   * Dismiss cookie consent banners using common patterns.
   * Tries multiple selectors in priority order. No-ops if no banner found.
   */
  async dismissCookieConsent(): Promise<ActionResult> {
    // Quick check: look for VISIBLE consent-related elements only (~10ms).
    // Checking raw HTML catches false positives (scripts, comments mentioning "cookie").
    try {
      const hasVisibleConsent = await this.page.evaluate(`(() => {
        // Check for known CMP containers that are visible
        const cmpSelectors = [
          '#onetrust-banner-sdk', '#onetrust-consent-sdk',
          '#CybotCookiebotDialog', '.cky-consent-container',
          '[id^="sp_message"]', '.fc-consent-root',
          '#didomi-host', '.qc-cmp2-container',
          '[class*="cookie-banner"]', '[class*="cookie-consent"]',
          '[id*="cookie-banner"]', '[id*="cookie-consent"]',
          '[class*="gdpr"]', '[id*="gdpr"]',
        ];
        for (const sel of cmpSelectors) {
          const el = document.querySelector(sel);
          if (el && el.offsetParent !== null) return true;
        }
        // Check for consent iframes (OneTrust, Sourcepoint use iframes)
        const iframeSelectors = ['iframe[id^="sp_message"]', 'iframe[title*="consent" i]', 'iframe[title*="privacy" i]', 'iframe[id*="consent"]'];
        for (const sel of iframeSelectors) {
          if (document.querySelector(sel)) return true;
        }
        // Check for visible buttons with consent-related text
        const buttons = document.querySelectorAll('button, a[role="button"]');
        for (const btn of buttons) {
          const text = (btn.textContent || '').toLowerCase().trim();
          if (text.length < 30 && btn.offsetParent !== null &&
              (text.includes('accept') || text.includes('agree') || text.includes('consent'))) {
            return true;
          }
        }
        return false;
      })()`);
      if (!hasVisibleConsent) {
        return { success: false, data: { method: "skip", reason: "no visible consent elements" } };
      }
    } catch { /* continue with full detection if quick check fails */ }

    // Phase 1: Try CSS selectors on main page (fast — ~500ms total)
    const selectors = [
      // Named CMP platforms (most reliable)
      '#onetrust-accept-btn-handler',                    // OneTrust
      '#didomi-notice-agree-button',                     // Didomi
      '#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll', // CookieBot
      '.sp_choice_type_11',                              // Sourcepoint
      '.fc-cta-consent',                                 // FundingChoices
      '[data-cookiefirst-action="accept"]',              // CookieFirst
      '.qc-cmp2-summary-buttons button[mode="primary"]', // Quantcast
      // Booking.com consent + sign-in overlays
      '#onetrust-accept-btn-handler',                      // Booking uses OneTrust sometimes
      'button[id*="accept"]',                              // Booking cookie accept
      '[data-testid="web-shell-header-mfe-close-btn"]',    // Booking sign-in popup close
      '[aria-label="Dismiss sign-in info."]',              // Booking sign-in dismiss
      'button[aria-label*="Dismiss" i]',                   // Booking generic dismiss
      '[class*="bui-modal"] button[class*="close"]',       // Booking modal close
      // Generic cookie/consent selectors
      '[id*="cookie"] button[id*="accept"]',
      '[class*="cookie"] button[class*="accept"]',
      '[id*="consent"] button[id*="accept"]',
      '[class*="consent"] button[class*="accept"]',
      '[data-testid*="cookie-accept"]',
      '[data-testid*="accept-cookies"]',
      'button[aria-label*="accept" i]',
      'button[aria-label*="agree" i]',
      '.cc-accept', '.cc-btn.cc-dismiss',
    ];

    for (const selector of selectors) {
      try {
        const el = this.page.locator(selector).first();
        if (await el.isVisible({ timeout: 300 })) {
          await el.click({ timeout: 1500 });
          await this.page.waitForTimeout(500);
          return { success: true, data: { method: "css", selector } };
        }
      } catch { /* try next */ }
    }

    // Phase 2: Sourcepoint / IAB CMP iframes (TechCrunch, many news sites)
    // These use cross-origin iframes that CSS selectors can't reach.
    // Playwright's frameLocator can pierce them.
    const iframeSelectors = [
      '[id^="sp_message_iframe"]',     // Sourcepoint
      'iframe[title*="consent" i]',    // Generic consent iframe
      'iframe[title*="privacy" i]',    // Privacy iframe
      'iframe[id*="consent"]',         // ID-based
    ];
    for (const frameSel of iframeSelectors) {
      try {
        const frame = this.page.frameLocator(frameSel).first();
        // Try accept buttons inside the iframe
        const acceptBtns = [
          'button[title="Accept All"]', 'button[title="Accept all"]',
          'button[title="ACCEPT ALL"]', 'button[title="OK"]',
          'button.sp_choice_type_11', 'button.sp_choice_type_ACCEPT_ALL',
          'button[aria-label="Accept"]', 'button[aria-label="Accept all"]',
        ];
        for (const btnSel of acceptBtns) {
          try {
            const btn = frame.locator(btnSel).first();
            await btn.click({ timeout: 2000 });
            await this.page.waitForTimeout(500);
            return { success: true, data: { method: "iframe", frameSel, btnSel } };
          } catch { /* try next button */ }
        }
        // Try close button as fallback
        try {
          const closeBtn = frame.locator('button[title="Close"], button[aria-label="Close"]').first();
          await closeBtn.click({ timeout: 1500 });
          await this.page.waitForTimeout(500);
          return { success: true, data: { method: "iframe-close", frameSel } };
        } catch { /* no close button */ }
      } catch { /* frame not found */ }
    }

    // Phase 3: JS text-based fallback — find buttons by visible text
    try {
      const dismissed = await this.page.evaluate(`(() => {
        const buttons = document.querySelectorAll('button, a[role="button"], [role="button"]');
        const acceptWords = ['accept all', 'accept', 'agree', 'i agree', 'got it', 'allow all', 'ok', 'dismiss', 'no thanks', 'not now', 'close', 'skip'];
        for (const btn of buttons) {
          const text = (btn.textContent || '').toLowerCase().trim();
          if (text.length < 30 && acceptWords.some(w => text === w || text.startsWith(w)) && btn.offsetParent !== null) {
            btn.click();
            return text;
          }
        }
        return null;
      })()`);
      if (dismissed) {
        await this.page.waitForTimeout(500);
        return { success: true, data: { method: "text", text: dismissed } };
      }
    } catch { /* best effort */ }

    // Phase 4: Nuclear — hide overlay elements blocking the page
    try {
      const removed = await this.page.evaluate(`(() => {
        const overlaySelectors = [
          '[class*="consent"]', '[class*="cookie-banner"]', '[id*="consent"]',
          '[id*="cookie"]', '.qc-cmp2-container', '[id^="sp_message"]',
          '.fc-consent-root', '[class*="gdpr"]',
          // Booking.com overlays
          '[class*="bui-modal"]', '[class*="signup-modal"]',
          '[data-testid*="dismissible-overlay"]',
          '[role="dialog"][aria-modal="true"]',
        ];
        let count = 0;
        for (const sel of overlaySelectors) {
          for (const el of document.querySelectorAll(sel)) {
            if (el.offsetParent !== null) {
              el.style.display = 'none';
              count++;
            }
          }
        }
        if (count > 0) {
          document.body.style.overflow = '';
          document.documentElement.style.overflow = '';
        }
        return count;
      })()`);
      if (removed) {
        return { success: true, data: { method: "remove-overlay", removed } };
      }
    } catch { /* best effort */ }

    return { success: true, data: { noBannerFound: true } };
  }
}
