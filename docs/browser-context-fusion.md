# Browser Context Fusion: DOM + Vision + Accessibility

> How CEL combines three independent streams for browser automation — and why this beats the current state of the art.

## The Problem

Browser automation agents need to understand what's on screen. Current approaches fall into two camps:

1. **Vision-only** (Claude Computer Use) — screenshots + pixel coordinates. Simple but imprecise. No semantic understanding of elements.
2. **DOM-primary** (browser-use) — extract DOM tree, optionally attach screenshots. More precise but brittle, slow, and blind to visual-only content.

Neither fuses multiple independent data sources with confidence scoring. CEL does.

---

## Competitive Analysis: browser-use

[browser-use](https://github.com/browser-use/browser-use) (79K+ GitHub stars, MIT) is the leading open-source browser automation agent. It connects LLMs to Chromium via Chrome DevTools Protocol (CDP) and uses a five-stage DOM extraction pipeline.

### What They Do Well

**Parallel CDP data collection** — five requests fire simultaneously:
- `DOM.getDocument` (full DOM tree)
- `Accessibility.getFullAXTree` (semantic accessibility data)
- `DOMSnapshot.captureSnapshot` (paint order / visual hierarchy)
- `Page.getLayoutMetrics` (viewport dimensions, scroll position)
- `Runtime.evaluate` (JavaScript event listener detection)

**Smart filtering** — reduces ~10,000 DOM nodes to ~200 interactive elements via:
- Simplification: strips non-interactive elements (plain divs, spans without listeners)
- Paint-order filtering: uses z-index/stacking context to remove elements occluded by overlays (prevents clicking behind modals)
- Bounding-box filtering: when nested elements are both interactive, keeps only the inner target
- Sequential index assignment in depth-first order

**DOM-primary, vision-supplementary** — DOM provides structure and semantics. Screenshots are annotated with bounding boxes around interactive elements and sent alongside the serialized DOM. The LLM receives both.

**Element interaction** — elements are numbered sequentially. The LLM references them by index. A `DOMSelectorMap` translates indices back to CSS selectors for Playwright execution.

### Where browser-use Fails

#### 1. Shadow DOM is Broken

XPath cannot traverse shadow DOM boundaries. iframe elements return `frame_id: None`. This is a [known issue (#3820)](https://github.com/browser-use/browser-use/issues/3820) with no fix.

**Impact:** Modern web components (Salesforce Lightning, Shopify, any app using Web Components) are partially or fully invisible to the agent. Shadow DOM adoption is accelerating — this gap will only widen.

#### 2. Extraction is Slow

The DOM extraction pipeline takes 5-6 seconds on Amazon product pages and 30+ seconds on complex sites like Booking.com ([#2808](https://github.com/browser-use/browser-use/issues/2808), [#627](https://github.com/browser-use/browser-use/issues/627)). The `DOMWatchdog` frequently times out.

**Impact:** Each agent step incurs multi-second overhead before the LLM even starts thinking. Real-time automation (responding to toasts, handling timeouts, reacting to live data) is impossible at these speeds.

#### 3. Aggressive Filtering Loses Context

Reducing to ~200 elements works for simple pages but drops important content on information-dense UIs. Non-interactive elements that provide context (labels, status indicators, data cells) get stripped.

**Impact:** The LLM makes decisions with incomplete information. A table header explaining what column values mean gets filtered out. A status badge next to a button gets dropped.

#### 4. Full Re-extraction Every Step

The entire DOM tree is re-extracted on every action step. There's no diffing, no incremental updates, no awareness of what actually changed.

**Impact:** Multiplies the extraction latency problem. A 10-step workflow on a complex page spends 50-300 seconds just on DOM extraction.

#### 5. No Network Awareness

browser-use has zero visibility into network activity. It cannot see:
- Whether a form submission returned 200 or 422
- If an XHR request is in-flight (loading state)
- API error responses that explain why a UI action failed
- Redirect chains during authentication flows

**Impact:** The agent has to infer success/failure purely from DOM changes, which may lag behind the actual network response or not reflect errors at all.

#### 6. Binary Vision Toggle

Vision is either always on, always off, or "auto" (LLM decides per step). There's no element-level confidence-driven escalation. When vision is on, it captures full screenshots regardless of whether the DOM already provided sufficient context.

**Impact:** Either wastes API calls (vision always on) or misses visual-only content (vision off). No middle ground for targeted verification of low-confidence elements.

#### 7. Prompt Injection Surface

Extracted DOM text content is inserted directly into the LLM prompt. Malicious pages can embed instructions in hidden elements, invisible text, or data attributes.

**Impact:** Researchers have demonstrated credential exfiltration and domain validation bypass through malicious web content injected into the agent's context.

#### 8. Browser-Native UI is Invisible

Anything rendered by the browser itself (PDF viewers, print dialogs, HTTP auth popups, download prompts, permission requests) doesn't exist in the page DOM.

**Impact:** The agent cannot interact with save-as dialogs, certificate warnings, or browser-level authentication prompts — common steps in real workflows.

#### 9. Write Tasks Are Unreliable

Benchmarks (89.1% on WebVoyager) focus on read-heavy tasks across 15 websites. Authentication flows, multi-step form filling, and file downloads have significantly lower success rates in the real world.

**Impact:** The benchmark score overstates production reliability. Tasks that involve state mutation — the ones businesses actually need automated — are where agents fail most.

#### 10. Chromium-Only

Requires Chrome DevTools Protocol. No Firefox, no Safari, no Electron apps, no WebView-based desktop applications.

**Impact:** Cannot automate internal tools built on non-Chromium platforms or test cross-browser workflows.

---

## CEL's Approach: Three Independent Streams

CEL treats DOM, OS-level accessibility, and vision as **independent data sources** with separate confidence scores, fused in the context merger.

```
┌─────────────┐  ┌──────────────────┐  ┌─────────────┐
│  DOM (CDP)  │  │  Accessibility    │  │   Vision    │
│             │  │  (AT-SPI2 / AX)  │  │  (LLM-based)│
│ source:     │  │  source:          │  │  source:    │
│ NativeApi   │  │ AccessibilityTree │  │  Vision     │
│ conf: 0.95  │  │  conf: 0.60-0.90 │  │ conf: 0.50+ │
└──────┬──────┘  └────────┬─────────┘  └──────┬──────┘
       │                  │                    │
       └──────────┬───────┘                    │
                  │                            │
          ┌───────▼────────┐                   │
          │ Context Merger │◄──────────────────┘
          │ (merge.rs)     │   (escalation only)
          │                │
          │ • IoU dedup    │
          │ • Cross-source │
          │   confirmation │
          │ • Confidence   │
          │   boosting     │
          └───────┬────────┘
                  │
          ┌───────▼────────┐
          │ ScreenContext  │
          │ (sorted by     │
          │  confidence)   │
          └────────────────┘
```

### Advantage 1: Shadow DOM via Accessibility Bridge

Where browser-use's XPath breaks on shadow boundaries, **OS-level accessibility trees traverse shadow DOM natively**. Browsers expose shadow content to AT-SPI2 (Linux), AXUIElement (macOS), and UIA (Windows) as part of the accessibility tree — the shadow boundary is a DOM concept, not an accessibility concept.

CEL uses accessibility as the primary source for shadow DOM elements, with DOM as supplementary where available.

### Advantage 2: Incremental Context Updates

Instead of re-extracting the entire DOM every step:

- **DOM stream:** `MutationObserver` reports diffs (added/removed/changed nodes)
- **Accessibility stream:** AT-SPI2 / AX event listeners fire on state changes
- **Vision stream:** only re-triggered when viewport actually changes or confidence is low

Target: **sub-100ms context updates** vs browser-use's 5-30 second full extractions.

### Advantage 3: Confidence-Driven Vision Escalation

No binary toggle. The merger decides per-element:

- High-confidence DOM + a11y match (>0.85): skip vision entirely
- Medium-confidence (0.50-0.85): targeted crop around the element, verify with vision
- Low-confidence or missing from DOM/a11y: full vision fallback

Result: **80-90% fewer vision API calls** while maintaining (or exceeding) accuracy on edge cases.

### Advantage 4: Network as Context Stream

`cel-network` monitors traffic. The agent knows:
- A POST to `/api/submit` just returned 422 with `{"error": "email_taken"}`
- An XHR to `/api/data` is in-flight (explains why the table is showing a spinner)
- A 302 redirect chain is happening during OAuth

This is context that DOM and vision literally cannot provide.

### Advantage 5: Cross-Source Confidence Boosting

When multiple independent sources agree, confidence increases:

```
DOM says: <button> at (100, 200, 80, 32) labeled "Submit"     → 0.95
A11y says: Button role at (100, 200, 80, 32) name "Submit"    → 0.85
                                                         Merged → 0.98
```

When sources disagree, the conflict itself is signal:

```
DOM says: <div> at (100, 200) with click handler                → 0.90
A11y says: nothing at that position                             → gap
Vision says: looks like a button labeled "Continue"             → 0.70
                                              Merged → 0.80 (flagged)
```

### Advantage 6: Sanitized Context Construction

DOM content doesn't go raw into the LLM prompt. The context assembly pipeline:
- Strips `<script>` content and event handler strings
- Truncates text content per element (prevents single element flooding context)
- Flags suspicious patterns (hidden text, zero-size elements with content)
- Structures output as typed `ContextElement` objects, not raw HTML

---

## Implementation Requirements

To realize the browser adapter:

1. **CDP connection** — Playwright or direct CDP WebSocket
2. **DOM → ContextElement mapper** — interactive elements extracted with bounds, type, value, actions
3. **MutationObserver injection** — incremental DOM diffs instead of full re-extraction
4. **Frame/shadow traversal** — enumerate all frames, flatten shadow trees into unified context
5. **Merge with existing a11y stream** — DOM elements get `source: NativeApi` (0.95 confidence), fused with AT-SPI2/AX elements in the context merger

The context fusion engine (`cel-context/src/merge.rs`) already handles multi-source merging, IoU deduplication, and confidence scoring. The browser adapter is a new source, not a new architecture.

---

## Summary

| Capability | browser-use | CEL |
|-----------|-------------|-----|
| DOM extraction | Full tree, 5 CDP calls | Full tree + MutationObserver diffs |
| Accessibility | Merged inside DOM pipeline | Independent stream, OS-level |
| Vision | Binary toggle (on/off/auto) | Per-element confidence escalation |
| Shadow DOM | Broken (XPath limitation) | Traversed via accessibility bridge |
| Extraction speed | 5-30 seconds per step | Sub-100ms incremental updates |
| Network awareness | None | Full request/response visibility |
| Context security | Raw DOM text in prompt | Sanitized, structured, truncated |
| Platform support | Chromium only | Any browser (via OS accessibility) |
| Confidence scoring | None (all-or-nothing filtering) | Per-element, per-source, with fusion |
