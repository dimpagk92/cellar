# External Agent Improvements

Plan for making CEL clean to drive from external agents (Claude Code, langgraph, cursor, codex) over MCP — written 2026-05-12 after a real session exposed friction.

## Why this exists

CEL today optimizes well for its *internal* agent (`run_goal`): focus stable across actions, primitives composed in-process, no yielding between tool calls. External agents that drive CEL via MCP have a different shape — every action is a separate round-trip, focus can shift to the host's window between calls, and there's no way to bind a sequence to a target app. The fragility this creates is consistent across hosts.

A representative failure from the 2026-05-12 session:
- Goal: edit an Apple Note's body to fix formatting.
- External agent issued `cel_act` calls one-at-a-time over MCP.
- Between calls, the host's window (Claude Code) regained focus.
- Subsequent `Cmd+A` / `Cmd+F` / `type` keystrokes landed in the host's chat input rather than Notes.
- Resulted in: typed text in wrong window, an accidentally deleted note (recovered by `Cmd+Z`), and a follow-up rewrite that appended-instead-of-replaced because cursor positioning drifted.

CEL's internal agent doesn't see this because it doesn't yield focus mid-sequence. The goal is to expose the same guarantees external agents need.

## Out of scope (already chipped or not needed)

- `cel_act navigate` — **not needed.** `cdp_eval` with `window.location.href = URL` is the right primitive; semantically identical to `cel.cdpNavigate`. The "new windows on nav" symptom was caused by (a) repeated `cellar browser ensure` relaunches and (b) site-side popunders, not by `cdp_eval`.
- `write_cells` auto-verify bug in Numbers adapter — already chipped, see task `Fix write_cells auto-verify bug in Numbers adapter`.
- `cellar browser cleanup` for stray about:blank tabs — already chipped, see task `Add stray-blank-tab cleanup to CEL browser CLI`.

## Layer 1: New `cel_act` primitives

Three new actions on the `cel_act` MCP tool. Highest ROI first.

### 1.1 `focus_lock` / `focus_release`

**Problem:** between MCP tool calls, the host's window regains focus. Multi-step sequences (find → position → select → delete → type) routinely have actions land in the wrong app.

**Action shape:**
```typescript
{ action: "focus_lock", app_name: "Notes" }
{ action: "focus_release" }
```

**Semantics:** while a focus_lock is active, every subsequent `cel_act` operation re-asserts focus on `app_name` immediately before executing. If activation fails (app quit, locked screen), the action errors rather than executing on the wrong target. `focus_release` (or process exit, or 5-minute timeout) drops the lock.

**Implementation notes:**
- State stored in the cel-napi singleton or the cortex session if active.
- The re-activation is a cheap `NSWorkspace activate` or AppleScript `activate`; sub-100ms.
- If a `focus_lock` is held and a `cel_act` arrives for a different app's element (by AX id from a previous `cel_see`), reject with a clear error message: "focus is locked to X; release or change lock target."
- Lock survives the implicit ax-tree refresh that `cel_see context` does.

**Files (estimated):**
- `mcp-server/src/tools/cel-act.ts` — new schema variants, dispatcher cases
- `agent/src/cel-bindings.ts` — bindings for focusLock/focusRelease
- `cel/cel-napi/src/focus.rs` (new) — native focus-lock state + re-assertion logic
- Tests: integration test that opens two apps, asserts keystrokes land in locked app even after activating the other.

### 1.2 `select_between { start_anchor, end_anchor }`

**Problem:** to select a paragraph in a rich-text editor (Notes, Mail compose, TextEdit), external agents must position cursor (Find + Esc), then extend selection with arrow keys (count visual lines — depends on window width and font), then delete. Fragile and slow.

**Action shape:**
```typescript
{
  action: "select_between",
  start_anchor: "Market thoughts — May 2026 Source",  // first occurrence after current cursor
  end_anchor: "this dataset.",                          // first occurrence after start
  include_end: true,                                    // include end_anchor text in selection
  scope: "focused_element" | "document"                 // default "document"
}
```

**Semantics:** uses the focused app's Find facility (Cmd+F + type + Return + Esc) twice — once to position cursor at start_anchor, mark that position; once to find end_anchor; then issues `Shift+Cmd+ArrowRight` or equivalent to extend selection from start to end position. Returns the selected text for caller verification.

**Implementation notes:**
- For AX-accessible text fields, can bypass Find entirely: read full value, compute character offsets of anchors, issue `AXSelectedTextRange` set via accessibility API.
- For non-AX (rich-text editors like Notes body), fall back to Find-based positioning.
- Error if start_anchor not found, end_anchor not found, or end appears before start.

**Files:**
- `mcp-server/src/tools/cel-act.ts`
- `cel/cel-napi/src/select.rs` (new) — selection helpers
- `agent/src/cel-bindings.ts`
- Tests: against TextEdit (AX-friendly) and Notes (AX-hostile) — same API both ways.

### 1.3 `text_replace_in_focused { find, replace, occurrence }`

**Problem:** the common case — "replace X with Y in the current document" — needs many primitives chained. Most rich-text editors have no API for this.

**Action shape:**
```typescript
{
  action: "text_replace_in_focused",
  find: "Market thoughts — May 2026 Source",
  replace: "Source",
  occurrence: "first" | "all"  // default "first"
}
```

**Semantics:** uses `select_between` (or AX direct manipulation if available) internally to find and replace. For `all`, iterates until no more matches.

**Implementation notes:**
- Layer on top of `select_between` + `type` once selection is active.
- For AX-direct path (TextEdit, search fields, form inputs): use `AXValue` set directly.
- Document explicitly: this is a "best-effort" primitive — semantics depend on the app's text engine. Surface app-detection in the response: `{ replaced: true, method: "ax" | "find_and_select" | "failed" }`.

**Files:**
- `mcp-server/src/tools/cel-act.ts`
- `agent/src/text-ops.ts` (new) — composition layer
- Tests: TextEdit, Notes, Mail compose, browser textarea (via CDP fallback).

## Layer 2: More adapters (the Numbers pattern, broadened)

CEL's `adapters/` directory holds structured-truth adapters (currently Numbers, Excel, SAP-GUI, Bloomberg, MetaTrader, browser-rs). Each exposes operations that bypass UI driving entirely — they speak the app's native API (AppleScript, COM, web API, etc.) and surface results as `cel_act` actions.

Apps to add, ranked by how often they appear in real external-agent tasks:

### 2.1 Notes adapter

**Native surface:** Apple Notes has a stable AppleScript dictionary. Operations like "create note with body", "set body of note", "find note by name" map 1:1 to AS commands.

**Adapter ops (proposed):**
- `notes.create_note { title, body, folder?, account? }` → note id
- `notes.set_body { note_id, body, format: "plain" | "html" }`
- `notes.append { note_id, text }`
- `notes.get_body { note_id }` → returns the current body
- `notes.list { folder?, account?, limit? }` → array of {id, title, modified_at, snippet}
- `notes.find { query }` → matching notes
- `notes.delete { note_id }`

**Why this matters:** would have replaced *all* of the keystroke automation in the 2026-05-12 session's Notes step with one `notes.set_body` call. No focus issues, no cursor positioning, no Cmd+A panic.

**Files (mirror `adapters/numbers/` structure):**
- `adapters/notes/Cargo.toml`
- `adapters/notes/src/lib.rs` (~250 lines, AppleScript dispatch)
- `adapters/notes/src/applescript.rs` (helpers)
- Tests under `adapters/notes/tests/`

### 2.2 Mail adapter

**Native surface:** Apple Mail has AppleScript dictionary; macOS 14+ also has Mail intents.

**Adapter ops:**
- `mail.compose { to, cc?, bcc?, subject, body, attachments? }` → draft id
- `mail.send_draft { draft_id }`
- `mail.list_inbox { since?, from?, limit? }` → message list
- `mail.read_message { message_id }` → body, headers
- `mail.search { query, folder?, limit? }`

**Risk:** sending mail is irreversible. Require explicit `send_draft` step after `compose`. Never auto-send.

### 2.3 Calendar adapter

**Native surface:** AppleScript + EventKit framework.

**Adapter ops:**
- `calendar.create_event { calendar, title, start, end, attendees?, notes? }`
- `calendar.list_events { calendar?, start, end }`
- `calendar.delete_event { event_id }`
- `calendar.update_event { event_id, ... }`

### 2.4 Reminders adapter

**Native surface:** AppleScript dictionary.

**Adapter ops:**
- `reminders.add { list, title, due?, notes? }`
- `reminders.list { list?, completed?, due_before? }`
- `reminders.complete { reminder_id }`
- `reminders.delete { reminder_id }`

### 2.5 Messages adapter (read-only first)

**Native surface:** Messages.app exposes a SQLite database at `~/Library/Messages/chat.db` (read access works without permissions on most setups; write requires entitlements).

**Adapter ops (read-only):**
- `messages.list_threads { limit? }`
- `messages.read_thread { thread_id, limit? }`
- `messages.search { query }`

**Defer write:** sending iMessages safely requires entitlements + user consent gating. Skip in v1.

## Layer 3: Skill / documentation updates

Cheap, documentation-only changes that affect agent behavior immediately.

### 3.1 Rewrite the `/cellar` skill instructions

Current skill teaches Perceive → See → Act → Think but in practice agents fall back to See-poll + screenshots. Rewrite to:

- **Mandate cortex for any task ≥3 actions.** Start at top, stop at bottom. Document the cost: ~5KB JSON `read` vs. ~500KB screenshot per check.
- **Tool priority order:** adapter → `ax_action` → `set_value` → `cdp_eval` → keystroke `type` → coordinate click. First match wins. Currently many agents reach for `cdp_eval`/`type` first.
- **One-action-per-batch when UI changes.** Multi-action batches are for fill-3-form-fields, not for navigate-then-click-then-type. The 100ms `delay_between_ms` isn't enough to absorb a UI transition.
- **Screenshots are debugging tools, not verification tools.** Use cortex `feed` after each action for verification; screenshots only for final user proof or AX-hostile debugging.

**File:** `/Users/dimitriospagkratis/.claude/skills/cellar/SKILL.md` (this is what gets loaded when the user types `/cellar`).

### 3.2 Add common-pitfalls section

A new doc page at `cellar/docs/external-agent-pitfalls.md`:

- "Why Cmd+A might select notes in the sidebar instead of body text" — focus scope confusion
- "Why `window.location.href = X` works for navigation but `<a href>.click()` doesn't always"
- "Why `cellar browser ensure` should only be called once per session"
- "Why typing into `cel_act type` can land in the wrong app and how to prevent it (focus_lock)"

### 3.3 Tool-description rewrites

Each MCP tool's `description` field is what agents see during tool selection. Tighten them:

- Lead with the use case, not the mechanic ("Use to navigate the browser tab in place. Backed by `Page.navigate` over CDP.")
- Include 1-line "do NOT use for" guidance ("Do NOT use to switch apps — use `activate_app` or `focus_lock` for that.")
- Cross-reference: if an action has a better alternative for a given scenario, name it.

## Sequencing — what to build first

Ranked by ROI per hour of work:

1. **`focus_lock` / `focus_release`** (Layer 1.1) — ~1 day of work, eliminates ~60% of external-agent fragility immediately. Build first.
2. **Notes adapter** (Layer 2.1) — ~1-2 days of work. Proves the "more adapters" thesis with an immediately useful target. Mail/Calendar/Reminders follow from the same pattern with low marginal cost.
3. **Skill rewrite** (Layer 3.1) — ~half day of work. Free-ish; just doc. Highest reach (every cellar invocation gets it).
4. **`select_between`** (Layer 1.2) — ~1 day. Solves rich-text editing for cases the Notes adapter doesn't cover (e.g., Pages, third-party text apps).
5. **Mail/Calendar/Reminders adapters** (Layer 2.2–2.4) — ~1 day each, sequential. Each unlocks its app's automation.
6. **`text_replace_in_focused`** (Layer 1.3) — ~half day. Layered on `select_between`, so build after it.
7. **Common-pitfalls doc + tool-description rewrites** (Layer 3.2–3.3) — ~half day. Ongoing maintenance, easy.
8. **Messages adapter (read-only)** (Layer 2.5) — ~half day. Lowest priority.

Total: ~10 working days for the full plan, or ~3-4 days for just items 1-3 (the high-ROI core).

## Open questions

- **`focus_lock` and the user's main Chrome.** If an external agent locks focus to "Google Chrome" and the user *also* has Chrome windows open, which one gets focus? Likely answer: the CEL-owned Chrome (port 9333 profile). Document this.
- **Adapter discovery.** External agents currently learn about adapters by reading docs. Should the MCP server expose a `cel_adapters list` action that returns available adapters and their ops? Probably yes — makes the adapter pattern self-discoverable.
- **Permission gating for write actions.** Mail.send, Calendar.create, Reminders.complete — should the adapter require an explicit `confirm: true` flag, or rely on the host agent's own confirmation? Leaning host-managed (CEL is a primitive layer, not a policy layer), but worth checking.
- **Async / long-running operations.** Some adapter ops (e.g., `mail.list_inbox` against a large mailbox) may take seconds. Should they support streaming/progress, or just return when done? v1: just return.
- **Test fixtures.** Adapter tests need ephemeral state — a test note, a test calendar event. Document a teardown pattern so tests don't pollute the user's actual data.

## Verification plan (once implemented)

A single integration test that re-runs the 2026-05-12 failure scenario:

1. External agent (Claude Code via MCP) issues these calls only:
   - `focus_lock { app_name: "Notes" }`
   - `notes.create_note { title: "Market thoughts — May 2026", body: "..." }`
   - `notes.set_body { note_id, body: "<corrected body>" }`
   - `notes.get_body { note_id }` → assert matches expected
   - `focus_release`
2. Total tool calls: 5. Total screenshots: 0. Total focus losses: 0. Total accidentally-deleted notes: 0.
3. Compare to the actual 2026-05-12 session: ~80 tool calls, ~12 screenshots, 1 focus-loss-induced typo into chat, 1 accidentally deleted note.

If the integration test passes, the plan worked.
