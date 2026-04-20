import { describe, it, expect } from "vitest";
import { mapElements } from "../element-mapper.js";
import type { RawDOMElement } from "../dom-extractor.js";

function makeRaw(overrides: Partial<RawDOMElement> = {}): RawDOMElement {
  return {
    backendNodeId: 1,
    tag: "button",
    id: "",
    role: "button",
    ariaLabel: "Submit",
    ariaDescription: "",
    textContent: "Submit",
    value: "",
    type: "",
    href: "",
    placeholder: "",
    bounds: { x: 100, y: 200, width: 80, height: 32 },
    isVisible: true,
    isEnabled: true,
    isFocused: false,
    isChecked: null,
    isExpanded: null,
    isSelected: false,
    parentCelId: "",
    shadowDepth: 0,
    iframeOrigin: null,
    attributes: {},
    ...overrides,
  };
}

describe("element-mapper", () => {
  describe("element type mapping", () => {
    it("maps button role to button type", () => {
      const elements = mapElements([makeRaw({ role: "button" })]);
      expect(elements[0].element_type).toBe("button");
    });

    it("maps link role to link type", () => {
      const elements = mapElements([makeRaw({ tag: "a", role: "link", href: "/page" })]);
      expect(elements[0].element_type).toBe("link");
    });

    it("maps textbox role to input type", () => {
      const elements = mapElements([makeRaw({ tag: "input", role: "textbox", type: "text" })]);
      expect(elements[0].element_type).toBe("input");
    });

    it("maps input[type=submit] to button", () => {
      const elements = mapElements([makeRaw({ tag: "input", role: "", type: "submit" })]);
      expect(elements[0].element_type).toBe("button");
    });

    it("maps input[type=checkbox] to checkbox", () => {
      const elements = mapElements([makeRaw({ tag: "input", role: "", type: "checkbox" })]);
      expect(elements[0].element_type).toBe("checkbox");
    });

    it("maps select to combobox", () => {
      const elements = mapElements([makeRaw({ tag: "select", role: "" })]);
      expect(elements[0].element_type).toBe("combobox");
    });

    it("maps dialog to dialog", () => {
      const elements = mapElements([makeRaw({ tag: "dialog", role: "dialog" })]);
      expect(elements[0].element_type).toBe("dialog");
    });

    it("maps table to table", () => {
      const elements = mapElements([makeRaw({ tag: "table", role: "" })]);
      expect(elements[0].element_type).toBe("table");
    });

    it("maps heading tags to text", () => {
      const elements = mapElements([makeRaw({ tag: "h1", role: "" })]);
      expect(elements[0].element_type).toBe("text");
    });

    it("falls back to text for unknown tags", () => {
      const elements = mapElements([makeRaw({ tag: "div", role: "" })]);
      expect(elements[0].element_type).toBe("text");
    });

    it("ARIA role takes precedence over tag", () => {
      const elements = mapElements([makeRaw({ tag: "div", role: "button" })]);
      expect(elements[0].element_type).toBe("button");
    });
  });

  describe("confidence scoring", () => {
    it("gives max confidence to fully qualified elements", () => {
      const el = makeRaw({
        role: "button",
        ariaLabel: "Submit",
        bounds: { x: 0, y: 0, width: 100, height: 40 },
        isVisible: true,
        isEnabled: true,
        shadowDepth: 0,
        iframeOrigin: null,
      });
      const elements = mapElements([el]);
      // base 0.70 + label 0.08 + bounds 0.06 + visible+enabled 0.04 + actionable 0.04 + role 0.03 + main doc 0.03 = 0.98
      expect(elements[0].confidence).toBeCloseTo(0.98, 2);
    });

    it("gives base confidence to minimal elements", () => {
      const el = makeRaw({
        tag: "div",
        role: "",
        ariaLabel: "",
        textContent: "",
        bounds: null,
        isVisible: false,
        isEnabled: false,
        shadowDepth: 2,
        iframeOrigin: "https://other.com",
      });
      const elements = mapElements([el]);
      // base 0.70 only — no bonuses
      expect(elements[0].confidence).toBeCloseTo(0.7, 2);
    });

    it("reduces confidence for shadow DOM elements", () => {
      const mainDoc = makeRaw({ shadowDepth: 0, iframeOrigin: null });
      const shadow = makeRaw({ backendNodeId: 2, shadowDepth: 1, iframeOrigin: null });
      const elements = mapElements([mainDoc, shadow]);
      // Main doc gets +0.03 bonus, shadow does not
      expect(elements[0].confidence).toBeGreaterThan(elements[1].confidence);
    });

    it("reduces confidence for iframe elements", () => {
      const mainDoc = makeRaw({ iframeOrigin: null });
      const iframe = makeRaw({ backendNodeId: 2, iframeOrigin: "https://other.com" });
      const elements = mapElements([mainDoc, iframe]);
      expect(elements[0].confidence).toBeGreaterThan(elements[1].confidence);
    });
  });

  describe("label extraction", () => {
    it("prefers aria-label over textContent", () => {
      const el = makeRaw({ ariaLabel: "ARIA Label", textContent: "Text Content" });
      const elements = mapElements([el]);
      expect(elements[0].label).toBe("ARIA Label");
    });

    it("falls back to textContent when no aria-label", () => {
      const el = makeRaw({ ariaLabel: "", textContent: "Visible Text" });
      const elements = mapElements([el]);
      expect(elements[0].label).toBe("Visible Text");
    });

    it("uses placeholder for inputs without label", () => {
      const el = makeRaw({ tag: "input", ariaLabel: "", textContent: "", placeholder: "Enter email" });
      const elements = mapElements([el]);
      expect(elements[0].label).toBe("Enter email");
    });

    it("returns undefined when no label sources exist", () => {
      const el = makeRaw({ ariaLabel: "", textContent: "", placeholder: "", value: "" });
      const elements = mapElements([el]);
      expect(elements[0].label).toBeUndefined();
    });
  });

  describe("ID generation", () => {
    it("uses DOM id when available", () => {
      const el = makeRaw({ id: "submit-btn" });
      const elements = mapElements([el]);
      expect(elements[0].id).toBe("dom:submit-btn");
    });

    it("falls back to tag:nodeId when no DOM id", () => {
      const el = makeRaw({ id: "", backendNodeId: 42 });
      const elements = mapElements([el]);
      expect(elements[0].id).toBe("dom:button:42");
    });

    it("prefixes shadow DOM elements", () => {
      const el = makeRaw({ id: "", backendNodeId: 5, shadowDepth: 1, parentCelId: "dom:host" });
      const elements = mapElements([el]);
      expect(elements[0].id).toContain("shadow:");
    });

    it("ensures unique IDs across elements", () => {
      const elements = mapElements([
        makeRaw({ backendNodeId: 1, id: "btn-a" }),
        makeRaw({ backendNodeId: 2, id: "btn-b" }),
        makeRaw({ backendNodeId: 3, id: "" }),
      ]);
      const ids = elements.map((e) => e.id);
      expect(new Set(ids).size).toBe(ids.length);
    });
  });

  describe("state mapping", () => {
    it("maps focused state", () => {
      const el = makeRaw({ isFocused: true });
      const elements = mapElements([el]);
      expect(elements[0].state.focused).toBe(true);
    });

    it("maps disabled state", () => {
      const el = makeRaw({ isEnabled: false });
      const elements = mapElements([el]);
      expect(elements[0].state.enabled).toBe(false);
    });

    it("maps checked state for checkboxes", () => {
      const el = makeRaw({ tag: "input", type: "checkbox", isChecked: true });
      const elements = mapElements([el]);
      expect(elements[0].state.checked).toBe(true);
    });

    it("maps expanded state", () => {
      const el = makeRaw({ isExpanded: true });
      const elements = mapElements([el]);
      expect(elements[0].state.expanded).toBe(true);
    });
  });

  describe("actions", () => {
    it("assigns click/press to buttons", () => {
      const elements = mapElements([makeRaw({ role: "button" })]);
      expect(elements[0].actions).toEqual(["click", "press"]);
    });

    it("assigns activate/set to inputs", () => {
      const elements = mapElements([makeRaw({ tag: "input", role: "textbox", type: "text" })]);
      expect(elements[0].actions).toEqual(["activate", "set"]);
    });

    it("assigns click/jump to links", () => {
      const elements = mapElements([makeRaw({ tag: "a", role: "link", href: "/" })]);
      expect(elements[0].actions).toEqual(["click", "jump"]);
    });

    it("assigns toggle to checkboxes", () => {
      const elements = mapElements([makeRaw({ tag: "input", role: "", type: "checkbox" })]);
      expect(elements[0].actions).toEqual(["toggle"]);
    });
  });

  describe("source", () => {
    it("marks all elements as native_api", () => {
      const elements = mapElements([makeRaw()]);
      expect(elements[0].source).toBe("native_api");
    });
  });

  describe("sorting", () => {
    it("sorts by confidence descending", () => {
      const elements = mapElements([
        makeRaw({ backendNodeId: 1, ariaLabel: "", textContent: "", bounds: null, isVisible: false, isEnabled: false, role: "" }),
        makeRaw({ backendNodeId: 2, ariaLabel: "High", role: "button" }),
      ]);
      expect(elements[0].confidence).toBeGreaterThanOrEqual(elements[1].confidence);
    });
  });
});
