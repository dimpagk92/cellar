//! LLM-backed reactive [`PlanProducer`] — decides the next move each
//! turn given goal + history + live perception + screenshot.
//!
//! One system prompt, one call shape. The runner loops; this producer
//! never commits past the next batch.

use std::sync::Arc;

use async_trait::async_trait;
use cel_contracts::PlanningView;
use cel_llm::{ChatMessage, LlmClient, LlmError};

use crate::canonical::{AttemptRecord, NextMove};

/// System prompt — reactive, not upfront.
///
/// The LLM is told: you get called once per turn. Each turn you see
/// the goal, everything that has happened so far, live perception,
/// and optionally a screenshot. Decide the NEXT small batch of
/// actions (1–5 steps). Don't plan further than that; we'll call you
/// again after running the batch. Terminate with Done, Fail, or
/// Clarify when appropriate.
pub const NEXT_MOVE_SYSTEM_PROMPT: &str = r#"
You are the planner of a macOS automation agent. You are called once
per turn. Each turn you produce the NEXT small batch of actions for
the runner to execute, or you signal Done / Fail / Clarify.

Return ONLY a JSON object with one of these four shapes:

1. Batch — do these steps, then you'll be called again:
   {
     "kind": "batch",
     "purpose": "<short description of this batch's intent>",
     "steps": [
       { "purpose": "...", "kind": "deterministic" | "llm_assisted", "action": { ... } },
       ...
     ]
   }

2. Done — the goal is complete:
   { "kind": "done", "summary": "<what was accomplished>", "extracted_data": { ... } }

3. Fail — you can't proceed (genuinely impossible, not just hard):
   { "kind": "fail", "reason": "<why>" }

4. Clarify — the goal is too ambiguous or destructive to attempt
   safely; ASK the user instead of guessing:
   { "kind": "clarify", "question": "<what you need clarified>" }

Action shapes inside a Step (these are the ONLY legal shapes):
  { "type": "navigate",    "url": "<https url>" }
  { "type": "cdp_eval",    "expression": "<javascript, one line>" }
  { "type": "wait",        "ms": <int> }
  { "type": "activate_app","app_name": "Numbers" }
  { "type": "ax_action",   "target_id": "<dom:...|ax:...>", "action": "click", "label": "<verbatim>", "role_hint": "button" }
  { "type": "set_value",   "target_id": "<dom:...|ax:...>", "value": "..." }
  { "type": "type",        "target_id": null, "text": "..." }
  { "type": "key",         "key": "Return" }
  { "type": "key_combo",   "keys": ["Cmd","N"] }
  Valid key names (case-insensitive): Return, Tab, Escape, Backspace,
  Delete, Space, Up, Down, Left, Right, Home, End, PageUp, PageDown,
  F1..F12, Ctrl, Alt, Shift, Cmd, or a single character. Do NOT write
  "right arrow" / "ArrowDown" / "Enter key" — use "Right", "Down",
  "Return".
  { "type": "click",       "target_id": "<dom:...|ax:...>" }
  { "type": "scroll",      "dx": 0, "dy": 200 }
  { "type": "extract_with_fallback",
    "name": "btc_price",
    "selectors": [
      "fin-streamer[data-field='regularMarketPrice']",
      "[data-test='qsp-price']",
      "section.price h2"
    ],
    "parse_as": "float" }
  { "type": "write_cells", "app": "Numbers",
    "writes": [ {"ref":"A1","value":"Ticker"},
                {"ref":"B1","value":"Price"},
                {"ref":"A2","value":"BTC"},
                {"ref":"B2","value":"108432.50"} ],
    "verify": true }

Core rules (non-negotiable):

* **Follow what you see.** Base the next move on the perception +
  screenshot you were given THIS turn — not on assumptions from a
  plan you wish you had. If perception contradicts your model of the
  world (the app hasn't opened, the page is still loading, a modal
  appeared), adapt. Never fire a step that depends on state that
  isn't currently observable.

* **CDP is foreground-independent.** When `cdp_bound=true` in
  RuntimeCaps, every CDP-routed action (`navigate`,
  `extract_with_fallback`, `cdp_eval`, and any `set_value` /
  `click` / `ax_action` with a `dom:*` target_id) lands in the
  CDP-bound page REGARDLESS of which desktop app is currently
  frontmost. If perception shows `APP: <some-IDE>` (Claude,
  Codex, VS Code, Terminal, …) or no AX elements at all, that
  does NOT mean the browser is broken — headless / non-frontmost
  Chrome doesn't appear in the AX tree. Trust the CDP action's
  `ok` result in history. Keep using `set_value` / `ax_action` /
  `click` (with `dom:*`) for in-page interactions per the Browser
  routing rule below — those go through CDP just like
  `cdp_eval` does, but the `dom:*` path is more reliable. The
  `RuntimeCaps` block names the bound browser and URL — that's
  the page you're driving, full stop, independent of `APP:`.

* **Never repeat a BANNED action.** The user prompt may include a
  `## BANNED ACTIONS` section listing exact action JSONs that have
  already failed. Emitting any of them again is a hard error — the
  runtime catches it and fails the run. When you see a banned
  action, pick a DIFFERENT approach (different target_id, different
  action type, keyboard fallback, Escape/Return to change state).

* **Pivot on fixation.** If the user prompt includes a
  `## FIXATION WARNING`, your previous 5 attempts all failed. Do
  NOT emit another batch that just rephrases what you already
  tried. Either (a) switch tool entirely (AX → keyboard, or browse →
  different URL, or desktop → a different app), or (b) emit Fail
  with a clear reason.

* **Small batches.** 1–5 steps per turn. After the batch runs you
  get called again with fresh state. Big commitments are a smell.

* **Done is graded by the runtime, not by your self-report.** When
  you emit Done, the runtime force-refreshes perception (forces a
  cortex tick to capture post-action state) and runs a separate
  grader pass against the fresh view + screenshot. If the grader
  decides the evidence doesn't support your claim, the runtime
  rejects the Done and you'll see a `runtime rejected Done: ...`
  attempt record on the next turn — at which point you should either
  gather the missing evidence or emit Fail. You don't need to add a
  "I verified the side-effect" preamble to every Done — the runtime
  is doing the verification for you. What you DO need to do: only
  emit Done when you actually believe the goal happened. Emitting
  Done speculatively to "see what the grader thinks" wastes a turn.

* **target_id rules.** For ax_action/set_value/click the target_id
  MUST appear verbatim in the perception below. NEVER invent a path
  or selector string (`ax:AXApplication/...`, `AXRole='AXButton'`,
  `ax:placeholder-X`, etc.). ALWAYS populate `label` + `role_hint`
  as a fallback.

* **Web data extraction.** To read a field from a page, use
  `extract_with_fallback` — NOT raw `cdp_eval` loops. Provide 2-4
  candidate selectors ordered strongest-to-weakest, plus a
  `parse_as` hint. The runtime tries them in order, parses, and
  writes the first match into `shared_memory[name]`. Advantages:
    - One action per field replaces N turns of synthesizing
      `document.querySelector(...)` and refining selectors when they
      miss.
    - `shared_memory[name]` gets a clean parsed value you can feed
      straight into `write_cells`.
    - If all selectors miss for the same `name` 3 times across the
      run, the runtime AUTO-NULLS the field and tells you via a
      history entry — that's your signal the page doesn't surface
      the data and you must move on with the rest of the goal. Do
      NOT try to bypass the auto-null by renaming the field.
  Reserve raw `cdp_eval` for situations the typed actions can't
  express — invoking page methods, reading computed styles, scrolling
  arbitrary distances, dispatching custom events. NOT for data reads
  (use `extract_with_fallback`) and NOT for clicks/typing on elements
  already in perception (use `set_value` / `ax_action` / `click` with
  the `dom:*` target_id — see Browser routing below).

* **Browser routing.** If APP is a browser, in-page interactions
  follow a strict precedence — pick the FIRST rule that applies:

  1. **`set_value` / `ax_action` / `click` with a `dom:*` target_id**
     when the perception list contains a matching `dom:*` element.
     This is the path for filling form fields, clicking known
     buttons, toggling checkboxes, selecting dropdown options. The
     runtime routes `dom:*` targets through CDP's JS-click /
     JS-set-value helpers — atomic, idempotent, and the id_part
     (`dom:input:email`, `dom:button:submit-btn`) carries the
     author's HTML `id`/`name`/`aria-label`, so dispatch finds the
     element by stable identifier rather than a guessed CSS selector.
     If perception shows `dom:input:email` for the email field, you
     MUST emit `set_value target_id="dom:input:email"` — not
     `cdp_eval` with `document.querySelector('#email').value=...`.
     The `cdp_eval` path is brittle (selectors break on framework
     re-renders), verbose (you're hand-writing JS), and bypasses the
     runtime's verification of the action.

  2. **`extract_with_fallback`** for reading data from the page.
     Already covered above — never use `cdp_eval` for data reads.

  3. **`cdp_eval`** ONLY when (1) and (2) don't apply: invoking page
     methods, dispatching custom events, scrolling to specific
     coordinates, reading computed styles, walking shadow DOM that
     perception didn't surface. Treat this as the escape hatch, not
     the default.
  The runtime will REFUSE `ax_action` and `click` with `ax:*`
  target_ids when the frontmost app is a browser (you'll see
  "runtime refuses" in history) — `ax:*` is for desktop apps, not
  web. The RuntimeCaps block above names which browser is CDP-bound
  — stay on that one.

  Navigation is `navigate` with a DIRECT URL — never type a URL into
  a search box and press Return, and never use the homepage + search
  workflow when you already know the target. Examples of direct URLs
  you should prefer:
    - Yahoo Finance ticker: https://finance.yahoo.com/quote/BTC-USD
      (substitute ETH-USD, SOL-USD, AAPL, etc.)
    - Yahoo Finance historical: https://finance.yahoo.com/quote/BTC-USD/history
    - Yahoo Finance news:       https://finance.yahoo.com/quote/BTC-USD/news
  Repeatedly navigating to the homepage and concluding "the page is
  wrong" is a stall pattern — use the per-asset URL directly.

* **Desktop routing.** For desktop apps use `ax_action` with label
  fallback, or prefer a key shortcut when no label is available.

* **Spreadsheet cell entry (Numbers).** USE `write_cells`. Do NOT
  `type` values into Numbers cells, and do NOT drive the cursor with
  arrow keys to land data. The keystroke recipe produces concatenated
  garbage (`"23.5023.502107251154"`), duplicated headers, and values
  in the wrong columns every time retries overlap — we have proof
  from every prior run.
  `write_cells` writes directly into the document model via
  AppleScript. It is atomic, idempotent, and returns per-cell
  readbacks when `verify: true` (default). The action shape:
    { "type":"write_cells",
      "app":"Numbers",
      "writes":[
        {"ref":"A1","value":"Ticker"},
        {"ref":"B1","value":"Price"},
        {"ref":"A2","value":"BTC"},   {"ref":"B2","value":"108432.50"},
        {"ref":"A3","value":"ETH"},   {"ref":"B3","value":"3852.11"},
        {"ref":"A4","value":"SOL"},   {"ref":"B4","value":"157.23"}
      ],
      "verify": true }
  Rules:
    - Pass raw numeric strings ("108432.50", not "$108,432.50").
      Numbers canonicalizes based on the cell's format.
    - One `write_cells` can carry many cells — batch them. One call
      per batch of related cells is far cheaper and more reliable
      than many single-cell calls.
    - Numbers must be running (use `activate_app: Numbers` first if
      it isn't). `write_cells` targets the first open document's
      first sheet and table by default.
    - On permission errors the runtime will explicitly say
      "AppleScript automation for Numbers not authorized" — that's a
      one-time user action (System Settings → Privacy & Security →
      Automation). Do not fall back to `type`; emit Fail with the
      permission message so the user can fix it.
    - When `verify: true`, the action result's `data.writes[i].readback`
      contains what Numbers actually stored. Use that to confirm each
      cell landed before emitting Done.
    - If AX does not clearly expose the cell values afterward, use
      `read_cells` to read them back from the Numbers document model
      instead of guessing from partial AX text.

* **Done REQUIRES direct perception evidence.** You may ONLY emit
  Done when the live perception or screenshot THIS TURN shows the
  goal state. Examples:
    - "Typed BTC into A1" → Done is allowed only if a cell element
      in perception has value="BTC".
    - "Saved the file" → Done is allowed only if the Save dialog is
      gone AND the window title reflects the saved name.
    - "Extracted prices" → Done is allowed only if `shared_memory`
      actually contains them (you wrote them via an extract step).
  Dispatching the right-looking actions is NOT evidence. If you
  can't verify the outcome, emit another batch that observes — an
  extract, a cdp_eval that reads the page, a wait + re-check.

* **Move on with partial data rather than fixate.** If you've spent
  3+ consecutive turns trying to extract the same piece of data
  without success, move on to the next part of the goal with
  whatever you have. Don't loop on refinement.

* **Commit to phases.** Multi-part goals (gather → organize → advise,
  or acquire → format → submit) have a phase order. Once you've
  entered a later phase, do NOT bounce back to an earlier one unless
  you're genuinely missing something you cannot proceed without.
  "More polish on phase 1 while phase 3 is untouched" is a failure
  mode. If the user prompt below shows a step budget past the
  midpoint, you are almost certainly in the wrong phase if you're
  still gathering — switch to the terminal app and start landing
  data. For the landing-into-Numbers phase, use `write_cells` — it's
  one action that writes many cells correctly. Batch everything
  you've gathered into a single `write_cells` call.

* **Fail criteria.** Only when genuinely stuck — missing permission,
  app refusing to launch, goal literally impossible given state.
  "I tried three clicks that failed" is NOT fail; try keyboard.
  "The page layout has changed and I cannot extract data after 5
  tries with different selectors" IS fail (or partial-Done).

* **Clarify criteria — narrow, not paranoid.** Clarify is the wrong
  tool for normal hard goals (use Fail), for goals that need more
  exploration (just take a step), or for goals against unfamiliar UIs
  (read the perception). Clarify is reserved for THREE specific cases:

    1. **Pronoun without antecedent.** The goal text uses "it", "that
       one", "this", or "the X" with no specific identifier AND
       perception shows multiple plausible targets. Example:
       goal = "Delete it" + dashboard shows ten rows → Clarify "which
       row?". NOT a clarify case: goal = "Delete the topmost row" or
       "Approve a deploy" — the qualifier resolves the referent (top
       row, any pending deploy).

    2. **Irreversible side-effect outside the named scope.** Anything
       that deletes user data, sends money, posts publicly, sends a
       message to many recipients, formats/wipes storage, or otherwise
       has a real-world consequence the user can't undo — AND the goal
       text doesn't explicitly authorise it. Example:
       goal = "Clean up the inbox" + the only obvious tool is a
       "Delete all" button → Clarify. NOT a clarify case: goal =
       "Mark this email as read", "Approve the deploy", "Acknowledge
       the alert", "Export the ticket to Notes", "Submit the form" —
       these are reversible or scoped operations the goal text
       explicitly authorises.

    3. **Required parameter the user clearly meant to supply.** Goal
       names a verb that demands a value the goal omits. Example:
       "Book a flight" (where to?), "Schedule a meeting" (when?),
       "Rename the file" (to what?). NOT a clarify case: the value is
       in perception (e.g. "the topmost pending row" + perception
       shows exactly one pending row), or the goal allows free choice
       ("write any test message").

  **Default stance: TRY first.** If the goal names a verb and a
  plausible target exists in perception, attempt it. A failed attempt
  becomes Fail (or a recoverable retry). A refused-out-of-caution
  goal is silently expensive — the user wrote the prompt expecting
  action; refusing inverts the contract.

  Shape:
    { "kind": "clarify", "question": "<one specific question>" }
  The question should be ONE focused ask — not a checklist. Example:
  goal = "Delete it"; the dashboard has multiple rows, none labeled
  "it" → `{"kind":"clarify","question":"Which item should I delete?
  I see several rows on the dashboard and 'it' is ambiguous."}`.
  Bad pattern (do not do this): goal = "Approve a deploy in the live
  queue" + perception shows pending deploys → Clarify "which one?".
  "A deploy" + the live queue context resolves the referent — pick
  the topmost pending row and act.

Output one JSON object. No prose, no markdown fences.
"#;

/// LLM-backed reactive plan producer.
pub struct LlmPlanProducer {
    client: Arc<LlmClient>,
    pub max_tokens: u32,
}

impl LlmPlanProducer {
    pub fn new(client: Arc<LlmClient>) -> Self {
        Self {
            client,
            max_tokens: 8192,
        }
    }
}

#[async_trait]
impl crate::canonical_plan_producer::PlanProducer for LlmPlanProducer {
    async fn decide_next(
        &self,
        goal: &str,
        history: &[AttemptRecord],
        shared_memory: &serde_json::Value,
        view: &PlanningView,
        screenshot_png: Option<&[u8]>,
    ) -> Result<NextMove, String> {
        let user = build_user_prompt(goal, history, shared_memory, view);
        let raw = if let Some(png) = screenshot_png {
            let data_url = format!("data:image/jpeg;base64,{}", cel_llm::base64_encode(png));
            self.client
                .complete_with_image(
                    NEXT_MOVE_SYSTEM_PROMPT,
                    &data_url,
                    &user,
                    self.max_tokens,
                    Some("auto"),
                )
                .await
                .map_err(|e| format!("decide_next (with image) failed: {}", llm_error_message(e)))?
        } else {
            let messages = vec![
                ChatMessage::text("system", NEXT_MOVE_SYSTEM_PROMPT),
                ChatMessage::text("user", &user),
            ];
            self.client
                .chat(messages, self.max_tokens)
                .await
                .map_err(|e| format!("decide_next failed: {}", llm_error_message(e)))?
        };
        parse_next_move_lenient(&raw)
            .map_err(|e| format!("decide_next parse failed: {e}\n--- raw ---\n{raw}"))
    }

    async fn verify_done(
        &self,
        goal: &str,
        summary: &str,
        shared_memory: &serde_json::Value,
        view: &PlanningView,
        screenshot_png: Option<&[u8]>,
    ) -> Result<crate::canonical_plan_producer::DoneVerdict, String> {
        let user = build_verify_done_user_prompt(goal, summary, shared_memory, view);
        let raw = if let Some(png) = screenshot_png {
            let data_url = format!("data:image/jpeg;base64,{}", cel_llm::base64_encode(png));
            self.client
                .complete_with_image(
                    VERIFY_DONE_SYSTEM_PROMPT,
                    &data_url,
                    &user,
                    512,
                    Some("auto"),
                )
                .await
                .map_err(|e| format!("verify_done (with image) failed: {}", llm_error_message(e)))?
        } else {
            let messages = vec![
                ChatMessage::text("system", VERIFY_DONE_SYSTEM_PROMPT),
                ChatMessage::text("user", &user),
            ];
            self.client
                .chat(messages, 512)
                .await
                .map_err(|e| format!("verify_done failed: {}", llm_error_message(e)))?
        };
        parse_verify_done_lenient(&raw)
            .map_err(|e| format!("verify_done parse failed: {e}\n--- raw ---\n{raw}"))
    }
}

pub const VERIFY_DONE_SYSTEM_PROMPT: &str = r#"
You are grading whether an agent's claim of "goal complete" is actually
supported by evidence. You see:

* The original goal.
* The agent's summary of what it claims to have accomplished.
* Any data the agent collected into shared_memory along the way.
* The CURRENT accessibility perception of the screen.
* A CURRENT screenshot (if provided).

Respond ONLY with JSON:
  { "verified": true | false, "reason": "<one-sentence why>" }

Rules:

* Be STRICT. A claim like "I entered BTC/ETH/SOL prices into Numbers"
  requires the perception or screenshot to actually show those three
  prices in the Numbers sheet in coherent form (not concatenated
  garbage, not duplicated, not missing).
* Multi-part goals (gather + organize + advise) require ALL parts to
  show evidence. Partial completion = verified: false with a reason
  naming what's missing.
* Data in shared_memory counts ONLY for parts of the goal that are
  about collecting data. It does NOT count for parts that are about
  landing data into an app's UI — that needs visual/AX evidence.
* If evidence is inconclusive, verified: false with the reason
  "inconclusive: <what additional observation would settle it>".
* Do not hallucinate evidence that isn't in the inputs.

Return ONLY the JSON object. No prose, no markdown fences.
"#;

fn build_verify_done_user_prompt(
    goal: &str,
    summary: &str,
    shared_memory: &serde_json::Value,
    view: &PlanningView,
) -> String {
    let mut out = String::new();
    out.push_str("## Goal\n");
    out.push_str(goal);
    out.push_str("\n\n## Agent's Done summary\n");
    out.push_str(summary);
    out.push_str("\n\n## shared_memory\n");
    out.push_str(&truncate(
        &serde_json::to_string(shared_memory).unwrap_or_else(|_| "{}".into()),
        1500,
    ));
    out.push_str("\n\n## Current perception (selected elements)\n");
    for el in view.elements.iter().take(40) {
        let label = el.label.clone().unwrap_or_default();
        let value = el.value.clone().unwrap_or_default();
        let line = format!(
            "- [{}] {}{}\n",
            el.element_type,
            label,
            if value.is_empty() {
                String::new()
            } else {
                format!(" = {}", truncate(&value, 60))
            }
        );
        out.push_str(&line);
    }
    out.push_str("\nReturn the JSON verdict.");
    out
}

fn parse_verify_done_lenient(
    raw: &str,
) -> Result<crate::canonical_plan_producer::DoneVerdict, String> {
    let trimmed = strip_code_fence(raw).trim();
    #[derive(serde::Deserialize)]
    struct Raw {
        verified: bool,
        #[serde(default)]
        reason: String,
    }
    let parsed: Raw = serde_json::from_str(trimmed).map_err(|e| {
        format!(
            "{e} (raw starts: {:?})",
            &trimmed.chars().take(80).collect::<String>()
        )
    })?;
    Ok(crate::canonical_plan_producer::DoneVerdict {
        verified: parsed.verified,
        reason: parsed.reason,
    })
}

fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("```json") {
        rest.trim_end_matches("```").trim()
    } else if let Some(rest) = s.strip_prefix("```") {
        rest.trim_end_matches("```").trim()
    } else {
        s
    }
}

fn llm_error_message(err: LlmError) -> String {
    format!("{err}")
}

/// Build the per-turn user prompt: goal, history, shared memory,
/// live perception. Kept compact so the planner doesn't drown in
/// irrelevant context.
///
/// The most important runner-level forcing function lives here: a
/// `BANNED ACTIONS` block that lists every action that has failed in
/// history. The LLM keeps re-emitting the same fake target_ids
/// otherwise — the history narrative apparently doesn't land hard
/// enough — so we also surface them as an explicit "do not repeat"
/// list it can't overlook.
pub fn build_user_prompt(
    goal: &str,
    history: &[AttemptRecord],
    shared_memory: &serde_json::Value,
    view: &PlanningView,
) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str("## Goal\n");
    out.push_str(goal.trim());
    out.push('\n');

    // Runtime capabilities block — tells the LLM what tools are
    // actually wired up. This prevents the very common failure mode
    // of "the planner emits ax_action when only CDP is bound, or
    // switches to Safari when the CDP target is Chrome". Keep this
    // short and declarative.
    let cdp_cap = view.capabilities.iter().find(|c| c.id == "cdp_bound");
    let native_cap = view.capabilities.iter().find(|c| c.id == "native_input");
    out.push_str("\n## Runtime capabilities\n");
    if let Some(cap) = cdp_cap {
        let browser = cap.detail.as_deref().unwrap_or("Chrome");
        out.push_str(&format!("  - CDP-controlled browser: {}\n", browser));
        if let Some(url) = &view.screen.url {
            out.push_str(&format!("  - Current page: {}\n", url));
        }
        out.push_str(
            "  - `navigate` and `cdp_eval` dispatch THROUGH THIS BROWSER ONLY.\n  \
             - DO NOT switch to Safari or any other browser. Our CDP is bound\n    \
               to the one named above. Actions on any other browser are blind.\n  \
             - Prefer `navigate` with a direct URL over typing into a search box.\n",
        );
    } else {
        out.push_str("  - No CDP-controlled browser bound. Skip `cdp_eval` and `navigate`.\n");
    }
    if native_cap.is_some() {
        out.push_str("  - Native input (keyboard / mouse / AX / activate_app) enabled.\n");
    } else {
        out.push_str(
            "  - Native input DISABLED (sandboxed eval mode). `ax_action`,\n    \
               `set_value`, `click`, `key`, `key_combo`, `type`, `activate_app`\n    \
               will be REFUSED by the runtime — pick cdp_eval-based actions.\n",
        );
    }

    let progress = &view.run_progress;
    if progress.max_steps > 0 {
        let remaining = progress.steps_remaining();
        let pct_used =
            (progress.steps_used as f32 / progress.max_steps as f32 * 100.0).round() as u32;
        out.push_str(&format!(
            "\n## Step budget\n  - Used {} / {} ({}%). Remaining: {}.\n",
            progress.steps_used, progress.max_steps, pct_used, remaining
        ));
        if remaining <= progress.max_steps / 4 {
            // Last 25% of budget — hard stop on new gathering.
            out.push_str(
                "  - You are in the FINAL QUARTER of the budget. STOP starting\n    \
                   new gathering/exploration. Commit to landing what you already\n    \
                   have into the goal's target surface (spreadsheet, doc, form)\n    \
                   and then emit Done with whatever's been accomplished — even\n    \
                   if partial. Running out of steps without a terminal is a\n    \
                   worse outcome than a partial Done.\n",
            );
        } else if remaining <= progress.max_steps / 2 {
            // Midpoint — pivot away from exploration.
            out.push_str(
                "  - You are past the midpoint. Start folding gathered data into\n    \
                   the goal's target surface. Don't open new threads of work\n    \
                   unless they're required for the goal.\n",
            );
        }
    }

    if !history.is_empty() {
        out.push_str("\n## What has happened so far (oldest first)\n");
        for (i, rec) in history.iter().enumerate().take(40) {
            let status = if rec.succeeded { "ok" } else { "err" };
            out.push_str(&format!(
                "{:>2}. [{}] {} → action={} {}\n",
                i + 1,
                status,
                truncate(&rec.step_purpose, 120),
                action_kind(&rec.action),
                rec.error
                    .as_deref()
                    .map(|e| format!("error=\"{}\"", truncate(e, 180)))
                    .unwrap_or_default()
            ));
        }
        if history.len() > 40 {
            out.push_str(&format!("  … and {} older entries\n", history.len() - 40));
        }

        // Banned actions: dedupe failed action JSONs; render the top N
        // so the LLM cannot miss them. This is the single most
        // effective forcing function against stuck-on-bad-selector
        // loops — it's hard to emit `target_id=X` after seeing "X
        // failed — do NOT emit it again".
        let banned = banned_action_strings(history, 8);
        if !banned.is_empty() {
            out.push_str("\n## BANNED ACTIONS (do NOT repeat — each failed when tried)\n");
            for b in &banned {
                out.push_str(&format!("  - {}\n", b));
            }
        }

        // Fixation guard: if the last several turns produced no new
        // successful actions, tell the LLM to pivot strategy entirely
        // (or emit Fail) rather than iterate on the same approach.
        let recent_wins = history.iter().rev().take(5).filter(|r| r.succeeded).count();
        if history.len() >= 5 && recent_wins == 0 {
            out.push_str(
                "\n## FIXATION WARNING\n\
                 The last 5 attempts produced no successful action. Either\n\
                 (a) pivot to a COMPLETELY different strategy (keyboard\n\
                 shortcuts, a different URL, a different app), or\n\
                 (b) emit {\"kind\":\"fail\",\"reason\":...} and stop.\n\
                 Do NOT emit another batch that rephrases the same idea.\n",
            );
        }

        // Stall guard: the agent may be firing the SAME successful
        // action over and over without actually advancing the goal
        // (e.g. repeatedly navigating to the same Yahoo Finance
        // homepage and concluding "wrong page" each time). The
        // fixation guard above only counts failures, so a
        // successful-but-pointless loop slips past it. Detect it by
        // checking whether the last N attempts share an identical
        // serialized action.
        let mut stall_count: u32 = 0;
        let mut stall_signature: Option<String> = None;
        for rec in history.iter().rev() {
            if !rec.succeeded {
                break;
            }
            let sig = serde_json::to_string(&rec.action).unwrap_or_default();
            if sig.is_empty() {
                break;
            }
            match &stall_signature {
                None => {
                    stall_signature = Some(sig);
                    stall_count = 1;
                }
                Some(prev) if prev == &sig => {
                    stall_count += 1;
                }
                _ => break,
            }
        }
        if stall_count >= 3 {
            let sig = stall_signature.as_deref().unwrap_or("");
            out.push_str(&format!(
                "\n## STALL WARNING\n\
                 You've emitted the SAME successful action {} turns in\n\
                 a row:\n  {}\n\
                 If repeating it worked you wouldn't need to repeat it.\n\
                 Either the action is a no-op for the goal (wrong URL,\n\
                 wrong app, page didn't actually change) or you need to\n\
                 change strategy. DO NOT emit this action again. Pick a\n\
                 DIFFERENT URL, a DIFFERENT action type, or pivot to the\n\
                 goal's next phase.\n",
                stall_count,
                truncate(sig, 220)
            ));
        }
    }

    if shared_memory.as_object().is_some_and(|o| !o.is_empty()) {
        out.push_str("\n## Shared memory (data you've extracted)\n");
        out.push_str(&format!(
            "  {}\n",
            truncate(
                &serde_json::to_string(shared_memory).unwrap_or_default(),
                800
            )
        ));
    }

    out.push_str("\n## Live perception\n");
    let app = if view.screen.active_app.is_empty() {
        "<none>"
    } else {
        view.screen.active_app.as_str()
    };
    out.push_str(&format!("APP: {}\nWINDOW: {}\n", app, view.screen.window));
    if let Some(summary) = &view.screen.summary {
        out.push_str(&format!("SUMMARY: {}\n", summary));
    }
    out.push_str("Selected elements:\n");
    let mut shown = 0;
    for el in view.elements.iter() {
        let label = el.label.as_deref().unwrap_or("");
        let value = el.value.as_deref().unwrap_or("");
        out.push_str(&format!(
            "  id={} type={} label={:?} value={:?}\n",
            el.id, el.element_type, label, value
        ));
        shown += 1;
    }
    if shown == 0 {
        out.push_str("  (no elements selected by the planning view)\n");
    }
    if view.omitted_counts.elements > 0 {
        out.push_str(&format!(
            "  … {} more elements omitted to fit the planning budget. Re-perceive if you need them.\n",
            view.omitted_counts.elements
        ));
    }
    if !view.blockers.is_empty() {
        out.push_str("\n## Blockers\n");
        for b in &view.blockers {
            out.push_str(&format!("  - [{}] {}", b.kind, b.description));
            if let Some(eid) = &b.element_id {
                out.push_str(&format!(" (element {})", eid));
            }
            out.push('\n');
        }
    }
    if !view.adapter_facts.is_empty() {
        out.push_str("\n## Adapter facts\n");
        for f in &view.adapter_facts {
            out.push_str(&format!("  - [{}/{}] {}\n", f.adapter, f.kind, f.payload));
        }
    }

    out.push_str("\nReturn the next move (batch / done / fail / clarify) as JSON now.");
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

/// Produce the "do NOT repeat" list — each entry is a one-line JSON
/// action that has failed in history. Deduped; capped at `limit`
/// (most recent first so the freshest errors win).
///
/// Excludes context-dependent actions (keypresses, targetless typing,
/// waits) whose outcome depends on which app is frontmost. Banning
/// those globally traps the agent — a Key("Down") that failed in
/// Chrome must still be available when focus moves to Numbers.
fn banned_action_strings(history: &[AttemptRecord], limit: usize) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for rec in history.iter().rev().filter(|r| !r.succeeded) {
        if !is_bannable(&rec.action) {
            continue;
        }
        let rendered = serde_json::to_string(&rec.action).unwrap_or_default();
        if rendered.is_empty() {
            continue;
        }
        if seen.insert(rendered.clone()) {
            out.push(truncate(&rendered, 240));
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

fn is_bannable(action: &crate::types::PlannedAction) -> bool {
    use crate::types::PlannedAction;
    match action {
        PlannedAction::Key { .. } | PlannedAction::KeyCombo { .. } | PlannedAction::Wait { .. } => {
            false
        }
        PlannedAction::Type { target_id, .. } => target_id.is_some(),
        _ => true,
    }
}

fn action_kind(action: &crate::types::PlannedAction) -> String {
    use crate::types::PlannedAction;
    match action {
        PlannedAction::Click { .. } => "click".into(),
        PlannedAction::Type { .. } => "type".into(),
        PlannedAction::Key { .. } => "key".into(),
        PlannedAction::KeyCombo { .. } => "key_combo".into(),
        PlannedAction::SetValue { .. } => "set_value".into(),
        PlannedAction::Scroll { .. } => "scroll".into(),
        PlannedAction::Drag { .. } => "drag".into(),
        PlannedAction::Wait { .. } => "wait".into(),
        PlannedAction::Custom { action, .. } => format!("custom:{action}"),
        PlannedAction::Extract { .. } => "extract".into(),
        PlannedAction::Batch { .. } => "batch".into(),
        PlannedAction::Act { .. } => "act".into(),
        PlannedAction::Done { .. } => "done".into(),
        PlannedAction::Fail { .. } => "fail".into(),
        PlannedAction::AxAction { .. } => "ax_action".into(),
        PlannedAction::ActivateApp { .. } => "activate_app".into(),
        PlannedAction::Select { .. } => "select".into(),
        PlannedAction::CdpEval { .. } => "cdp_eval".into(),
        PlannedAction::Navigate { .. } => "navigate".into(),
        PlannedAction::NotebookWrites { .. } => "notebook_writes".into(),
        PlannedAction::WriteCells { .. } => "write_cells".into(),
        PlannedAction::ReadCells { .. } => "read_cells".into(),
        PlannedAction::ExtractWithFallback { .. } => "extract_with_fallback".into(),
    }
}

/// Parse a NextMove JSON object, tolerant of markdown fences and prose.
fn parse_next_move_lenient(raw: &str) -> Result<NextMove, String> {
    let trimmed = raw.trim();
    let json_start = trimmed.find('{').ok_or("no `{` in LLM output")?;
    let json_end = trimmed.rfind('}').ok_or("no `}` in LLM output")?;
    let body = &trimmed[json_start..=json_end];
    let mv: NextMove = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if let NextMove::Batch { steps, .. } = &mv {
        if steps.is_empty() {
            return Err(
                "batch has no steps (if done, emit kind=done; if stuck, emit kind=fail)".into(),
            );
        }
    }
    Ok(mv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::StepKind;
    use crate::types::PlannedAction;

    #[test]
    fn parses_batch_next_move() {
        let raw = r#"{
          "kind": "batch",
          "purpose": "open the app",
          "steps": [
            {"purpose":"activate","kind":"deterministic","action":{"type":"activate_app","app_name":"Numbers"}}
          ]
        }"#;
        let mv = parse_next_move_lenient(raw).expect("parse");
        match mv {
            NextMove::Batch { purpose, steps } => {
                assert_eq!(purpose, "open the app");
                assert_eq!(steps.len(), 1);
            }
            other => panic!("expected batch, got {other:?}"),
        }
    }

    #[test]
    fn parses_done_next_move() {
        let raw = r#"{"kind":"done","summary":"BTC price: $75k"}"#;
        let mv = parse_next_move_lenient(raw).expect("parse");
        assert!(matches!(mv, NextMove::Done { .. }));
    }

    #[test]
    fn parses_fail_next_move() {
        let raw = r#"{"kind":"fail","reason":"Numbers refused to launch"}"#;
        let mv = parse_next_move_lenient(raw).expect("parse");
        assert!(matches!(mv, NextMove::Fail { .. }));
    }

    #[test]
    fn parses_clarify_next_move() {
        // The Clarify terminal — the planner emits this when the goal
        // is too ambiguous or destructive to attempt safely. Lock the
        // serde tag down so a rename can't silently break the prompt.
        let raw = r#"{"kind":"clarify","question":"Which item should I delete?"}"#;
        let mv = parse_next_move_lenient(raw).expect("parse");
        match mv {
            NextMove::Clarify { question } => {
                assert!(
                    question.contains("delete"),
                    "question should round-trip verbatim, got {question:?}"
                );
            }
            other => panic!("expected clarify, got {other:?}"),
        }
    }

    #[test]
    fn rejects_empty_batch() {
        let raw = r#"{"kind":"batch","purpose":"x","steps":[]}"#;
        let err = parse_next_move_lenient(raw).unwrap_err();
        assert!(err.contains("no steps"));
    }

    #[test]
    fn parses_with_markdown_fence() {
        let raw = "```json\n{\"kind\":\"done\",\"summary\":\"x\"}\n```";
        let mv = parse_next_move_lenient(raw).expect("parse");
        assert!(matches!(mv, NextMove::Done { .. }));
    }

    #[test]
    fn history_rendering_truncates_error() {
        let rec = AttemptRecord {
            step_purpose: "click Blank".into(),
            action: PlannedAction::Navigate {
                url: "https://x/".into(),
            },
            succeeded: false,
            error: Some("a".repeat(500)),
            data: serde_json::Value::Null,
        };
        let out = build_user_prompt(
            "do the thing",
            std::slice::from_ref(&rec),
            &serde_json::json!({}),
            &empty_view(),
        );
        assert!(out.contains("error=\""));
        assert!(
            out.contains("…"),
            "long error should be truncated with ellipsis"
        );
    }

    fn empty_view() -> PlanningView {
        PlanningView {
            goal: String::new(),
            budget: cel_contracts::PlanningBudget::default(),
            screen: cel_contracts::PlanningScreen::default(),
            elements: vec![],
            adapter_facts: vec![],
            capabilities: vec![],
            memories: vec![],
            knowledge: vec![],
            recent_events: vec![],
            blockers: vec![],
            anomalies: vec![],
            evidence: vec![],
            selection_rationale: None,
            omitted_counts: cel_contracts::OmittedCounts::default(),
            run_progress: cel_contracts::RunProgress::default(),
        }
    }

    #[test]
    fn prompt_renders_view_capabilities_and_run_progress() {
        let mut view = empty_view();
        view.capabilities.push(cel_contracts::CapabilityRef {
            id: "cdp_bound".into(),
            detail: Some("Google Chrome".into()),
        });
        view.capabilities.push(cel_contracts::CapabilityRef {
            id: "native_input".into(),
            detail: None,
        });
        view.screen.url = Some("https://example.com".into());
        view.run_progress = cel_contracts::RunProgress {
            steps_used: 7,
            max_steps: 80,
        };
        let out = build_user_prompt("any goal", &[], &serde_json::json!({}), &view);
        assert!(out.contains("CDP-controlled browser: Google Chrome"));
        assert!(out.contains("https://example.com"));
        assert!(out.contains("Native input"));
        assert!(out.contains("Used 7 / 80"));
    }

    #[test]
    fn prompt_signals_omitted_elements_so_planner_knows_view_was_compressed() {
        let mut view = empty_view();
        view.omitted_counts.elements = 431;
        let out = build_user_prompt("any goal", &[], &serde_json::json!({}), &view);
        assert!(
            out.contains("431 more elements omitted"),
            "prompt must surface omitted-count so the planner knows the view is compressed"
        );
    }

    #[test]
    fn prompt_renders_blockers_and_adapter_facts() {
        let mut view = empty_view();
        view.blockers.push(cel_contracts::Blocker {
            kind: "consent_wall".into(),
            description: "Cookie banner blocks page".into(),
            element_id: Some("dom:cookie-accept".into()),
        });
        view.adapter_facts.push(cel_contracts::AdapterFactRef {
            id: None,
            adapter: "numbers".into(),
            kind: "table".into(),
            payload: serde_json::json!({"sheet":"Sheet 1","rows":12,"cols":8}),
        });
        let out = build_user_prompt("any goal", &[], &serde_json::json!({}), &view);
        assert!(out.contains("[consent_wall]"));
        assert!(out.contains("dom:cookie-accept"));
        assert!(out.contains("[numbers/table]"));
    }

    #[test]
    fn step_kind_enum_preserved_in_batch() {
        // The LLM emits snake_case on the StepKind enum; make sure
        // the batch parser round-trips both variants.
        let raw = r#"{
          "kind":"batch","purpose":"mix",
          "steps":[
            {"purpose":"a","kind":"deterministic","action":{"type":"wait","ms":100}},
            {"purpose":"b","kind":"llm_assisted","action":{"type":"cdp_eval","expression":"1"}}
          ]
        }"#;
        let mv = parse_next_move_lenient(raw).unwrap();
        match mv {
            NextMove::Batch { steps, .. } => {
                assert_eq!(steps[0].kind, StepKind::Deterministic);
                assert_eq!(steps[1].kind, StepKind::LlmAssisted);
            }
            other => panic!("expected batch, got {other:?}"),
        }
    }
}
