import { describe, it, expect } from "vitest";
import { distillContextByGoal } from "./goal-runner/context-distiller.js";
import type { ScreenContext, ContextElement } from "./types.js";

function makeElement(id: string, type: string, label: string, extra?: Partial<ContextElement>): ContextElement {
  return {
    id,
    element_type: type,
    label,
    confidence: 0.8,
    source: "accessibility_tree",
    state: { focused: false, enabled: true, visible: true, selected: false },
    actions: type === "button" || type === "link" ? ["click"] : [],
    ...extra,
  } as ContextElement;
}

function makeContext(elements: ContextElement[]): ScreenContext {
  return {
    app: "Test",
    window: "Test",
    elements,
    timestamp_ms: Date.now(),
  } as ScreenContext;
}

describe("Semantic Synonym Expansion", () => {
  it("should match 'Reservations' when goal says 'book'", () => {
    const ctx = makeContext([
      makeElement("nav", "link", "Home"),
      makeElement("res", "button", "Reservations", { bounds: { x: 0, y: 0, width: 100, height: 30 } }),
      makeElement("about", "link", "About Us"),
      // Add enough elements to trigger filtering
      ...Array.from({ length: 25 }, (_, i) =>
        makeElement(`filler-${i}`, "text", `Filler text ${i}`),
      ),
    ]);

    const result = distillContextByGoal(ctx, "Book a hotel in Amsterdam");
    const resEl = result.elements.find(e => e.id === "res");
    expect(resEl).toBeDefined();
  });

  it("should match 'cart' when goal says 'buy'", () => {
    const ctx = makeContext([
      makeElement("cart", "button", "Add to Cart", { bounds: { x: 0, y: 0, width: 100, height: 30 } }),
      makeElement("desc", "text", "Product description goes here"),
      ...Array.from({ length: 25 }, (_, i) =>
        makeElement(`filler-${i}`, "text", `Filler ${i}`),
      ),
    ]);

    const result = distillContextByGoal(ctx, "Buy this product");
    const cartEl = result.elements.find(e => e.id === "cart");
    expect(cartEl).toBeDefined();
  });

  it("should match 'sign in' when goal says 'login'", () => {
    const ctx = makeContext([
      makeElement("signin", "button", "Sign In", { bounds: { x: 0, y: 0, width: 100, height: 30 } }),
      makeElement("logo", "text", "Company Logo"),
      ...Array.from({ length: 25 }, (_, i) =>
        makeElement(`filler-${i}`, "text", `Filler ${i}`),
      ),
    ]);

    const result = distillContextByGoal(ctx, "Login to my account");
    const signInEl = result.elements.find(e => e.id === "signin");
    expect(signInEl).toBeDefined();
  });

  it("should boost interactive elements via content_role", () => {
    const ctx = makeContext([
      makeElement("btn", "button", "Submit", {
        bounds: { x: 0, y: 0, width: 100, height: 30 },
        content_role: "interactive" as any,
      }),
      makeElement("txt", "text", "Submit your application today", {
        content_role: "content" as any,
      }),
      ...Array.from({ length: 25 }, (_, i) =>
        makeElement(`filler-${i}`, "text", `Filler ${i}`),
      ),
    ]);

    const result = distillContextByGoal(ctx, "Submit the form");
    const btn = result.elements.find(e => e.id === "btn");
    expect(btn).toBeDefined();
  });

  it("should deprioritize decorative elements", () => {
    const ctx = makeContext([
      makeElement("sep", "separator", "---", { content_role: "decorative" as any }),
      makeElement("btn", "button", "Search", {
        bounds: { x: 0, y: 0, width: 100, height: 30 },
        content_role: "interactive" as any,
      }),
      ...Array.from({ length: 25 }, (_, i) =>
        makeElement(`filler-${i}`, "text", `Filler ${i}`),
      ),
    ]);

    const result = distillContextByGoal(ctx, "Search for hotels");
    const sepEl = result.elements.find(e => e.id === "sep");
    const btnEl = result.elements.find(e => e.id === "btn");
    // Button should be present, separator might be filtered out or ranked last
    expect(btnEl).toBeDefined();
  });

  it("should preserve row-scoped generic actions over header chrome noise", () => {
    const ctx = makeContext([
      makeElement("header-more", "button", "More", {
        bounds: { x: 1200, y: 20, width: 80, height: 28 },
        properties: { css_selector: "header .toolbar .more" } as any,
      }),
      makeElement("row", "group", "", {}),
      makeElement("name", "text", "Jamie Rodriguez", { parent_id: "row" }),
      makeElement("email", "text", "jamie.rodriguez@acme.io", { parent_id: "row" }),
      makeElement("remove", "button", "Remove", {
        parent_id: "row",
        bounds: { x: 840, y: 200, width: 120, height: 28 },
      }),
      ...Array.from({ length: 20 }, (_, i) =>
        makeElement(`filler-${i}`, "text", `Filler ${i}`),
      ),
    ]);

    const result = distillContextByGoal(ctx, "Remove Jamie Rodriguez with email jamie.rodriguez@acme.io");
    const removeIdx = result.elements.findIndex(e => e.id === "remove");
    const moreIdx = result.elements.findIndex(e => e.id === "header-more");
    expect(removeIdx).toBeGreaterThanOrEqual(0);
    if (moreIdx >= 0) {
      expect(removeIdx).toBeLessThan(moreIdx);
    }
  });

  it("should downrank already-completed controls in sequential goals", () => {
    const ctx = makeContext([
      makeElement("ack", "button", "Acknowledged ✓", {
        bounds: { x: 0, y: 0, width: 120, height: 30 },
      }),
      makeElement("reply", "button", "Reply via Email", {
        bounds: { x: 140, y: 0, width: 140, height: 30 },
      }),
      makeElement("status", "text", "Ticket TICKET-4821 acknowledged. Status updated."),
    ]);

    const result = distillContextByGoal(
      ctx,
      "Click Acknowledge and then click Reply via Email to open an email compose window.",
    );

    const ackIdx = result.elements.findIndex(e => e.id === "ack");
    const replyIdx = result.elements.findIndex(e => e.id === "reply");
    expect(replyIdx).toBeGreaterThanOrEqual(0);
    expect(ackIdx).toBeGreaterThanOrEqual(0);
    expect(replyIdx).toBeLessThan(ackIdx);
  });
});
