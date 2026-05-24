//! LLM-backed reactive [`PlanProducer`] — decides the next move each
//! turn given goal + history + live perception + screenshot.
//!
//! One system prompt, one call shape. The runner loops; this producer
//! never commits past the next batch.

use std::sync::Arc;

use async_trait::async_trait;
use cel_contracts::{AdapterActionRef, PlanningView};
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
pub const NEXT_MOVE_SYSTEM_PROMPT: &str = r##"
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

  Optional `expect_after` on click / set_value / ax_action — the
  runtime polls the page after dispatch and FAILS the action if the
  expected post-state doesn't materialise within `timeout_ms`
  (default 2000). Four shapes:
    "expect_after": {"kind": "selector_appears",     "selector": "#success-message"}
    "expect_after": {"kind": "selector_disappears",  "selector": ".modal.open"}
    "expect_after": {"kind": "selector_text_contains","selector": "#status", "substring": "Approved"}
    "expect_after": {"kind": "dom_changed"}

  `dom_changed` is the fallback when you know the action SHOULD
  change SOMETHING visible but no single selector captures it
  (delete-row, tab-switch, "load more", submit→navigate). The
  runtime captures a before-snapshot at dispatch, polls until the
  page differs (text length, interactive element count, OR URL),
  and reports `EffectMissing` if nothing changed within the
  timeout. Strictly weaker than the selector-based variants — use
  those when you have a verbatim `selector="..."` from perception;
  use `dom_changed` when you don't.

  STRICT USAGE RULE — read carefully, this is the most-misused
  feature:

  Each element in perception comes with a precomputed
  `selector="..."` attribute (when the element has an HTML `id` or
  `data-testid`). That is the ONLY value you may use for
  `expect_after.selector` — paste it verbatim, no translation.

  Examples (correct):
    Perception line: `[5]<button label="Approve" selector="#btn-approve" />`
      → `"expect_after": {"kind": "selector_appears", "selector": "#btn-approve"}`
    Perception line: `[7]<button label="Approve" selector="[data-testid=\"approve-pgw\"]" />`
      → `"expect_after": {"kind": "selector_appears", "selector": "[data-testid=\"approve-pgw\"]"}`

  If the element you want to assert against has no
  `selector="..."` on its perception line, OMIT `expect_after`
  entirely. A missing expectation is STRICTLY BETTER than a
  hallucinated one — the runtime falls back to verify_done's
  screenshot grader at end-of-run.

  Do NOT invent CSS classes (`.success`, `.confirmation`,
  `.modal`, `.thank-you`, `.alert`, `.success-message`, etc.) or
  attribute-selector guesses (`[data-success]`, `[data-status]`).
  The runtime will strip your `expect_after` at parse time when
  the selector isn't in perception, so the bogus assertion does
  no work; but the planner that doesn't try in the first place
  saves a turn.

  Symptom you are misusing this feature: history shows
  `Stripped hallucinated expect_after — selector not in this
  turn's perception`. That means you invented a selector; just
  paste from `selector="..."` next time, or omit.

  Without `expect_after` the runtime reports `ok` whenever the
  click HANDLER ran — not whether the page reacted. That's still
  the right default unless you have a verbatim selector to assert
  against.
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

* **Done summaries carry the data.** When the goal is a question
  ("what number does it show?", "what is the price?", "how many
  results?") or asks to extract a value, the Done `summary` MUST
  state the literal answer. "Read the total counter" is a
  description of the work; "The total is 1000" is the answer. The
  grader looks for the value in the summary text — vague
  acknowledgements ("successfully read the counter") fail even when
  the value sits in shared_memory and the screenshot proves you
  reached the right page. For action goals (click X, submit Y),
  describe the side-effect ("Submitted the form, success banner is
  visible") rather than restating the goal verbatim.

* **Honor the grader's `next_action_hint`.** When a previous
  AttemptRecord in history has `next_action_hint = retry_last_action`
  (look for `HINT: re-emit your previous action` in the error
  string), your next batch MUST contain that exact action again.
  Do NOT emit a "verify state" / "check current state" batch — the
  runtime already verified the side-effect didn't materialise.
  Wasting a turn on verification when the grader explicitly asked
  for a retry is the exact failure mode this hint exists to prevent.
  Other hint values:
    - `different_action`  → switch verbs (click → cdp_eval-with-
      trusted-event, etc.); same target, different shape.
    - `different_target`  → re-read perception, find the element
      that actually corresponds to the goal, dispatch against THAT
      target_id (not the previous one).
    - `give_up`           → strongly consider emitting Fail with a
      specific reason — the grader believes the goal is
      unachievable from here.
  When you DO retry, attach `expect_after` to the retried action
  (see Slice 2's contract) so the runtime catches a second silent
  failure immediately rather than letting the next turn discover
  it via verify_done again.

* **target_id rules.** For ax_action/set_value/click the target_id
  MUST appear verbatim in the perception below. NEVER invent a path
  or selector string (`ax:AXApplication/...`, `AXRole='AXButton'`,
  `ax:placeholder-X`, etc.). ALWAYS populate `label` + `role_hint`
  as a fallback.

  For browser-DOM elements you have two valid forms — pick whichever
  matches perception:
  1. The bracket index `"5"` (matches the `[5]` shown in front of
     each element). The runtime resolves it to the real `dom:role:id`
     for you. Always safe — never out of date.
  2. The exact `dom:role:<id_part>` string. The `<id_part>` MUST come
     from a value the perception line surfaces — the `id="..."`
     attribute, `testid="..."` attribute, or (for inputs) `name`. Do
     NOT manufacture the id_part by slugifying the visible label —
     `<button id="btn-export">Export to Notes</button>` is
     `dom:button:btn-export`, NOT `dom:button:export-to-notes`. The
     dispatch path does an `id`/`name`/`placeholder`/`aria-label`
     substring search as a fallback, but a wrong guess fails noisily
     (`no-match:button:export-to-notes`) and burns a step. If the
     element has no `id="..."` and no `testid="..."` attribute on
     its perception line, use the bracket index — that is always
     correct.

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

  **Do NOT `navigate` to "go back" or "reload" the current page when
  perception already shows the elements you need.** If `cdp_current_url`
  in the runtime caps shows you are on the right page and the element
  table includes your target, a click that returns `no-match:...` means
  your `target_id` is wrong, NOT that the page is wrong. Re-read the
  perception line (`id="..."` / `testid="..."` is the verbatim id_part),
  use the bracket index as a fallback, or try a different element — do
  not navigate away and lose page state. Inventing a URL like
  `https://github.com/...` because the goal mentions a ticket number
  takes you away from the fixture page and burns the rest of your
  budget chasing your own re-navigations.

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
"##;

fn adapter_action_prompt_description(description: &str) -> String {
    const MAX_CHARS: usize = 240;
    let compact = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_CHARS {
        return compact;
    }
    let mut truncated = compact.chars().take(MAX_CHARS).collect::<String>();
    truncated.push('…');
    truncated
}

fn render_adapter_actions_prompt(actions: &[AdapterActionRef]) -> String {
    if actions.is_empty() {
        return String::new();
    }

    let mut sorted = actions.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| {
        left.adapter
            .cmp(&right.adapter)
            .then(left.action.cmp(&right.action))
    });

    let mut out = String::new();
    for action in sorted {
        let example = serde_json::json!({
            "type": "custom",
            "adapter": &action.adapter,
            "action": &action.action,
            "params": &action.params_schema,
        });
        out.push_str("  ");
        out.push_str(&serde_json::to_string(&example).unwrap_or_else(|_| "{}".into()));
        out.push('\n');

        let description = adapter_action_prompt_description(&action.description);
        out.push_str("    - ");
        if !description.is_empty() {
            out.push_str(&description);
            out.push(' ');
        }
        out.push_str(&format!(
            "[mutates_state={}, requires_verification={}, returns_data={}]\n",
            action.mutates_state, action.requires_verification, action.returns_data
        ));
    }
    out
}

/// Build the full system prompt for a planner turn.
///
/// Always starts with [`NEXT_MOVE_SYSTEM_PROMPT`] verbatim. When
/// `PlanningView::adapter_actions` is non-empty, this planner renders the
/// structured action catalogue at the planner boundary. If an older caller only
/// supplies `PlanningView::adapter_actions_prompt`, that pre-rendered fragment
/// remains a transitional fallback.
///
/// Without this section the LLM never learns about adapter routing: asked to
/// "draft an email" it falls through to GUI driving (Cmd+N, AX clicks on
/// Mail's compose form), which is exactly the failure mode the adapter system
/// exists to prevent.
fn build_system_prompt(view: &PlanningView) -> String {
    let rendered_adapter_actions = render_adapter_actions_prompt(&view.adapter_actions);
    let adapter_actions = if !rendered_adapter_actions.is_empty() {
        Some(rendered_adapter_actions.as_str())
    } else {
        view.adapter_actions_prompt
            .as_deref()
            .filter(|section| !section.is_empty())
    };

    match adapter_actions {
        Some(section) => {
            let mut s = String::with_capacity(NEXT_MOVE_SYSTEM_PROMPT.len() + section.len() + 128);
            s.push_str(NEXT_MOVE_SYSTEM_PROMPT);
            s.push_str("\n## App-Specific Actions\n");
            s.push_str(
                "These adapter actions are available for the current turn. \
                Prefer them over GUI keystrokes when the goal matches an \
                action's description — they bypass focus-loss / keystroke \
                fragility entirely. Use the exact shape shown.\n\n",
            );
            s.push_str(section);
            s
        }
        _ => NEXT_MOVE_SYSTEM_PROMPT.to_string(),
    }
}

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
        // Static base + per-turn adapter actions catalogue when present.
        // The runner stamps `view.adapter_actions` from the cortex's active
        // adapter manifests; this planner renders that structured catalogue
        // into the system prompt so `{"type":"custom", "adapter":"mail",
        // "action":...}` is a legal shape.
        let system = build_system_prompt(view);
        let raw = if let Some(png) = screenshot_png {
            let data_url = format!("data:image/jpeg;base64,{}", cel_llm::base64_encode(png));
            self.client
                .complete_with_image(&system, &data_url, &user, self.max_tokens, Some("auto"))
                .await
                .map_err(|e| format!("decide_next (with image) failed: {}", llm_error_message(e)))?
        } else {
            let messages = vec![
                ChatMessage::text("system", &system),
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
  {
    "verified":         true | false,
    "reason":           "<one-sentence why>",
    "next_action_hint": "retry_last_action" | "different_action" | "different_target" | "give_up" | null
  }

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

`next_action_hint` (set ONLY when verified=false; null when verified=true):

* "retry_last_action" — the agent's last action looks correct in
  shape (right target, right verb) but the page didn't react. The
  most useful next move is to re-emit the same action. Use this when
  perception shows the pre-state still in place (submit button still
  there, modal still open) AND the goal is something a single click /
  set_value would accomplish if it actually fired.
* "different_action" — the action shape is wrong. Same intent,
  different verb (e.g. switch from `click` to `cdp_eval`-with-
  trusted-event, or a key shortcut instead of a coordinate click).
* "different_target" — the action targeted the wrong element. The
  element it landed on isn't the one the goal needs (e.g. clicked
  "Approve" on the wrong row, set_value on a similarly-named-but-
  different field).
* "give_up" — the goal is unachievable from the current state. The
  planner should emit Fail rather than burn more budget.
* null — uncertain, or no clear hint applies. Default to null when
  in doubt — false positives here are worse than no signal.

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
    #[derive(serde::Deserialize)]
    struct Raw {
        verified: bool,
        #[serde(default)]
        reason: String,
        /// Optional categorical hint added in the Slice 3 grader
        /// prompt update. Older grader responses (or older parsers
        /// reading this raw shape) omit the field, defaulting to
        /// None.
        #[serde(default)]
        next_action_hint: Option<cel_contracts::NextActionHint>,
    }
    let candidate = extract_json_object(raw);
    match serde_json::from_str::<Raw>(&candidate) {
        Ok(parsed) => Ok(crate::canonical_plan_producer::DoneVerdict {
            verified: parsed.verified,
            reason: parsed.reason,
            next_action_hint: parsed.next_action_hint,
        }),
        Err(e) => {
            // Last-resort regex fallback for truncated / malformed
            // responses (seen with Gemini Flash: the model emits
            // `{"verified": false,\n` then hits its token cap before
            // closing the object). If we can recover the `verified`
            // boolean from a partial response, that's strictly more
            // useful than fail-open's "accept the Done". `reason` is
            // best-effort; `next_action_hint` falls back to None.
            if let Some(verdict) = extract_verdict_via_regex(raw) {
                tracing::warn!(
                    "verify_done JSON parse failed (\"{e}\") but regex \
                     fallback recovered verified={}",
                    verdict.verified,
                );
                return Ok(verdict);
            }
            Err(format!(
                "{e} (raw starts: {:?})",
                &candidate.chars().take(80).collect::<String>()
            ))
        }
    }
}

/// Try to pull a JSON object out of a free-form LLM response.
///
/// Handles the response shapes we've seen models produce:
///   * `{"verified": true, ...}` — clean JSON, return as-is.
///   * `\`\`\`json\n{...}\n\`\`\`` — markdown code fence around JSON.
///   * `Here's the verdict: {...}` — prose preamble + JSON.
///   * `{...} The reason is...` — JSON + trailing prose.
///   * Combination of the above.
///
/// Algorithm: strip code fences, then scan for the first `{`. If
/// found, walk forward tracking brace depth and quote state until
/// depth returns to zero (closing brace) or end-of-string. Return
/// the spanning substring. When no `{` is found, return the trimmed
/// input untouched — serde will give a clear error.
fn extract_json_object(raw: &str) -> String {
    let stripped = strip_code_fence(raw);
    let trimmed = stripped.trim();
    let Some(start) = trimmed.find('{') else {
        return trimmed.to_string();
    };
    let bytes = trimmed.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut end = trimmed.len();
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    trimmed[start..end].to_string()
}

/// Regex-style fallback for truncated grader responses. The grader
/// prompt asks for `{"verified": true|false, "reason": "...",
/// "next_action_hint": ...}`. When the model emits a truncated
/// response we can still recover at least the `verified` boolean
/// and (sometimes) the `reason` string by string-matching the keys.
///
/// Returns None when even the `verified` field isn't extractable
/// — at which point the caller surfaces the original parse error
/// and the fail-open path takes over.
fn extract_verdict_via_regex(raw: &str) -> Option<crate::canonical_plan_producer::DoneVerdict> {
    let verified = extract_bool_field(raw, "verified")?;
    let reason = extract_string_field(raw, "reason").unwrap_or_default();
    let hint_raw = extract_string_field(raw, "next_action_hint");
    let next_action_hint = hint_raw.and_then(|s| {
        // Same names as the NextActionHint enum's serde variants.
        match s.as_str() {
            "retry_last_action" => Some(cel_contracts::NextActionHint::RetryLastAction),
            "different_action" => Some(cel_contracts::NextActionHint::DifferentAction),
            "different_target" => Some(cel_contracts::NextActionHint::DifferentTarget),
            "give_up" => Some(cel_contracts::NextActionHint::GiveUp),
            _ => None,
        }
    });
    Some(crate::canonical_plan_producer::DoneVerdict {
        verified,
        reason,
        next_action_hint,
    })
}

/// Find `"<key>": true` or `"<key>": false` in free-form text.
///
/// Tolerant of whitespace and newlines between the key, the colon,
/// and the value. The grader sometimes emits compact JSON
/// (`"verified":true`), sometimes pretty-printed (`"verified": true`),
/// and very occasionally pretty-printed across a newline
/// (`"verified":\n  true`). Truncated responses also benefit:
/// `"verified":\n    true` followed by EOF is still recoverable.
fn extract_bool_field(text: &str, key: &str) -> Option<bool> {
    let after_colon = scan_past_key_and_colon(text, key)?;
    if after_colon.trim_start().starts_with("true") {
        Some(true)
    } else if after_colon.trim_start().starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Find `"<key>": "<value>"` in free-form text. Handles simple
/// strings; doesn't try to decode escapes (a backslash in the
/// fallback value is a corner-case worth accepting since this path
/// is best-effort recovery from a broken response anyway).
///
/// Tolerant of whitespace between the key, the colon, and the
/// opening quote — same rationale as `extract_bool_field`.
fn extract_string_field(text: &str, key: &str) -> Option<String> {
    let after_colon = scan_past_key_and_colon(text, key)?;
    let after_quote = after_colon.trim_start().strip_prefix('"')?;
    // Scan to the closing quote, skipping escaped quotes.
    let bytes = after_quote.as_bytes();
    let mut i = 0;
    let mut escape = false;
    while i < bytes.len() {
        if escape {
            escape = false;
            i += 1;
            continue;
        }
        if bytes[i] == b'\\' {
            escape = true;
            i += 1;
            continue;
        }
        if bytes[i] == b'"' {
            return Some(after_quote[..i].to_string());
        }
        i += 1;
    }
    None
}

/// Scan past `"<key>"` then optional whitespace then `:` then optional
/// whitespace. Returns the slice starting at the value (still on the
/// caller to parse `true` / `false` / `"..."` etc.). Returns None if
/// the key isn't present, or is present but not in object-key position
/// (no colon after it).
///
/// The key may appear multiple times in the input — once as the
/// actual JSON object key and once inside a string value
/// (`"reason": "verified the page"` contains the substring
/// `"verified"`). Walk through every occurrence and return the first
/// one that's actually followed by a colon; this rejects the
/// inside-string false positive without hand-coding string parsing.
fn scan_past_key_and_colon<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let mut start = 0;
    while let Some(rel) = text[start..].find(&needle) {
        let after = &text[start + rel + needle.len()..];
        let trimmed = after.trim_start();
        if let Some(rest) = trimmed.strip_prefix(':') {
            return Some(rest);
        }
        start = start + rel + needle.len();
    }
    None
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
            // Truncation budgets:
            // - succeeded rows: 180 chars is enough — "ok"-side errors
            //   are rare and short (recoverable-warning style).
            // - failed rows: 600 chars. The runtime's structured
            //   rejection messages name the AVAILABLE element ids,
            //   suggest the closest match, and tell the planner to
            //   re-read perception. Truncating at 180 amputated all
            //   that signal and the planner kept re-emitting the same
            //   hallucinated target. Eval evidence (server-runs/
            //   eval-2026-05-14): a 399-char "Available dom:* ids: …"
            //   list cut to 180 → planner sees the "your target was
            //   refused" verdict but NOT the suggested replacement.
            let err_max = if rec.succeeded { 180 } else { 600 };
            out.push_str(&format!(
                "{:>2}. [{}] {} → action={} {}\n",
                i + 1,
                status,
                truncate(&rec.step_purpose, 120),
                action_kind(&rec.action),
                rec.error
                    .as_deref()
                    .map(|e| format!("error=\"{}\"", truncate(e, err_max)))
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
    // Compact ID index — the model otherwise has to scan ~40 verbose
    // element lines to figure out which `dom:*` handles are
    // available this turn. Run-6 (Gemini Pro, 2026-05-19) showed the
    // planner constructing IDs like `dom:button:reject-cookies` that
    // never existed in perception (closest=None at rejection time);
    // listing the available IDs up front makes the next turn's
    // history hint redundant rather than corrective.
    //
    // Only emit when there's at least one `dom:` handle — the AX /
    // vision sources use different ID namespaces (`ax:`, `vis:`) and
    // listing those compactly would be noise without the same
    // hallucination problem.
    let dom_ids: Vec<&str> = view
        .elements
        .iter()
        .filter_map(|el| {
            if el.id.starts_with("dom:") {
                Some(el.id.as_str())
            } else {
                None
            }
        })
        .collect();
    if !dom_ids.is_empty() {
        out.push_str(&format!(
            "Available dom:* ids this turn ({}): {}\n",
            dom_ids.len(),
            dom_ids.join(", ")
        ));
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
        // For <select> elements, surface the enumerated option
        // values immediately under the element. The planner needs
        // these to dispatch `set_value` with an actual `value=`
        // attribute string rather than a guessed slug. Capped to a
        // reasonable line length — the encoded string is already
        // capped at 50 options from the extractor.
        if let Some(opts) = &el.select_options {
            out.push_str(&format!("      options: {}\n", truncate(opts, 600)));
        }
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
        return s.to_string();
    }
    // Walk to a char boundary at or below `max` so multibyte content
    // (emoji, CJK, Greek, …) doesn't panic. `s.is_char_boundary(max)`
    // already returns true on ASCII bytes so this is a no-op for the
    // common case. Surfaced by a messages.read_thread response that
    // included Greek text in a snippet.
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &s[..cut])
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
    fn verify_done_parser_extracts_next_action_hint_when_present() {
        let raw = r#"{
            "verified": false,
            "reason": "Send Message button still present and accessible — submission did not occur",
            "next_action_hint": "retry_last_action"
        }"#;
        let v = parse_verify_done_lenient(raw).expect("parse");
        assert!(!v.verified);
        assert!(v.reason.contains("Send Message"));
        assert_eq!(
            v.next_action_hint,
            Some(cel_contracts::NextActionHint::RetryLastAction)
        );
    }

    #[test]
    fn verify_done_parser_handles_missing_hint_back_compat() {
        // Pre-Slice-3 grader responses don't include `next_action_hint`.
        // The parser must still accept them — defaulting the field to
        // None — so any cached/stale grader behaviour keeps working.
        let raw = r#"{"verified": true, "reason": ""}"#;
        let v = parse_verify_done_lenient(raw).expect("parse");
        assert!(v.verified);
        assert!(v.next_action_hint.is_none());
    }

    #[test]
    fn verify_done_parser_handles_explicit_null_hint() {
        // The grader prompt says to emit `null` when uncertain. Confirm
        // explicit null parses as None (not as a parse error).
        let raw = r#"{
            "verified": false,
            "reason": "page is mid-transition; recheck after wait",
            "next_action_hint": null
        }"#;
        let v = parse_verify_done_lenient(raw).expect("parse");
        assert!(!v.verified);
        assert!(v.next_action_hint.is_none());
    }

    #[test]
    fn verify_done_parser_strips_prose_preamble() {
        // Gemini Flash sometimes emits prose before the JSON. The
        // extractor should scan past it to the first `{` and parse
        // the rest.
        let raw = r#"Sure, here's the verdict:

{"verified": true, "reason": "All good"}

Hope that helps!"#;
        let v = parse_verify_done_lenient(raw).expect("parse");
        assert!(v.verified);
        assert_eq!(v.reason, "All good");
    }

    #[test]
    fn verify_done_parser_handles_nested_braces_in_reason() {
        // Don't stop at the first `}` — track brace depth properly.
        // A reason string can contain unrelated braces in transcript
        // snippets, and `next_action_hint` is itself a nested value.
        let raw = r#"{
            "verified": false,
            "reason": "found a stray { in the page text",
            "next_action_hint": null
        }"#;
        let v = parse_verify_done_lenient(raw).expect("parse");
        assert!(!v.verified);
        assert!(v.reason.contains("stray {"));
    }

    #[test]
    fn verify_done_parser_regex_fallback_recovers_from_truncation() {
        // The 2026-05-14 server-eval log caught Gemini emitting:
        //     {
        //       "verified": false,
        // ...and stopping there, mid-JSON. Strict parsing fails;
        // the regex fallback recovers `verified=false` so the
        // grader signal isn't lost to a truncated response.
        let raw = "{\n  \"verified\": false,\n";
        let v =
            parse_verify_done_lenient(raw).expect("regex fallback should recover from truncation");
        assert!(!v.verified);
        // `reason` defaults to empty when the field is absent /
        // unrecoverable; that's strictly better than fail-open
        // accepting the Done.
        assert_eq!(v.reason, "");
    }

    #[test]
    fn verify_done_parser_regex_fallback_recovers_reason_when_complete() {
        // Truncation after `"reason"` field but before close-brace:
        // strict parse fails, regex picks up both `verified` and
        // the partially-present `reason`.
        let raw = r#"{
            "verified": true,
            "reason": "submitted form successfully",
            "next_action_hint": "retry_last_action""#;
        // Note: no closing `}` — truncated.
        let v = parse_verify_done_lenient(raw).expect("parse via fallback");
        assert!(v.verified);
        assert_eq!(v.reason, "submitted form successfully");
        assert_eq!(
            v.next_action_hint,
            Some(cel_contracts::NextActionHint::RetryLastAction)
        );
    }

    #[test]
    fn regex_fallback_tolerates_whitespace_around_colon() {
        // Gemini Pro occasionally emits pretty-printed JSON with a
        // space before the colon: `"verified" : true`. The original
        // `text.contains("\"verified\": true")` check missed this,
        // so even a fully-valid (but unusually-spaced) truncated
        // response would fail the regex fallback and force the
        // caller into fail-open. Run-6 (2026-05-19) didn't hit this
        // exact pattern, but multiple verify_done EOF errors came
        // close enough that loosening the matcher is worth doing.
        let raw = r#"{"verified" : true, "reason" : "ok"}"#;
        let v = parse_verify_done_lenient(raw).expect("parse");
        assert!(v.verified);
        assert_eq!(v.reason, "ok");
    }

    #[test]
    fn regex_fallback_tolerates_newline_between_key_and_value() {
        // Truncation pattern observed in the wild: `"verified":\n`
        // followed by indented value. Older fallback required the
        // value on the same line as the key.
        let raw = "{\n  \"verified\":\n    true,\n  \"reason\":\n    \"recovered\"\n}";
        let v = parse_verify_done_lenient(raw).expect("parse");
        assert!(v.verified);
        assert_eq!(v.reason, "recovered");
    }

    #[test]
    fn regex_fallback_skips_inside_string_occurrence_of_key() {
        // Adversarial: the reason field happens to contain the
        // literal `"verified"` substring (e.g. the grader is
        // explaining what `"verified"` means). The scanner must
        // identify the REAL key (the one followed by a colon and a
        // bool) rather than confusing itself with the in-string
        // mention. Strict parsing handles this fine; the fallback
        // must too — otherwise truncating right after the real
        // key would silently misread the verdict.
        let raw = "{\n  \"reason\": \"the term \\\"verified\\\" means the action's effect held\",\n  \"verified\": false\n}";
        let v = parse_verify_done_lenient(raw).expect("parse");
        assert!(
            !v.verified,
            "must read the actual key, not the in-string mention"
        );
    }

    #[test]
    fn extract_json_object_walks_brace_depth() {
        // Multiple nested objects — scanner must walk to the
        // matching outer brace, not the first close-brace.
        let raw = r#"prelude {"a": {"b": "c"}, "d": 1} trailing"#;
        let extracted = extract_json_object(raw);
        assert_eq!(extracted, r#"{"a": {"b": "c"}, "d": 1}"#);
    }

    #[test]
    fn extract_json_object_handles_strings_with_braces() {
        // A brace inside a string literal isn't a JSON close-brace.
        let raw = r#"{"text": "looks like a { brace"}"#;
        let extracted = extract_json_object(raw);
        assert_eq!(extracted, raw);
    }

    #[test]
    fn extract_json_object_handles_escaped_quotes_in_strings() {
        let raw = r#"{"text": "an escaped \" inside"}"#;
        let extracted = extract_json_object(raw);
        assert_eq!(extracted, raw);
    }

    #[test]
    fn verify_done_parser_handles_all_hint_variants() {
        for (label, raw_hint, expected) in [
            (
                "retry",
                "retry_last_action",
                cel_contracts::NextActionHint::RetryLastAction,
            ),
            (
                "different action",
                "different_action",
                cel_contracts::NextActionHint::DifferentAction,
            ),
            (
                "different target",
                "different_target",
                cel_contracts::NextActionHint::DifferentTarget,
            ),
            ("give up", "give_up", cel_contracts::NextActionHint::GiveUp),
        ] {
            let raw = format!(
                r#"{{"verified":false,"reason":"x","next_action_hint":"{}"}}"#,
                raw_hint
            );
            let v = parse_verify_done_lenient(&raw).expect(label);
            assert_eq!(v.next_action_hint, Some(expected), "label={label}");
        }
    }

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
    fn history_rendering_truncates_failed_error_at_600() {
        // Failed rows get a 600-char window (bumped from 180) so the
        // runtime's structured rejection messages — which name the
        // AVAILABLE element ids and the closest match — reach the
        // planner intact. Strings longer than 600 still get the
        // ellipsis treatment.
        let rec = AttemptRecord {
            step_purpose: "click Blank".into(),
            action: PlannedAction::Navigate {
                url: "https://x/".into(),
                wait_until: None,
                timeout_ms: None,
                dismiss_overlays: None,
            },
            succeeded: false,
            error: Some("a".repeat(900)),
            data: serde_json::Value::Null,
            next_action_hint: None,
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
            "900-char failed-row error should be truncated with ellipsis"
        );
    }

    #[test]
    fn history_rendering_preserves_full_rejection_message_under_600() {
        // The motivating real-world failure: the runtime rejects a
        // hallucinated dom:* target and packs the recovery hint
        // ("Available dom:* ids: ...", "Closest match: ...") into
        // ~400 chars. The old 180-char window amputated this signal.
        // Pin the new behaviour: a ~400-char failed-row message
        // round-trips intact.
        let realistic = "runtime refused: target_id \"dom:button:purge-all-user-sessions\" \
             is not in the current perception. Closest match in this turn's perception: \
             \"dom:button:purge-all-sessions\" — use that id verbatim if it's the element \
             you meant. Pick a verbatim id from this turn's element table (the [N] bracket \
             index is always safe), or a different action. Available dom:* ids: \
             dom:button:purge-all-sessions, dom:button:cancel, dom:input:reason";
        assert!(realistic.len() < 600, "test fixture should fit the window");
        let rec = AttemptRecord {
            step_purpose: "Click Purge button".into(),
            action: PlannedAction::Click {
                target_id: "dom:button:purge-all-user-sessions".into(),
                expect_after: None,
            },
            succeeded: false,
            error: Some(realistic.to_string()),
            data: serde_json::Value::Null,
            next_action_hint: Some(cel_contracts::NextActionHint::DifferentTarget),
        };
        let out = build_user_prompt(
            "purge sessions",
            std::slice::from_ref(&rec),
            &serde_json::json!({}),
            &empty_view(),
        );
        // History block must contain the actual suggested closest-match
        // id (this is the single piece of info the planner needs to
        // recover with a one-token edit).
        assert!(
            out.contains("dom:button:purge-all-sessions"),
            "the suggested closest-match id must survive history rendering, \
             got: {out}"
        );
        assert!(
            out.contains("Closest match"),
            "the 'Closest match' label must survive"
        );
        assert!(
            out.contains("Available dom:* ids"),
            "the suggestions list label must survive"
        );
    }

    #[test]
    fn history_rendering_still_truncates_successful_rows_at_180() {
        // Successful rows keep the conservative 180-char window — "ok"
        // entries rarely carry meaningful errors and we don't want
        // every successful turn padding the prompt with junk.
        let rec = AttemptRecord {
            step_purpose: "click Blank".into(),
            action: PlannedAction::Navigate {
                url: "https://x/".into(),
                wait_until: None,
                timeout_ms: None,
                dismiss_overlays: None,
            },
            succeeded: true,
            error: Some("b".repeat(400)),
            data: serde_json::Value::Null,
            next_action_hint: None,
        };
        let out = build_user_prompt(
            "do the thing",
            std::slice::from_ref(&rec),
            &serde_json::json!({}),
            &empty_view(),
        );
        assert!(
            out.contains("…"),
            "400-char succeeded-row error should still truncate at 180"
        );
    }

    fn empty_view() -> PlanningView {
        PlanningView {
            goal: String::new(),
            budget: cel_contracts::PlanningBudget::default(),
            screen: cel_contracts::PlanningScreen::default(),
            elements: vec![],
            adapter_facts: vec![],
            adapter_actions: vec![],
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
            adapter_actions_prompt: None,
        }
    }

    #[test]
    fn system_prompt_prefers_structured_adapter_actions() {
        let mut view = empty_view();
        view.adapter_actions_prompt = Some("legacy adapter prompt".into());
        view.adapter_actions.push(cel_contracts::AdapterActionRef {
            adapter: "mail".into(),
            action: "compose".into(),
            params_schema: std::collections::BTreeMap::from([
                ("body".into(), "string".into()),
                ("to".into(), "string|string[]".into()),
            ]),
            description: "Create a draft without sending it.".into(),
            mutates_state: true,
            requires_verification: false,
            returns_data: true,
        });

        let out = build_system_prompt(&view);
        assert!(out.contains("## App-Specific Actions"));
        assert!(out.contains(r#""type":"custom""#));
        assert!(out.contains(r#""adapter":"mail""#));
        assert!(out.contains(r#""action":"compose""#));
        assert!(out.contains(r#""body":"string""#));
        assert!(out.contains("mutates_state=true"));
        assert!(out.contains("requires_verification=false"));
        assert!(out.contains("returns_data=true"));
        assert!(!out.contains("legacy adapter prompt"));
    }

    #[test]
    fn system_prompt_falls_back_to_transitional_adapter_prompt() {
        let mut view = empty_view();
        view.adapter_actions_prompt = Some("  legacy-section\n".into());

        let out = build_system_prompt(&view);
        assert!(out.contains("## App-Specific Actions"));
        assert!(out.contains("legacy-section"));
    }

    #[test]
    fn system_prompt_omits_adapter_section_when_no_actions_exist() {
        let out = build_system_prompt(&empty_view());
        assert!(!out.contains("## App-Specific Actions"));
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
    fn prompt_lists_available_dom_ids_compactly() {
        // Run-6 (2026-05-19, Gemini Pro) showed the planner
        // constructing IDs like `dom:button:reject-cookies` that
        // never existed in this turn's perception. The Levenshtein
        // closest-match hint fires AT REJECTION TIME, after the
        // model already burned a turn on a bad target. The compact
        // ID index in `## Live perception` is the pre-rejection
        // signal: with the available `dom:*` handles listed up
        // front, the model should pick from them instead of
        // hallucinating.
        let mut view = empty_view();
        view.elements.push(cel_contracts::PlanningElement {
            id: "dom:button:accept-all".into(),
            element_type: "button".into(),
            label: Some("Accept All".into()),
            value: None,
            state: cel_contracts::PlanningElementState::default(),
            clickable: true,
            settable: false,
            select_options: None,
        });
        view.elements.push(cel_contracts::PlanningElement {
            id: "dom:input:email".into(),
            element_type: "input".into(),
            label: Some("Email".into()),
            value: None,
            state: cel_contracts::PlanningElementState::default(),
            clickable: false,
            settable: true,
            select_options: None,
        });
        // An AX-sourced element with non-`dom:` id — must not pollute
        // the dom-id list (different namespace, different hallucination
        // mode).
        view.elements.push(cel_contracts::PlanningElement {
            id: "ax:button:1".into(),
            element_type: "button".into(),
            label: Some("Native button".into()),
            value: None,
            state: cel_contracts::PlanningElementState::default(),
            clickable: true,
            settable: false,
            select_options: None,
        });

        let out = build_user_prompt("any goal", &[], &serde_json::json!({}), &view);
        assert!(
            out.contains("Available dom:* ids this turn (2): dom:button:accept-all, dom:input:email"),
            "compact id list must appear with only the dom:* handles, in perception order. Got:\n{}",
            out
        );
        assert!(
            !out.contains(", ax:button:1"),
            "ax:* ids must NOT appear in the dom:* index"
        );
    }

    #[test]
    fn prompt_omits_dom_id_list_when_only_non_dom_elements_present() {
        // AX-only perception (no browser bound) shouldn't show an
        // empty "Available dom:* ids" header — that'd be confusing
        // noise. The omission is silent.
        let mut view = empty_view();
        view.elements.push(cel_contracts::PlanningElement {
            id: "ax:button:42".into(),
            element_type: "button".into(),
            label: Some("Native".into()),
            value: None,
            state: cel_contracts::PlanningElementState::default(),
            clickable: true,
            settable: false,
            select_options: None,
        });

        let out = build_user_prompt("any goal", &[], &serde_json::json!({}), &view);
        assert!(
            !out.contains("Available dom:* ids"),
            "header must be omitted when there are no dom:* ids"
        );
    }

    #[test]
    fn prompt_surfaces_select_options_under_the_select_element() {
        // Run-6 (2026-05-19) caught the contact-form scenarios
        // failing 3/3 trials because the planner guessed slugs like
        // `Test` or `general-inquiry` for a `<select name="subject">`
        // whose actual option values were different. The browser
        // extractor now captures option pairs and the planning view
        // carries them through; the planner prompt must render them
        // under the element so the model can copy the right value
        // string verbatim.
        let mut view = empty_view();
        view.elements.push(cel_contracts::PlanningElement {
            id: "dom:select:subject".into(),
            element_type: "select".into(),
            label: Some("Subject".into()),
            value: None,
            state: cel_contracts::PlanningElementState::default(),
            clickable: false,
            settable: true,
            select_options: Some(
                "general_inquiry|General Inquiry, bug_report|Bug Report, feature|Feature Request"
                    .into(),
            ),
        });

        let out = build_user_prompt("any goal", &[], &serde_json::json!({}), &view);
        assert!(
            out.contains("id=dom:select:subject"),
            "select element itself must render"
        );
        assert!(
            out.contains("options: general_inquiry|General Inquiry"),
            "first option pair must appear under the element. Got:\n{}",
            out
        );
        assert!(
            out.contains("bug_report|Bug Report"),
            "subsequent option pairs must appear"
        );
    }

    #[test]
    fn prompt_omits_options_line_when_select_has_none() {
        // A `<select>` with zero options (e.g. dynamically populated
        // via JS that hasn't fired yet) must NOT emit an empty
        // `options:` line — the planner would treat that as
        // "options exist but are empty", which is misleading.
        let mut view = empty_view();
        view.elements.push(cel_contracts::PlanningElement {
            id: "dom:select:country".into(),
            element_type: "select".into(),
            label: Some("Country".into()),
            value: None,
            state: cel_contracts::PlanningElementState::default(),
            clickable: false,
            settable: true,
            select_options: None,
        });

        let out = build_user_prompt("any goal", &[], &serde_json::json!({}), &view);
        assert!(out.contains("id=dom:select:country"));
        assert!(
            !out.contains("options:"),
            "must not emit options: line when select_options is None"
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
