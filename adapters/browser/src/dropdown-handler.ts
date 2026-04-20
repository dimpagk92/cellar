/**
 * Dropdown/Combobox Intelligence — handles native <select> elements and
 * ARIA combobox/listbox patterns for option discovery and selection.
 *
 * License: MIT
 */

import type { Evaluator, RawDOMElement } from "./dom-extractor.js";

/** A single option within a dropdown or listbox. */
export interface DropdownOption {
  value: string;
  text: string;
  index: number;
  selected: boolean;
}

export class DropdownHandler {
  /** Get options from a native <select> element. */
  async getSelectOptions(
    evaluator: Evaluator,
    elementId: string,
  ): Promise<DropdownOption[]> {
    const script = `(() => {
      const el = document.getElementById(${JSON.stringify(elementId.replace(/^dom:/, ""))})
        || document.querySelector('[data-cel-id="${elementId}"]');
      if (!el || el.tagName !== 'SELECT') return [];
      const options = [];
      for (let i = 0; i < el.options.length; i++) {
        const opt = el.options[i];
        options.push({
          value: opt.value,
          text: opt.textContent.trim(),
          index: i,
          selected: opt.selected,
        });
      }
      return options;
    })()`;

    try {
      return await (evaluator as any).evaluate(script) as DropdownOption[];
    } catch {
      return [];
    }
  }

  /** Select an option in a native <select> by value or text. */
  async selectOption(
    evaluator: Evaluator,
    elementId: string,
    valueOrText: string,
  ): Promise<boolean> {
    const script = `(() => {
      const el = document.getElementById(${JSON.stringify(elementId.replace(/^dom:/, ""))})
        || document.querySelector('[data-cel-id="${elementId}"]');
      if (!el || el.tagName !== 'SELECT') return false;
      const target = ${JSON.stringify(valueOrText)};
      for (let i = 0; i < el.options.length; i++) {
        const opt = el.options[i];
        if (opt.value === target || opt.textContent.trim() === target) {
          el.selectedIndex = i;
          el.dispatchEvent(new Event('change', { bubbles: true }));
          el.dispatchEvent(new Event('input', { bubbles: true }));
          return true;
        }
      }
      return false;
    })()`;

    try {
      return await (evaluator as any).evaluate(script) as boolean;
    } catch {
      return false;
    }
  }

  /** Get options from an ARIA combobox/listbox. */
  async getAriaOptions(
    evaluator: Evaluator,
    elementId: string,
  ): Promise<DropdownOption[]> {
    const script = `(() => {
      const el = document.getElementById(${JSON.stringify(elementId.replace(/^dom:/, ""))})
        || document.querySelector('[data-cel-id="${elementId}"]');
      if (!el) return [];

      // Find the associated listbox — via aria-controls or sibling/descendant
      let listbox = null;
      const controlsId = el.getAttribute('aria-controls') || el.getAttribute('aria-owns');
      if (controlsId) {
        listbox = document.getElementById(controlsId);
      }
      if (!listbox) {
        // Search siblings and descendants for a listbox role
        const parent = el.parentElement;
        if (parent) {
          listbox = parent.querySelector('[role="listbox"], [role="menu"]');
        }
      }
      if (!listbox) return [];

      const items = listbox.querySelectorAll('[role="option"], [role="menuitem"]');
      const options = [];
      for (let i = 0; i < items.length; i++) {
        const item = items[i];
        options.push({
          value: item.getAttribute('data-value') || item.id || '',
          text: (item.textContent || '').trim(),
          index: i,
          selected: item.getAttribute('aria-selected') === 'true',
        });
      }
      return options;
    })()`;

    try {
      return await (evaluator as any).evaluate(script) as DropdownOption[];
    } catch {
      return [];
    }
  }

  /** Detect if an element is an autocomplete field. */
  isAutocomplete(element: RawDOMElement): boolean {
    // Check HTML autocomplete attribute
    if (element.attributes["autocomplete"] && element.attributes["autocomplete"] !== "off") {
      return true;
    }
    // Check ARIA combobox with autocomplete
    if (element.role === "combobox") {
      const ariaAutocomplete = element.attributes["aria-autocomplete"];
      if (ariaAutocomplete && ariaAutocomplete !== "none") {
        return true;
      }
    }
    // Check for common autocomplete data attributes
    if (element.attributes["data-autocomplete"] || element.attributes["data-typeahead"]) {
      return true;
    }
    return false;
  }
}
