/**
 * OverlayDetector — structural detection and dismissal of blocking overlays
 * (cookie consent, notification prompts, paywalls, login walls, generic modals).
 *
 * Single canonical implementation that replaces five scattered ones:
 *   - cel-cortex/src/dialog.rs (label patterns, English-only)
 *   - agent/src/dialog-dismisser.ts (RegExp clone of the above)
 *   - agent/src/cdp-extractor.ts dismissCookieBanner (CDP + selectors)
 *   - benchmarks/src/standard/cookie-dismisser.ts (yet another bank)
 *   - action-handler.ts dismissCookieConsent (the most robust prior art)
 *
 * Detection flow:
 *   1. Detect — is there a blocking overlay? (DOM structure, CSS, ARIA)
 *   2. Classify — what kind? (cookie / notification / paywall / generic)
 *   3. Find dismiss targets — CMP API → CMP selectors → ARIA → position → text
 *   4. Dismiss — try each target in priority order; verify overlay is gone
 *
 * Language-independent: detection looks at structure (z-index, fixed position,
 * aria-modal, CMP fingerprints), not button text. Text matching is a last-resort
 * fallback only.
 */

import type { Page } from "playwright";

// ─── Public types ──────────────────────────────────────────────────────────

export type OverlayType =
  | "cookie_consent"
  | "notification_prompt"
  | "paywall"
  | "login_wall"
  | "generic";

export type DismissIntent = "reject" | "accept" | "close" | "dismiss" | "unknown";
export type DismissMethod =
  | "tcf_api"
  | "cmp_selector"
  | "aria"
  | "position"
  | "text_match";

export interface OverlayBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface DismissTarget {
  /** CSS selector that uniquely locates the dismiss element. */
  selector: string;
  /** Visible label, may be empty for icon-only buttons. */
  label: string;
  /** What the click is intended to do. */
  intent: DismissIntent;
  /** How we found this target. Higher in the list = more reliable. */
  method: DismissMethod;
  /** 0..1 — confidence the click will dismiss without granting consent. */
  confidence: number;
}

export interface BlockingOverlay {
  /** CSS selector of the overlay container. */
  containerSelector: string;
  /** Bounds of the overlay container in viewport coords. */
  bounds: OverlayBounds;
  /** Best guess at what kind of overlay this is. */
  type: OverlayType;
  /** 0..1 — how confident we are this is actually blocking content. */
  confidence: number;
  /** Detected CMP platform if any (onetrust, cookiebot, didomi, ...). */
  cmpPlatform: string | null;
  /** Whether the page exposes the IAB TCF v2 API. */
  hasTcfApi: boolean;
  /** Ordered candidates to dismiss the overlay (highest priority first). */
  dismissTargets: DismissTarget[];
}

export interface DismissResult {
  success: boolean;
  /** Which target/method actually dismissed the overlay, if any. */
  method?: DismissMethod;
  /** Selector or API call that worked. */
  detail?: string;
  /** True if the overlay still appears to be present after the attempt. */
  stillPresent?: boolean;
}

// ─── Detection script ──────────────────────────────────────────────────────

/**
 * Self-contained JS evaluated in the page. Returns either null (no overlay)
 * or a BlockingOverlay-shaped object. Pure structural detection — does not
 * read button text except as a last-resort tiebreaker.
 */
const DETECTION_SCRIPT = `(() => {
  // ─── CMP fingerprint table ──────────────────────────────────────────
  // Each entry: [container selector, platform name, reject selector(s), accept selector(s)]
  const CMP_FINGERPRINTS = [
    {
      platform: "onetrust",
      container: "#onetrust-banner-sdk, #onetrust-consent-sdk",
      reject: ["#onetrust-reject-all-handler", ".ot-pc-refuse-all-handler"],
      accept: ["#onetrust-accept-btn-handler"],
      close: ["#onetrust-close-btn-container button", ".onetrust-close-btn-handler"],
    },
    {
      platform: "cookiebot",
      container: "#CybotCookiebotDialog",
      reject: ["#CybotCookiebotDialogBodyButtonDecline", "#CybotCookiebotDialogBodyLevelButtonLevelOptinDeclineAll"],
      accept: ["#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll", "#CybotCookiebotDialogBodyButtonAccept"],
      close: [],
    },
    {
      platform: "didomi",
      container: "#didomi-host, #didomi-notice",
      reject: ["#didomi-notice-disagree-button", ".didomi-continue-without-agreeing"],
      accept: ["#didomi-notice-agree-button"],
      close: [],
    },
    {
      platform: "sourcepoint",
      container: '[id^="sp_message_container"], [id^="sp_message_iframe"]',
      reject: [".sp_choice_type_REJECT_ALL", '[title*="Reject" i]'],
      accept: [".sp_choice_type_11", '[title*="Accept All" i]'],
      close: ['[title="Close"]'],
    },
    {
      platform: "fundingchoices",
      container: ".fc-consent-root, .fc-dialog-container",
      reject: [".fc-cta-do-not-consent"],
      accept: [".fc-cta-consent"],
      close: [],
    },
    {
      platform: "cookiefirst",
      container: "[data-cookiefirst-widget]",
      reject: ['[data-cookiefirst-action="reject"]'],
      accept: ['[data-cookiefirst-action="accept"]'],
      close: [],
    },
    {
      platform: "quantcast",
      container: ".qc-cmp2-container, #qc-cmp2-container",
      reject: ['.qc-cmp2-summary-buttons button[mode="secondary"]'],
      accept: ['.qc-cmp2-summary-buttons button[mode="primary"]'],
      close: [],
    },
    {
      platform: "cookieconsent",
      container: ".cc-window, .cc-banner",
      reject: [".cc-deny", ".cc-dismiss"],
      accept: [".cc-allow", ".cc-btn.cc-allow"],
      close: [".cc-close"],
    },
    {
      platform: "klaro",
      container: ".klaro .cookie-notice, #klaro",
      reject: [".cm-btn-decline", ".cn-decline"],
      accept: [".cm-btn-accept-all", ".cn-ok"],
      close: [],
    },
    {
      platform: "cookieyes",
      container: ".cky-consent-container",
      reject: [".cky-btn-reject"],
      accept: [".cky-btn-accept"],
      close: [".cky-btn-close"],
    },
    {
      platform: "google_consent",
      container: 'form[action*="consent"]',
      reject: ['button[aria-label*="Reject" i]', '#W0wltc'],
      accept: ['button[aria-label*="Accept" i]', '#L2AGLb'],
      close: [],
    },
  ];

  // ─── Helpers ────────────────────────────────────────────────────────
  function isVisible(el) {
    if (!(el instanceof Element)) return false;
    if (el.offsetParent === null && el.tagName !== "BODY") {
      const cs = getComputedStyle(el);
      if (cs.position !== "fixed" && cs.position !== "sticky") return false;
    }
    const rect = el.getBoundingClientRect();
    if (rect.width < 1 || rect.height < 1) return false;
    const cs = getComputedStyle(el);
    if (cs.visibility === "hidden" || cs.display === "none" || parseFloat(cs.opacity) < 0.05) return false;
    return true;
  }
  function viewportArea() {
    return Math.max(1, window.innerWidth * window.innerHeight);
  }
  function elementArea(el) {
    const r = el.getBoundingClientRect();
    return Math.max(0, r.width) * Math.max(0, r.height);
  }
  function bounds(el) {
    const r = el.getBoundingClientRect();
    return {
      x: Math.round(r.left),
      y: Math.round(r.top),
      width: Math.round(r.width),
      height: Math.round(r.height),
    };
  }
  function uniqueSelectorFor(el) {
    if (!(el instanceof Element)) return null;
    if (el.id && /^[A-Za-z][\\w-]*$/.test(el.id)) {
      const escaped = "#" + el.id.replace(/([^a-zA-Z0-9_-])/g, "\\\\$1");
      try { if (document.querySelectorAll(escaped).length === 1) return escaped; } catch {}
    }
    // Build a path of tag + nth-of-type up to a stable ancestor.
    let cur = el;
    const parts = [];
    while (cur && cur.nodeType === 1 && parts.length < 6) {
      let part = cur.tagName.toLowerCase();
      if (cur.id && /^[A-Za-z][\\w-]*$/.test(cur.id)) {
        part = part + "#" + cur.id;
        parts.unshift(part);
        break;
      }
      const parent = cur.parentElement;
      if (parent) {
        const same = Array.from(parent.children).filter(c => c.tagName === cur.tagName);
        if (same.length > 1) part += ":nth-of-type(" + (same.indexOf(cur) + 1) + ")";
      }
      parts.unshift(part);
      cur = cur.parentElement;
    }
    return parts.join(" > ");
  }
  function classify(container) {
    const text = (container.textContent || "").slice(0, 1500).toLowerCase();
    const idClass = ((container.id || "") + " " + (container.className || "")).toLowerCase();
    // Cookie / consent signals — keyword-based, broadly multilingual via CMP table above.
    const cookieKeywords = ["cookie", "consent", "gdpr", "privacy", "tracking", "datenschutz", "cookies", "tracker"];
    if (cookieKeywords.some(k => idClass.includes(k))) return "cookie_consent";
    // Most consent dialogs contain the word "cookie" or its plural in many languages.
    if (/cookie|cookies|gdpr|consent|datenschutz|privacidad|confidentialit/i.test(text)) return "cookie_consent";
    if (/notification|subscribe|newsletter|push notif/i.test(text)) return "notification_prompt";
    if (/sign in|log in|create account|continue reading|subscribe to read/i.test(text) ||
        container.querySelector('input[type="password"], input[type="email"]')) {
      return /paywall|subscribe|premium|continue reading/i.test(text) ? "paywall" : "login_wall";
    }
    return "generic";
  }

  // ─── Step 1: Find candidate overlay containers ──────────────────────
  // Three independent sources of evidence; we union them.
  const candidates = new Set();

  // 1a. CMP fingerprints — most reliable.
  let cmpHit = null;
  for (const fp of CMP_FINGERPRINTS) {
    const found = document.querySelector(fp.container);
    if (found && isVisible(found)) {
      candidates.add(found);
      if (!cmpHit) cmpHit = { fp, container: found };
    }
  }

  // 1b. ARIA modal dialogs.
  const ariaModals = document.querySelectorAll('[role="dialog"], [role="alertdialog"], [aria-modal="true"]');
  ariaModals.forEach(el => { if (isVisible(el)) candidates.add(el); });

  // ─── FAST PATH ──────────────────────────────────────────────────────
  // If no CMP fingerprint matched and no ARIA modal exists, scan the DOM for
  // *cheap* overlay markers (id/class containing cookie/consent/modal/overlay)
  // before doing the expensive computed-style sweep. Most pages without
  // overlays exit here in <5ms instead of forcing layout on 4k elements.
  const cheapOverlayHints = document.querySelectorAll(
    '[id*="cookie" i], [id*="consent" i], [id*="gdpr" i], [class*="cookie" i], [class*="consent" i], [class*="gdpr" i], [id*="modal" i], [class*="modal" i], [class*="overlay" i], [class*="banner" i], [data-testid*="cookie" i], [data-testid*="consent" i]'
  );
  const hasCheapHints = cheapOverlayHints.length > 0;
  const skipExpensiveScan = !cmpHit && ariaModals.length === 0 && !hasCheapHints;
  if (skipExpensiveScan && candidates.size === 0) return null;

  // 1c. Fixed/sticky elements with high z-index covering significant area.
  // Skip if we already have a CMP hit AND no ARIA modal — the CMP container is
  // already in candidates and the heavy scan would just duplicate.
  if (!skipExpensiveScan && !(cmpHit && ariaModals.length === 0)) {
    const allEls = document.querySelectorAll("body *");
    const cap = Math.min(allEls.length, 4000);
    for (let i = 0; i < cap; i++) {
      const el = allEls[i];
      const cs = getComputedStyle(el);
      if (cs.position !== "fixed" && cs.position !== "sticky") continue;
      const z = parseInt(cs.zIndex, 10);
      if (!Number.isFinite(z) || z < 100) continue;
      const area = elementArea(el);
      const ratio = area / viewportArea();
      // Either covers >=20% of viewport OR is a banner-shaped strip across the page
      const r = el.getBoundingClientRect();
      const isBanner = r.width >= window.innerWidth * 0.7 && r.height >= 60 && r.height <= window.innerHeight * 0.9;
      if (ratio >= 0.2 || isBanner) {
        if (isVisible(el)) candidates.add(el);
      }
    }
  }

  if (candidates.size === 0) return null;

  // Pick the topmost / most-blocking candidate (largest z-index, then largest area).
  let best = null;
  let bestScore = -Infinity;
  candidates.forEach(el => {
    const cs = getComputedStyle(el);
    const z = parseInt(cs.zIndex, 10);
    const zScore = Number.isFinite(z) ? z : 0;
    const areaScore = elementArea(el) / viewportArea();
    const ariaBonus = el.getAttribute("aria-modal") === "true" ? 1000 : 0;
    const score = zScore + areaScore * 100 + ariaBonus;
    if (score > bestScore) { bestScore = score; best = el; }
  });
  if (!best) return null;

  // ─── Step 2: Classify ───────────────────────────────────────────────
  const overlayType = cmpHit ? "cookie_consent" : classify(best);

  // ─── Step 3: Dismiss targets (priority order) ───────────────────────
  const targets = [];

  // 3a. CMP-specific selectors — highest priority.
  if (cmpHit) {
    for (const sel of cmpHit.fp.reject) {
      const el = document.querySelector(sel);
      if (el && isVisible(el)) {
        targets.push({ selector: sel, label: (el.textContent || "").trim(), intent: "reject", method: "cmp_selector", confidence: 0.95 });
      }
    }
    for (const sel of cmpHit.fp.close) {
      const el = document.querySelector(sel);
      if (el && isVisible(el)) {
        targets.push({ selector: sel, label: (el.textContent || "").trim(), intent: "close", method: "cmp_selector", confidence: 0.85 });
      }
    }
    for (const sel of cmpHit.fp.accept) {
      const el = document.querySelector(sel);
      if (el && isVisible(el)) {
        targets.push({ selector: sel, label: (el.textContent || "").trim(), intent: "accept", method: "cmp_selector", confidence: 0.6 });
      }
    }
  }

  // 3b. ARIA dismiss patterns within the overlay.
  const ariaSelectors = [
    'button[aria-label*="reject" i]',
    'button[aria-label*="decline" i]',
    'button[aria-label*="refuse" i]',
    'button[aria-label*="dismiss" i]',
    'button[aria-label*="close" i]',
    '[role="button"][aria-label*="close" i]',
  ];
  for (const sel of ariaSelectors) {
    const el = best.querySelector(sel);
    if (el && isVisible(el)) {
      const al = (el.getAttribute("aria-label") || "").toLowerCase();
      const intent = /reject|decline|refuse/.test(al) ? "reject" :
                     /close/.test(al) ? "close" : "dismiss";
      const ds = uniqueSelectorFor(el);
      if (ds) targets.push({ selector: ds, label: el.getAttribute("aria-label") || "", intent, method: "aria", confidence: 0.8 });
    }
  }

  // 3c. Positional X-button: small button in top-right of overlay with no/minimal text.
  const overlayRect = best.getBoundingClientRect();
  const candidatesX = best.querySelectorAll('button, [role="button"], a[role="button"], svg[role="button"]');
  for (const btn of candidatesX) {
    if (!isVisible(btn)) continue;
    const r = btn.getBoundingClientRect();
    const w = r.width, h = r.height;
    if (w > 60 || h > 60) continue;
    const inTopRight = r.right >= overlayRect.right - 80 && r.top <= overlayRect.top + 80;
    if (!inTopRight) continue;
    const text = (btn.textContent || "").trim();
    if (text.length > 2 && !/^[\\u00d7\\u2715\\u2716x\\u00D7]$/i.test(text)) continue;
    const ds = uniqueSelectorFor(btn);
    if (ds) targets.push({ selector: ds, label: text || "×", intent: "close", method: "position", confidence: 0.7 });
  }

  // 3d. Styling-based intent detection within the overlay.
  // Secondary/outline buttons are typically reject; primary/filled are accept.
  // This is language-independent.
  const buttonEls = Array.from(best.querySelectorAll('button, [role="button"], a[role="button"]'));
  // 3d.i — class-name based reject (fast)
  for (const btn of buttonEls) {
    if (!isVisible(btn)) continue;
    const cls = (btn.className || "").toString().toLowerCase();
    if (/reject|decline|refuse|deny|disagree/.test(cls)) {
      const ds = uniqueSelectorFor(btn);
      if (ds && !targets.some(t => t.selector === ds)) {
        targets.push({ selector: ds, label: (btn.textContent || "").trim(), intent: "reject", method: "position", confidence: 0.78 });
      }
    }
  }
  // 3d.ii — computed-style heuristic. For CTA pairs in a consent dialog, the
  // accept button is typically filled (opaque background) and the reject is
  // typically outline/ghost (transparent background, visible border). When we
  // find exactly one of each in the overlay's button row, the outline one is
  // the reject. Pure visual cues — no language dependency.
  //
  // CRITICAL: filter out "Settings/Preferences/Manage/Customize/More options"
  // — these are usually styled identically to reject (outline) but clicking
  // them opens the prefs panel instead of dismissing. Multilingual.
  function bgAlpha(cs) {
    const m = (cs.backgroundColor || "").match(/rgba?\\(([^)]+)\\)/);
    if (!m) return 1;
    const parts = m[1].split(",").map(s => parseFloat(s.trim()));
    return parts.length === 4 ? parts[3] : 1;
  }
  function hasVisibleBorder(cs) {
    const w = parseFloat(cs.borderTopWidth || "0");
    return w >= 1 && cs.borderTopStyle !== "none";
  }
  // Words that indicate "open preferences / manage cookies", NOT reject.
  const SETTINGS_WORDS = [
    "settings", "preferences", "manage", "customize", "customise", "options",
    "more options", "more info", "details", "show purposes", "purposes",
    "show vendors", "vendors", "configure", "choices", "manage choices",
    // de
    "einstellungen", "verwalten", "anpassen", "auswahl", "details",
    // fr
    "paramètres", "parametres", "préférences", "preferences", "gérer", "gerer", "personnaliser", "plus d'options",
    // es
    "configuración", "configuracion", "preferencias", "gestionar", "personalizar", "más opciones",
    // it
    "impostazioni", "preferenze", "gestire", "personalizza", "altre opzioni",
    // pt
    "configurações", "configuracoes", "preferências", "preferencias", "gerir", "personalizar",
    // nl
    "instellingen", "voorkeuren", "beheren", "aanpassen",
    // sv/da/no
    "inställningar", "indstillinger", "innstillinger", "anpassa", "tilpas",
    // el
    "ρυθμίσεις", "προτιμήσεις", "διαχείριση",
    // ja/zh/ko
    "設定", "設置", "设置", "환경설정", "설정",
    // ru
    "настройки", "управление",
  ];
  function isSettingsButton(btn) {
    const text = (btn.textContent || "").trim().toLowerCase();
    if (text.length === 0 || text.length > 60) return false;
    return SETTINGS_WORDS.some(w => text === w || text.includes(w));
  }
  const styleClassed = buttonEls.filter(b => isVisible(b)).map(b => {
    const cs = getComputedStyle(b);
    const filled = bgAlpha(cs) > 0.4;
    const outline = !filled && hasVisibleBorder(cs);
    return { btn: b, filled, outline, settings: isSettingsButton(b) };
  });
  const filledOnes = styleClassed.filter(x => x.filled && !x.settings);
  const outlineOnes = styleClassed.filter(x => x.outline && !x.settings);
  if (filledOnes.length === 1 && outlineOnes.length === 1) {
    const ds = uniqueSelectorFor(outlineOnes[0].btn);
    if (ds && !targets.some(t => t.selector === ds)) {
      targets.push({
        selector: ds,
        label: (outlineOnes[0].btn.textContent || "").trim(),
        intent: "reject",
        method: "position",
        confidence: 0.72,
      });
    }
  }

  // 3e. Text-match fallback — last resort, multilingual word list.
  // Single-word triggers (match if button text === word OR starts with word).
  const REJECT_WORDS = [
    "reject", "decline", "refuse", "deny", "disagree", "no thanks", "not now",
    "ablehnen", "verweigern", "nein", // de
    "refuser", "refus", "non merci", // fr
    "rechazar", "no acepto", // es
    "rifiuta", // it
    "rejeitar", "recusar", // pt
    "afwijzen", // nl
    "avvisa", "neka", // sv/da
    "απόρριψη", "διαφωνώ", // el
    "拒否", // ja
    "拒绝", // zh
    "거부", // ko
    "отклонить", // ru
  ];
  // Phrase triggers (match anywhere in button text). Cover the long tail of
  // GDPR phrasings that don't start with "reject/decline".
  const REJECT_PHRASES = [
    "continue without accepting", "without accepting", "do not accept",
    "do not consent", "do not agree", "i do not agree",
    "only necessary", "only essential", "essential only", "necessary only",
    "use necessary only", "use only necessary",
    // de
    "ohne zustimmung", "nur erforderliche", "nur notwendige", "nur essenzielle", "nicht zustimmen",
    // fr
    "continuer sans accepter", "sans accepter", "uniquement nécessaires", "ne pas accepter", "refuser tout",
    // es
    "continuar sin aceptar", "sin aceptar", "solo necesarias", "solo esenciales", "no aceptar",
    // it
    "continua senza accettare", "senza accettare", "solo necessari", "solo essenziali", "non accettare",
    // pt
    "continuar sem aceitar", "sem aceitar", "apenas necessários", "apenas essenciais", "não aceitar",
    // nl
    "doorgaan zonder accepteren", "zonder accepteren", "alleen noodzakelijke", "niet accepteren",
    // ru
    "продолжить без принятия", "только необходимые",
  ];
  const CLOSE_WORDS = [
    "close", "dismiss", "skip", "later", "maybe later", "not now",
    "schließen", "verwerfen", // de
    "fermer", "ignorer", // fr
    "cerrar", "ignorar", "más tarde", // es
    "chiudere", "ignora", // it
    "fechar", // pt
    "sluiten", // nl
    "stäng", "luk", // sv/da
    "κλείσιμο", // el
    "閉じる", // ja
    "关闭", // zh
    "닫기", // ko
    "закрыть", // ru
  ];
  const allBtns = best.querySelectorAll('button, [role="button"], a[role="button"]');
  for (const btn of allBtns) {
    if (!isVisible(btn)) continue;
    const text = (btn.textContent || "").trim();
    if (!text || text.length > 80) continue;
    const lower = text.toLowerCase();
    // Skip settings/preferences buttons even if they contain a reject word.
    if (isSettingsButton(btn)) continue;
    let matched = false;
    if (REJECT_WORDS.some(w => lower === w || lower.startsWith(w))) {
      matched = true;
    } else if (REJECT_PHRASES.some(p => lower.includes(p))) {
      matched = true;
    }
    if (matched) {
      const ds = uniqueSelectorFor(btn);
      if (ds && !targets.some(t => t.selector === ds)) {
        targets.push({ selector: ds, label: text, intent: "reject", method: "text_match", confidence: 0.55 });
      }
      continue;
    }
    if (CLOSE_WORDS.some(w => lower === w || lower.startsWith(w))) {
      const ds = uniqueSelectorFor(btn);
      if (ds && !targets.some(t => t.selector === ds)) {
        targets.push({ selector: ds, label: text, intent: "close", method: "text_match", confidence: 0.5 });
      }
    }
  }

  // Sort: prefer reject > close > dismiss > accept; within intent, higher confidence first.
  const intentRank = { reject: 4, close: 3, dismiss: 2, accept: 1, unknown: 0 };
  targets.sort((a, b) => {
    const ir = intentRank[b.intent] - intentRank[a.intent];
    if (ir !== 0) return ir;
    return b.confidence - a.confidence;
  });

  // Confidence in the overlay-detection itself.
  let overlayConfidence = 0.4;
  if (cmpHit) overlayConfidence = 0.95;
  else if (best.getAttribute("aria-modal") === "true") overlayConfidence = 0.85;
  else if (overlayType === "cookie_consent") overlayConfidence = 0.7;
  else if (targets.length > 0) overlayConfidence = 0.6;

  const containerSelector = uniqueSelectorFor(best) || "body";

  return {
    containerSelector,
    bounds: bounds(best),
    type: overlayType,
    confidence: overlayConfidence,
    cmpPlatform: cmpHit ? cmpHit.fp.platform : null,
    hasTcfApi: typeof window.__tcfapi === "function" || typeof window.__cmp === "function",
    dismissTargets: targets.slice(0, 8),
  };
})()`;

// ─── TCF API helper script ─────────────────────────────────────────────────

const TCF_REJECT_SCRIPT = `(() => new Promise((resolve) => {
  const timeout = setTimeout(() => resolve({ ok: false, reason: "tcf_timeout" }), 2500);
  function done(ok, reason) { clearTimeout(timeout); resolve({ ok, reason }); }

  // Try CMP-specific reject APIs first — these are language-independent and
  // bypass the UI entirely. Each CMP's reject method is documented by the
  // vendor; we cover the major IAB TCF v2 implementations.
  try {
    // OneTrust
    if (typeof window.OneTrust === "object" && window.OneTrust) {
      if (typeof window.OneTrust.RejectAll === "function") {
        window.OneTrust.RejectAll(); return done(true, "OneTrust.RejectAll");
      }
      if (typeof window.OneTrust.SetAlertBoxClosed === "function") {
        window.OneTrust.SetAlertBoxClosed(); /* close fallback */
      }
    }
    // Didomi
    if (typeof window.Didomi === "object" && window.Didomi) {
      if (typeof window.Didomi.setUserDisagreeToAll === "function") {
        window.Didomi.setUserDisagreeToAll(); return done(true, "Didomi.setUserDisagreeToAll");
      }
    }
    // CookieConsent (osano / orestbida variants)
    if (typeof window.cookieconsent === "object" && window.cookieconsent) {
      if (typeof window.cookieconsent.deny === "function") {
        window.cookieconsent.deny(); return done(true, "cookieconsent.deny");
      }
    }
    if (typeof window.CookieConsent === "object" && window.CookieConsent) {
      if (typeof window.CookieConsent.acceptCategory === "function") {
        window.CookieConsent.acceptCategory([]); return done(true, "CookieConsent.acceptCategory[]");
      }
    }
    // Cookiebot
    if (typeof window.Cookiebot === "object" && window.Cookiebot) {
      if (typeof window.Cookiebot.withdraw === "function") {
        window.Cookiebot.withdraw(); return done(true, "Cookiebot.withdraw");
      }
    }
    // Quantcast Choice (CMP API v2)
    if (typeof window.__cmpapi === "function") {
      try {
        window.__cmpapi("setUserConsent", 2, () => {}, { consent: false });
        return done(true, "__cmpapi.setUserConsent");
      } catch {}
    }
    // Klaro
    if (typeof window.klaro === "object" && window.klaro) {
      if (typeof window.klaro.getManager === "function") {
        try {
          const m = window.klaro.getManager();
          if (m && typeof m.declineAll === "function") {
            m.declineAll(); m.saveAndApplyConsents?.();
            return done(true, "klaro.declineAll");
          }
        } catch {}
      }
    }
    // Generic IAB TCF v2 ping — confirms compliance even if no reject hook found.
    if (typeof window.__tcfapi === "function") {
      window.__tcfapi("getTCData", 2, (_tcData, ok) => {
        // We've already tried platform-specific rejects above; the standard
        // TCF v2 API has no portable "rejectAll" call (CMPs differ).
        done(false, ok ? "tcf_no_reject_hook" : "tcf_getTCData_failed");
      });
      return;
    }
    done(false, "no_cmp_api");
  } catch {
    done(false, "exception");
  }
}))`;

// ─── Public API ────────────────────────────────────────────────────────────

/** Runtime shape check for results returned by the in-page detection script. */
function isValidOverlay(raw: unknown): raw is BlockingOverlay {
  if (!raw || typeof raw !== "object") return false;
  const o = raw as Record<string, unknown>;
  if (typeof o.containerSelector !== "string" || o.containerSelector.length === 0) return false;
  if (!o.bounds || typeof o.bounds !== "object") return false;
  if (typeof o.type !== "string") return false;
  if (typeof o.confidence !== "number") return false;
  if (!Array.isArray(o.dismissTargets)) return false;
  for (const t of o.dismissTargets as unknown[]) {
    if (!t || typeof t !== "object") return false;
    const tt = t as Record<string, unknown>;
    if (typeof tt.selector !== "string" || tt.selector.length === 0) return false;
    if (typeof tt.intent !== "string" || typeof tt.method !== "string") return false;
  }
  return true;
}

/** Options accepted by `dismissOverlay`. */
export interface DismissOverlayOptions {
  /**
   * Last-resort: if every other strategy fails, hide the overlay container via
   * `display:none` to unblock content. Defaults to true. Set to false in
   * environments where DOM mutation is forbidden (e.g., recording).
   */
  allowNuclearHide?: boolean;
  /**
   * Time to wait for the page to react after a dismiss click before re-checking.
   * Default 800ms (covers CMP close animations).
   */
  verifyWaitMs?: number;
}

const DEFAULT_VERIFY_WAIT_MS = 800;

/**
 * Detect a blocking overlay on the current page. Returns null if none found.
 * Pure observation — does not click anything.
 */
export async function detectOverlay(page: Page): Promise<BlockingOverlay | null> {
  try {
    const raw = await page.evaluate(DETECTION_SCRIPT);
    if (!isValidOverlay(raw)) return null;
    return raw;
  } catch {
    return null;
  }
}

/**
 * Detect and dismiss in one call. Strategy order:
 *   1. TCF / CMP-specific reject API (no UI click)
 *   2. Walk dismissTargets in priority order, with one re-detect retry if the
 *      first target's selector is stale (page mutated between detect and click)
 *   3. Cross-origin iframe consent frames (Sourcepoint et al.)
 *   4. Nuclear: hide overlay container via display:none (gated by opts)
 */
export async function dismissOverlay(
  page: Page,
  opts: DismissOverlayOptions = {},
): Promise<DismissResult> {
  const verifyWait = opts.verifyWaitMs ?? DEFAULT_VERIFY_WAIT_MS;
  const allowNuclear = opts.allowNuclearHide ?? true;

  let overlay = await detectOverlay(page);
  if (!overlay) {
    return { success: false, detail: "no overlay detected" };
  }

  // Stage 1: programmatic CMP API (no UI click required).
  if (overlay.hasTcfApi || overlay.cmpPlatform) {
    try {
      const tcf = await page.evaluate(TCF_REJECT_SCRIPT) as { ok: boolean; reason: string };
      if (tcf?.ok) {
        await page.waitForTimeout(verifyWait);
        const stillThere = await detectOverlay(page);
        if (!stillThere || stillThere.confidence < 0.5) {
          return { success: true, method: "tcf_api", detail: tcf.reason };
        }
      }
    } catch { /* fall through to clicks */ }
  }

  // Stage 2: walk dismissTargets in priority order.
  // If the first target's selector is stale (page mutated), re-detect once and
  // retry against the fresh target list before giving up on click-based dismissal.
  let usedRetry = false;
  let targets = [...overlay.dismissTargets];
  for (let i = 0; i < targets.length; i++) {
    const target = targets[i];
    try {
      const loc = page.locator(target.selector).first();
      const visible = await loc.isVisible({ timeout: 200 }).catch(() => false);
      if (!visible) {
        // Stale selector — try one re-detect on the very first miss only.
        if (i === 0 && !usedRetry) {
          usedRetry = true;
          const fresh = await detectOverlay(page);
          if (fresh && fresh.dismissTargets.length > 0) {
            overlay = fresh;
            targets = [...fresh.dismissTargets];
            i = -1; // restart loop with fresh targets
          }
        }
        continue;
      }
      await loc.click({ timeout: 1500 });
      await page.waitForTimeout(verifyWait);
      const stillThere = await detectOverlay(page);
      const dismissed = !stillThere ||
        stillThere.containerSelector !== overlay.containerSelector ||
        stillThere.confidence < 0.5;
      if (dismissed) {
        return { success: true, method: target.method, detail: target.selector };
      }
    } catch { /* try next target */ }
  }

  // Stage 3: cross-origin iframe consent frames (Sourcepoint, OneTrust hosted).
  const iframeSelectors = [
    '[id^="sp_message_iframe"]',
    'iframe[title*="consent" i]',
    'iframe[title*="privacy" i]',
    'iframe[id*="consent"]',
  ];
  const iframeButtons = [
    'button[title*="Reject" i]',
    'button[aria-label*="Reject" i]',
    'button.sp_choice_type_REJECT_ALL',
    'button[title="Close"]',
    'button[aria-label="Close"]',
    'button[title*="Accept" i]',
    'button.sp_choice_type_11',
  ];
  for (const frameSel of iframeSelectors) {
    try {
      const frame = page.frameLocator(frameSel).first();
      for (const btnSel of iframeButtons) {
        try {
          const btn = frame.locator(btnSel).first();
          await btn.click({ timeout: 1200 });
          await page.waitForTimeout(verifyWait);
          return { success: true, method: "cmp_selector", detail: `iframe ${frameSel} ${btnSel}` };
        } catch { /* try next */ }
      }
    } catch { /* frame missing */ }
  }

  // Stage 4: nuclear hide — last resort. Hide the overlay container so the
  // page is at least usable. Does NOT grant or reject consent; the CMP state
  // is unchanged. Only runs if allowed.
  if (allowNuclear) {
    try {
      const hidden = await page.evaluate(`((containerSelector) => {
        function hide(el) {
          if (!el || !(el instanceof HTMLElement)) return false;
          el.style.setProperty("display", "none", "important");
          return true;
        }
        let count = 0;
        try {
          const target = document.querySelector(containerSelector);
          if (hide(target)) count++;
        } catch {}
        // Also hide common backdrop/overlay siblings that block scrolling.
        const backdrops = document.querySelectorAll(
          '[class*="backdrop" i], [class*="overlay" i][class*="modal" i], [aria-modal="true"]',
        );
        backdrops.forEach(el => { if (hide(el)) count++; });
        // Restore scrolling on body/html in case the CMP locked it.
        if (count > 0) {
          document.body.style.removeProperty("overflow");
          document.documentElement.style.removeProperty("overflow");
        }
        return count;
      })(${JSON.stringify(overlay.containerSelector)})`);
      if (typeof hidden === "number" && hidden > 0) {
        return {
          success: true,
          method: "cmp_selector",
          detail: `nuclear-hide:${hidden} elements (consent state unchanged)`,
        };
      }
    } catch { /* best effort */ }
  }

  return { success: false, stillPresent: true, detail: "all dismiss strategies failed" };
}
