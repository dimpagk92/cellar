//! Cookie-consent / paywall / modal banner detection and dismissal for the
//! Rust browser adapter.
//!
//! This is a port of the proven detection + dismissal pipeline from
//! `adapters/browser/src/overlay-detector.ts` (845 LOC of TS that's been
//! tuned against real-world WebVoyager and Mind2Web traces). The TS
//! adapter feeds the LangGraph runtime; this module brings the same
//! capability to the canonical (Rust) cortex / MCP path, which previously
//! had only the minimal `CEL_DISMISS_OVERLAYS_JS` snippet in cortex.rs
//! (English-only, accept-only, ~30 lines).
//!
//! ## Why port rather than share via FFI?
//!
//! The detection logic is pure browser JS — it runs inside the page via
//! `Runtime.evaluate`. Both adapters ship the SAME JS string verbatim
//! and dispatch through their respective CDP clients. No language
//! interop required, no duplicate logic — only the host plumbing differs.
//!
//! ## What this module owns
//!
//! - `DETECTION_SCRIPT` — the structural-detection JS (CMP fingerprints,
//!   ARIA, z-index/area sweep, classification, dismiss-target ranking).
//! - `TCF_REJECT_SCRIPT` — the IAB TCF v2 + vendor-specific reject API
//!   bypass (no UI click required when the page exposes the right hooks).
//! - `BlockingOverlay` / `DismissTarget` / `DismissResult` — Rust mirrors
//!   of the TS interface shapes, deserialised from the detection script's
//!   return value.
//! - `detect_overlay(client)` — observation-only.
//! - `dismiss_overlay(client, opts)` — try TCF → CMP selectors → ARIA →
//!   position → text-match → nuclear hide, in that priority order.
//! - `tag_blocked_elements(elements, overlay)` — mutates a slice of
//!   `ContextElement`s so any element whose bounds intersect the overlay
//!   container gets `properties["blocked_by_overlay"] = "true"`.

use cel_cdp::CdpClient;
use cel_context::ContextElement;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// The single source of truth for overlay detection JS. Mirrors
/// `adapters/browser/src/overlay-detector.ts`'s `DETECTION_SCRIPT`
/// verbatim (modulo the JS template-literal escapes that don't apply
/// to a Rust raw string). Returns either `null` or a JSON object whose
/// shape matches [`BlockingOverlay`].
///
/// If you change this, change the TS twin in lockstep. We will eventually
/// extract this string to a shared fixture file both adapters
/// `include_str!` / `import` — until then this comment is the contract.
const DETECTION_SCRIPT: &str = r##"(() => {
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
      container: 'form[action*="consent"], div[role="dialog"][aria-modal="true"]',
      // Cover German + Spanish + French equivalents that hit on .de / .es / .fr
      // domains. Trader Joe's / Mind2Web sites surface these and the previous
      // English-only selectors missed them.
      reject: [
        'button[aria-label*="Reject" i]', '#W0wltc',
        'button[aria-label*="Ablehnen" i]', 'button[aria-label*="Alle ablehnen" i]',
        'button[aria-label*="Rechazar" i]',
        'button[aria-label*="Refuser" i]',
      ],
      accept: [
        'button[aria-label*="Accept" i]', '#L2AGLb',
        'button[aria-label*="Akzeptieren" i]', 'button[aria-label*="Alle akzeptieren" i]',
        'button[aria-label*="Aceptar" i]',
        'button[aria-label*="Accepter" i]',
      ],
      close: [],
    },
  ];

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
  function viewportArea() { return Math.max(1, window.innerWidth * window.innerHeight); }
  function elementArea(el) {
    const r = el.getBoundingClientRect();
    return Math.max(0, r.width) * Math.max(0, r.height);
  }
  function bounds(el) {
    const r = el.getBoundingClientRect();
    return { x: Math.round(r.left), y: Math.round(r.top), width: Math.round(r.width), height: Math.round(r.height) };
  }
  function uniqueSelectorFor(el) {
    if (!(el instanceof Element)) return null;
    if (el.id && /^[A-Za-z][\w-]*$/.test(el.id)) {
      const escaped = "#" + el.id.replace(/([^a-zA-Z0-9_-])/g, "\\$1");
      try { if (document.querySelectorAll(escaped).length === 1) return escaped; } catch {}
    }
    let cur = el;
    const parts = [];
    while (cur && cur.nodeType === 1 && parts.length < 6) {
      let part = cur.tagName.toLowerCase();
      if (cur.id && /^[A-Za-z][\w-]*$/.test(cur.id)) {
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
    const cookieKeywords = ["cookie", "consent", "gdpr", "privacy", "tracking", "datenschutz", "cookies", "tracker"];
    if (cookieKeywords.some(k => idClass.includes(k))) return "cookie_consent";
    if (/cookie|cookies|gdpr|consent|datenschutz|privacidad|confidentialit/i.test(text)) return "cookie_consent";
    if (/notification|subscribe|newsletter|push notif/i.test(text)) return "notification_prompt";
    if (/sign in|log in|create account|continue reading|subscribe to read/i.test(text) ||
        container.querySelector('input[type="password"], input[type="email"]')) {
      return /paywall|subscribe|premium|continue reading/i.test(text) ? "paywall" : "login_wall";
    }
    return "generic";
  }

  const candidates = new Set();
  let cmpHit = null;
  for (const fp of CMP_FINGERPRINTS) {
    const found = document.querySelector(fp.container);
    if (found && isVisible(found)) { candidates.add(found); if (!cmpHit) cmpHit = { fp, container: found }; }
  }
  const ariaModals = document.querySelectorAll('[role="dialog"], [role="alertdialog"], [aria-modal="true"]');
  ariaModals.forEach(el => { if (isVisible(el)) candidates.add(el); });

  const cheapOverlayHints = document.querySelectorAll(
    '[id*="cookie" i], [id*="consent" i], [id*="gdpr" i], [class*="cookie" i], [class*="consent" i], [class*="gdpr" i], [id*="modal" i], [class*="modal" i], [class*="overlay" i], [class*="banner" i], [data-testid*="cookie" i], [data-testid*="consent" i]'
  );
  const hasCheapHints = cheapOverlayHints.length > 0;
  const skipExpensiveScan = !cmpHit && ariaModals.length === 0 && !hasCheapHints;
  if (skipExpensiveScan && candidates.size === 0) return null;

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
      const r = el.getBoundingClientRect();
      const isBanner = r.width >= window.innerWidth * 0.7 && r.height >= 60 && r.height <= window.innerHeight * 0.9;
      if (ratio >= 0.2 || isBanner) { if (isVisible(el)) candidates.add(el); }
    }
  }

  if (candidates.size === 0) return null;

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

  const overlayType = cmpHit ? "cookie_consent" : classify(best);

  const targets = [];
  if (cmpHit) {
    for (const sel of cmpHit.fp.reject) {
      const el = document.querySelector(sel);
      if (el && isVisible(el)) targets.push({ selector: sel, label: (el.textContent || "").trim(), intent: "reject", method: "cmp_selector", confidence: 0.95 });
    }
    for (const sel of cmpHit.fp.close) {
      const el = document.querySelector(sel);
      if (el && isVisible(el)) targets.push({ selector: sel, label: (el.textContent || "").trim(), intent: "close", method: "cmp_selector", confidence: 0.85 });
    }
    for (const sel of cmpHit.fp.accept) {
      const el = document.querySelector(sel);
      if (el && isVisible(el)) targets.push({ selector: sel, label: (el.textContent || "").trim(), intent: "accept", method: "cmp_selector", confidence: 0.6 });
    }
  }
  const ariaSelectors = [
    'button[aria-label*="reject" i]','button[aria-label*="decline" i]','button[aria-label*="refuse" i]',
    'button[aria-label*="dismiss" i]','button[aria-label*="close" i]','[role="button"][aria-label*="close" i]',
  ];
  for (const sel of ariaSelectors) {
    const el = best.querySelector(sel);
    if (el && isVisible(el)) {
      const al = (el.getAttribute("aria-label") || "").toLowerCase();
      const intent = /reject|decline|refuse/.test(al) ? "reject" : /close/.test(al) ? "close" : "dismiss";
      const ds = uniqueSelectorFor(el);
      if (ds) targets.push({ selector: ds, label: el.getAttribute("aria-label") || "", intent, method: "aria", confidence: 0.8 });
    }
  }
  const overlayRect = best.getBoundingClientRect();
  const candidatesX = best.querySelectorAll('button, [role="button"], a[role="button"], svg[role="button"]');
  for (const btn of candidatesX) {
    if (!isVisible(btn)) continue;
    const r = btn.getBoundingClientRect();
    if (r.width > 60 || r.height > 60) continue;
    const inTopRight = r.right >= overlayRect.right - 80 && r.top <= overlayRect.top + 80;
    if (!inTopRight) continue;
    const text = (btn.textContent || "").trim();
    if (text.length > 2 && !/^[×✕✖x×]$/i.test(text)) continue;
    const ds = uniqueSelectorFor(btn);
    if (ds) targets.push({ selector: ds, label: text || "×", intent: "close", method: "position", confidence: 0.7 });
  }
  const buttonEls = Array.from(best.querySelectorAll('button, [role="button"], a[role="button"]'));
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

  const REJECT_WORDS = ["reject","decline","refuse","deny","disagree","no thanks","not now","ablehnen","verweigern","nein","refuser","refus","non merci","rechazar","no acepto","rifiuta","rejeitar","recusar","afwijzen","avvisa","neka","απόρριψη","διαφωνώ","拒否","拒绝","거부","отклонить"];
  const REJECT_PHRASES = ["continue without accepting","without accepting","do not accept","do not consent","do not agree","i do not agree","only necessary","only essential","essential only","necessary only","use necessary only","use only necessary","ohne zustimmung","nur erforderliche","nur notwendige","nur essenzielle","nicht zustimmen","continuer sans accepter","sans accepter","uniquement nécessaires","ne pas accepter","refuser tout","continuar sin aceptar","sin aceptar","solo necesarias","solo esenciales","no aceptar","continua senza accettare","senza accettare","solo necessari","solo essenziali","non accettare","continuar sem aceitar","sem aceitar","apenas necessários","apenas essenciais","não aceitar","doorgaan zonder accepteren","zonder accepteren","alleen noodzakelijke","niet accepteren","продолжить без принятия","только необходимые"];
  const CLOSE_WORDS = ["close","dismiss","skip","later","maybe later","not now","schließen","verwerfen","fermer","ignorer","cerrar","ignorar","más tarde","chiudere","ignora","fechar","sluiten","stäng","luk","κλείσιμο","閉じる","关闭","닫기","закрыть"];
  const SETTINGS_WORDS = ["settings","preferences","manage","customize","customise","options","more options","more info","details","show purposes","purposes","show vendors","vendors","configure","choices","manage choices","einstellungen","verwalten","anpassen","auswahl","details","paramètres","parametres","préférences","preferences","gérer","gerer","personnaliser","plus d'options","configuración","configuracion","preferencias","gestionar","personalizar","más opciones","impostazioni","preferenze","gestire","personalizza","altre opzioni","configurações","configuracoes","preferências","preferencias","gerir","personalizar","instellingen","voorkeuren","beheren","aanpassen","inställningar","indstillinger","innstillinger","anpassa","tilpas","ρυθμίσεις","προτιμήσεις","διαχείριση","設定","設置","设置","환경설정","설정","настройки","управление"];
  function isSettingsButton(btn) {
    const text = (btn.textContent || "").trim().toLowerCase();
    if (text.length === 0 || text.length > 60) return false;
    return SETTINGS_WORDS.some(w => text === w || text.includes(w));
  }
  const allBtns = best.querySelectorAll('button, [role="button"], a[role="button"]');
  for (const btn of allBtns) {
    if (!isVisible(btn)) continue;
    const text = (btn.textContent || "").trim();
    if (!text || text.length > 80) continue;
    const lower = text.toLowerCase();
    if (isSettingsButton(btn)) continue;
    let matched = false;
    if (REJECT_WORDS.some(w => lower === w || lower.startsWith(w))) matched = true;
    else if (REJECT_PHRASES.some(p => lower.includes(p))) matched = true;
    if (matched) {
      const ds = uniqueSelectorFor(btn);
      if (ds && !targets.some(t => t.selector === ds)) targets.push({ selector: ds, label: text, intent: "reject", method: "text_match", confidence: 0.55 });
      continue;
    }
    if (CLOSE_WORDS.some(w => lower === w || lower.startsWith(w))) {
      const ds = uniqueSelectorFor(btn);
      if (ds && !targets.some(t => t.selector === ds)) targets.push({ selector: ds, label: text, intent: "close", method: "text_match", confidence: 0.5 });
    }
  }

  const intentRank = { reject: 4, close: 3, dismiss: 2, accept: 1, unknown: 0 };
  targets.sort((a, b) => {
    const ir = intentRank[b.intent] - intentRank[a.intent];
    if (ir !== 0) return ir;
    return b.confidence - a.confidence;
  });

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
})()"##;

/// Vendor-API reject helpers. Tries OneTrust, Didomi, Cookiebot,
/// CookieConsent, Quantcast, Klaro and standard `__tcfapi` ping.
/// Returns `{ ok: bool, reason: string }` so the Rust side can log
/// which path succeeded for receipt evidence.
const TCF_REJECT_SCRIPT: &str = r##"(() => new Promise((resolve) => {
  const timeout = setTimeout(() => resolve({ ok: false, reason: "tcf_timeout" }), 2500);
  function done(ok, reason) { clearTimeout(timeout); resolve({ ok, reason }); }
  try {
    if (typeof window.OneTrust === "object" && window.OneTrust) {
      if (typeof window.OneTrust.RejectAll === "function") { window.OneTrust.RejectAll(); return done(true, "OneTrust.RejectAll"); }
      if (typeof window.OneTrust.SetAlertBoxClosed === "function") { window.OneTrust.SetAlertBoxClosed(); }
    }
    if (typeof window.Didomi === "object" && window.Didomi) {
      if (typeof window.Didomi.setUserDisagreeToAll === "function") { window.Didomi.setUserDisagreeToAll(); return done(true, "Didomi.setUserDisagreeToAll"); }
    }
    if (typeof window.cookieconsent === "object" && window.cookieconsent) {
      if (typeof window.cookieconsent.deny === "function") { window.cookieconsent.deny(); return done(true, "cookieconsent.deny"); }
    }
    if (typeof window.CookieConsent === "object" && window.CookieConsent) {
      if (typeof window.CookieConsent.acceptCategory === "function") { window.CookieConsent.acceptCategory([]); return done(true, "CookieConsent.acceptCategory[]"); }
    }
    if (typeof window.Cookiebot === "object" && window.Cookiebot) {
      if (typeof window.Cookiebot.withdraw === "function") { window.Cookiebot.withdraw(); return done(true, "Cookiebot.withdraw"); }
    }
    if (typeof window.__cmpapi === "function") {
      try { window.__cmpapi("setUserConsent", 2, () => {}, { consent: false }); return done(true, "__cmpapi.setUserConsent"); } catch {}
    }
    if (typeof window.klaro === "object" && window.klaro) {
      if (typeof window.klaro.getManager === "function") {
        try {
          const m = window.klaro.getManager();
          if (m && typeof m.declineAll === "function") { m.declineAll(); m.saveAndApplyConsents?.(); return done(true, "klaro.declineAll"); }
        } catch {}
      }
    }
    if (typeof window.__tcfapi === "function") {
      window.__tcfapi("getTCData", 2, (_tcData, ok) => { done(false, ok ? "tcf_no_reject_hook" : "tcf_getTCData_failed"); });
      return;
    }
    done(false, "no_cmp_api");
  } catch { done(false, "exception"); }
}))"##;

/// Nuclear-hide JS: when every other strategy fails, suppress the overlay
/// container via inline `display: none !important` so the page is at least
/// usable. Does NOT touch CMP consent state — caller's responsibility to
/// document that a nuclear-hide leaves consent unrecorded.
const NUCLEAR_HIDE_SCRIPT_TEMPLATE: &str = r##"((containerSelector) => {
  function hide(el) {
    if (!el || !(el instanceof HTMLElement)) return false;
    el.style.setProperty("display", "none", "important");
    return true;
  }
  let count = 0;
  try { const target = document.querySelector(containerSelector); if (hide(target)) count++; } catch {}
  const backdrops = document.querySelectorAll('[class*="backdrop" i], [class*="overlay" i][class*="modal" i], [aria-modal="true"]');
  backdrops.forEach(el => { if (hide(el)) count++; });
  if (count > 0) {
    document.body.style.removeProperty("overflow");
    document.documentElement.style.removeProperty("overflow");
  }
  return count;
})(__PLACEHOLDER__)"##;

/// Click a single dismiss target by CSS selector. Used after detection
/// to actually press a reject / close button. Returns true on success.
const CLICK_SELECTOR_SCRIPT_TEMPLATE: &str = r##"((selector) => {
  try {
    const el = document.querySelector(selector);
    if (!el) return false;
    if (typeof el.scrollIntoView === "function") {
      try { el.scrollIntoView({ block: "center", inline: "center" }); } catch {}
    }
    if (typeof el.click === "function") { el.click(); return true; }
    return false;
  } catch { return false; }
})(__PLACEHOLDER__)"##;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayBounds {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissTarget {
    pub selector: String,
    #[serde(default)]
    pub label: String,
    pub intent: String,
    pub method: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockingOverlay {
    #[serde(rename = "containerSelector")]
    pub container_selector: String,
    pub bounds: OverlayBounds,
    #[serde(rename = "type")]
    pub overlay_type: String,
    pub confidence: f64,
    #[serde(rename = "cmpPlatform")]
    pub cmp_platform: Option<String>,
    #[serde(rename = "hasTcfApi", default)]
    pub has_tcf_api: bool,
    #[serde(rename = "dismissTargets", default)]
    pub dismiss_targets: Vec<DismissTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissResult {
    pub success: bool,
    /// Which method actually dismissed the overlay: `tcf_api`, `cmp_selector`,
    /// `aria`, `position`, `text_match`, or `nuclear_hide`. None when nothing
    /// worked (or no overlay was present in the first place).
    pub method: Option<String>,
    /// Free-form detail for receipts (selector that worked, vendor API name,
    /// `tcf_api` reason string, etc.).
    pub detail: Option<String>,
}

/// Dismissal options. Defaults are tuned for benchmark / agent runs —
/// `allow_nuclear_hide = true` (don't get stuck on stubborn overlays),
/// `verify_wait_ms = 800` (covers most CMP close animations).
#[derive(Debug, Clone)]
pub struct DismissOverlayOptions {
    pub allow_nuclear_hide: bool,
    pub verify_wait_ms: u64,
}

impl Default for DismissOverlayOptions {
    fn default() -> Self {
        Self {
            allow_nuclear_hide: true,
            verify_wait_ms: 800,
        }
    }
}

/// Run the detection script against `client` and return the parsed
/// overlay (or None if no overlay was found or the script errored).
///
/// Returns None silently on any CDP / parse error — the caller (cortex
/// tick, benchmark setup) should treat absence as "no overlay" rather
/// than "detection failed" because a botched detect is harmless but a
/// spurious error log every tick is noise.
pub async fn detect_overlay(client: &CdpClient) -> Option<BlockingOverlay> {
    match client.evaluate(DETECTION_SCRIPT).await {
        Ok(value) => {
            if value.is_null() {
                return None;
            }
            match serde_json::from_value::<BlockingOverlay>(value.clone()) {
                Ok(overlay) => Some(overlay),
                Err(err) => {
                    debug!(error = %err, raw = %value, "overlay_detector: parse failed");
                    None
                }
            }
        }
        Err(err) => {
            debug!(error = %err, "overlay_detector: detect evaluate failed");
            None
        }
    }
}

/// Detect AND dismiss in one call. Strategy priority:
///   1. TCF / CMP-specific reject API (no UI click)
///   2. Walk dismissTargets (CMP > ARIA > position > text_match)
///   3. Nuclear hide via `display: none !important` (gated by `opts.allow_nuclear_hide`)
///
/// Returns `DismissResult { success: false, .. }` when there was no
/// overlay to begin with (`detail = "no_overlay"`), and
/// `success: true, method: Some(_)` when something was dismissed.
pub async fn dismiss_overlay(client: &CdpClient, opts: &DismissOverlayOptions) -> DismissResult {
    let overlay = match detect_overlay(client).await {
        Some(o) => o,
        None => {
            return DismissResult {
                success: false,
                method: None,
                detail: Some("no_overlay".into()),
            };
        }
    };

    // Stage 1: TCF / vendor reject API. No UI click required when this works.
    if overlay.has_tcf_api || overlay.cmp_platform.is_some() {
        if let Ok(value) = client.evaluate(TCF_REJECT_SCRIPT).await {
            let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let reason = value
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if ok {
                tokio::time::sleep(std::time::Duration::from_millis(opts.verify_wait_ms)).await;
                // Re-detect: if overlay vanished (or its confidence dropped), TCF won.
                let after = detect_overlay(client).await;
                let dismissed = after
                    .as_ref()
                    .map(|a| {
                        a.confidence < 0.5 || a.container_selector != overlay.container_selector
                    })
                    .unwrap_or(true);
                if dismissed {
                    return DismissResult {
                        success: true,
                        method: Some("tcf_api".into()),
                        detail: Some(reason),
                    };
                }
            }
        }
    }

    // Stage 2: walk dismissTargets in priority order. Each click is a
    // separate evaluate so a single bad selector doesn't poison the rest.
    for target in &overlay.dismiss_targets {
        let sel_json = serde_json::to_string(&target.selector).unwrap_or_else(|_| "\"\"".into());
        let click_js = CLICK_SELECTOR_SCRIPT_TEMPLATE.replace("__PLACEHOLDER__", &sel_json);
        match client.evaluate(&click_js).await {
            Ok(value) => {
                if value.as_bool() == Some(true) {
                    tokio::time::sleep(std::time::Duration::from_millis(opts.verify_wait_ms)).await;
                    let after = detect_overlay(client).await;
                    let dismissed = after
                        .as_ref()
                        .map(|a| {
                            a.confidence < 0.5 || a.container_selector != overlay.container_selector
                        })
                        .unwrap_or(true);
                    if dismissed {
                        return DismissResult {
                            success: true,
                            method: Some(target.method.clone()),
                            detail: Some(target.selector.clone()),
                        };
                    }
                }
            }
            Err(err) => {
                debug!(selector = %target.selector, error = %err, "overlay_detector: click failed");
            }
        }
    }

    // Stage 3: nuclear hide. Last resort — page might still be in a
    // broken state but at least the content is reachable.
    if opts.allow_nuclear_hide {
        let sel_json = serde_json::to_string(&overlay.container_selector)
            .unwrap_or_else(|_| "\"body\"".into());
        let nuke_js = NUCLEAR_HIDE_SCRIPT_TEMPLATE.replace("__PLACEHOLDER__", &sel_json);
        if let Ok(value) = client.evaluate(&nuke_js).await {
            let hidden = value.as_i64().unwrap_or(0);
            if hidden > 0 {
                return DismissResult {
                    success: true,
                    method: Some("nuclear_hide".into()),
                    detail: Some(format!(
                        "{} element(s) hidden; consent state UNCHANGED",
                        hidden
                    )),
                };
            }
        }
    }

    DismissResult {
        success: false,
        method: None,
        detail: Some("all_strategies_failed".into()),
    }
}

/// Tag every element whose bounds intersect the overlay container's bounds
/// with `properties["blocked_by_overlay"] = "true"`. Lets the planner
/// see at a glance which elements are obscured by a banner without
/// needing to reason about z-index. Idempotent — runs on each cortex
/// tick that detects an overlay.
///
/// Also records the overlay metadata once on the FIRST element in
/// `elements` (or appends a sentinel if `elements` is empty) so the
/// planner can introspect the overlay shape without us having to extend
/// `ScreenContext` schema-wise yet. Keys:
///   - `_overlay_present` = "true"
///   - `_overlay_type` = "cookie_consent" | ...
///   - `_overlay_cmp` = "onetrust" | ... (when known)
///   - `_overlay_confidence` = 0..1 stringified
pub fn tag_blocked_elements(elements: &mut [ContextElement], overlay: &BlockingOverlay) {
    let ox = overlay.bounds.x;
    let oy = overlay.bounds.y;
    let ow = overlay.bounds.width;
    let oh = overlay.bounds.height;
    let ox2 = ox + ow;
    let oy2 = oy + oh;

    for el in elements.iter_mut() {
        if let Some(bounds) = &el.bounds {
            // Cheap AABB intersection. We treat ContextElement.bounds as
            // viewport coords (which the browser-rs adapter produces from
            // `getBoundingClientRect()` via cel_cdp::DomElement). Anything
            // overlapping the overlay container is flagged.
            let ex = bounds.x as i64;
            let ey = bounds.y as i64;
            let ex2 = ex + bounds.width as i64;
            let ey2 = ey + bounds.height as i64;
            let intersects = ex < ox2 && ex2 > ox && ey < oy2 && ey2 > oy;
            if intersects {
                el.properties
                    .insert("blocked_by_overlay".into(), "true".into());
            }
        }
    }

    // Stamp overlay metadata onto the first element so it shows up in
    // perception without requiring a ScreenContext extension. The
    // planner can look at any element's properties to discover the
    // overlay context; we pick element[0] by convention.
    if let Some(first) = elements.first_mut() {
        first
            .properties
            .insert("_overlay_present".into(), "true".into());
        first
            .properties
            .insert("_overlay_type".into(), overlay.overlay_type.clone());
        if let Some(cmp) = &overlay.cmp_platform {
            first.properties.insert("_overlay_cmp".into(), cmp.clone());
        }
        first.properties.insert(
            "_overlay_confidence".into(),
            format!("{:.2}", overlay.confidence),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_overlay_payload() {
        // The CDP RuntimeEvaluate result for our DETECTION_SCRIPT is
        // returnByValue: true => arrives as a plain serde_json::Value.
        // Pin the parse path so a future renaming of containerSelector
        // / dismissTargets / cmpPlatform doesn't silently break the
        // serde rename mappings.
        let raw = serde_json::json!({
            "containerSelector": "#cookie-banner",
            "bounds": { "x": 10, "y": 100, "width": 800, "height": 200 },
            "type": "cookie_consent",
            "confidence": 0.95,
            "cmpPlatform": "onetrust",
            "hasTcfApi": true,
            "dismissTargets": [
                { "selector": "#reject-all", "label": "Reject All", "intent": "reject", "method": "cmp_selector", "confidence": 0.95 }
            ]
        });
        let overlay: BlockingOverlay = serde_json::from_value(raw).expect("parse");
        assert_eq!(overlay.container_selector, "#cookie-banner");
        assert_eq!(overlay.bounds.width, 800);
        assert_eq!(overlay.overlay_type, "cookie_consent");
        assert_eq!(overlay.cmp_platform.as_deref(), Some("onetrust"));
        assert!(overlay.has_tcf_api);
        assert_eq!(overlay.dismiss_targets.len(), 1);
        assert_eq!(overlay.dismiss_targets[0].intent, "reject");
    }

    #[test]
    fn parses_minimal_overlay_without_cmp() {
        // Pages without a CMP fingerprint return cmpPlatform: null and
        // empty dismissTargets when no buttons match. The parser must
        // handle both cases without erroring.
        let raw = serde_json::json!({
            "containerSelector": "div.modal",
            "bounds": { "x": 0, "y": 0, "width": 400, "height": 300 },
            "type": "generic",
            "confidence": 0.4,
            "cmpPlatform": null,
            "hasTcfApi": false,
            "dismissTargets": []
        });
        let overlay: BlockingOverlay = serde_json::from_value(raw).expect("parse");
        assert!(overlay.cmp_platform.is_none());
        assert!(overlay.dismiss_targets.is_empty());
        assert!(!overlay.has_tcf_api);
    }

    #[test]
    fn tag_blocked_elements_flags_intersecting_only() {
        use cel_context::{Bounds, ContentRole, ContextSource};
        use std::collections::HashMap;

        fn mk(id: &str, x: i32, y: i32, w: u32, h: u32) -> ContextElement {
            ContextElement {
                id: id.into(),
                label: None,
                description: None,
                element_type: "button".into(),
                value: None,
                bounds: Some(Bounds {
                    x,
                    y,
                    width: w,
                    height: h,
                }),
                state: Default::default(),
                parent_id: None,
                actions: vec![],
                confidence: 1.0,
                source: ContextSource::Cdp,
                content_role: ContentRole::default(),
                properties: HashMap::new(),
            }
        }
        let mut elements = vec![
            mk("inside", 50, 50, 40, 40),    // inside overlay
            mk("outside", 500, 500, 40, 40), // outside overlay
            mk("partial", 90, 90, 40, 40),   // partial overlap
        ];
        let overlay = BlockingOverlay {
            container_selector: "#banner".into(),
            bounds: OverlayBounds {
                x: 0,
                y: 0,
                width: 120,
                height: 120,
            },
            overlay_type: "cookie_consent".into(),
            confidence: 0.9,
            cmp_platform: Some("onetrust".into()),
            has_tcf_api: true,
            dismiss_targets: vec![],
        };
        tag_blocked_elements(&mut elements, &overlay);
        assert_eq!(
            elements[0]
                .properties
                .get("blocked_by_overlay")
                .map(String::as_str),
            Some("true")
        );
        assert!(!elements[1].properties.contains_key("blocked_by_overlay"));
        assert_eq!(
            elements[2]
                .properties
                .get("blocked_by_overlay")
                .map(String::as_str),
            Some("true")
        );
        // First element also carries the overlay metadata stamp.
        assert_eq!(
            elements[0]
                .properties
                .get("_overlay_present")
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            elements[0]
                .properties
                .get("_overlay_type")
                .map(String::as_str),
            Some("cookie_consent")
        );
        assert_eq!(
            elements[0]
                .properties
                .get("_overlay_cmp")
                .map(String::as_str),
            Some("onetrust")
        );
    }

    #[test]
    fn dismiss_options_defaults_are_benchmark_friendly() {
        // Confirms the default opts. If we ever flip allow_nuclear_hide
        // to false by default, every dim sitea on the real web that
        // doesn't expose a clean CMP API would get stuck. Pin the
        // benchmark contract.
        let opts = DismissOverlayOptions::default();
        assert!(opts.allow_nuclear_hide);
        assert_eq!(opts.verify_wait_ms, 800);
    }
}
