# Cortex / MCP integration tests

Spawn the MCP server as a subprocess and exercise it via JSON-RPC over
stdio. These tests are macOS-specific and expect:

1. The MCP server built: `pnpm --filter @dpagk/cellar-mcp build`
2. The native module built: `make build-napi`
3. Accessibility permissions granted to the host process running the test

## Running

Each file is a standalone Node script — run with `node`:

```bash
node tests/cortex/mcp-protocol.mjs        # cel_perceive lifecycle
node tests/cortex/mcp-cognition.mjs       # cel_think modes
node tests/cortex/mcp-act-focus.mjs       # cel_act target_app
node tests/cortex/mcp-act-focus-lock.mjs  # cel_act focus_lock / focus_release
node tests/cortex/mcp-adapter-notes.mjs   # cel_act adapter_action → notes adapter
node tests/cortex/mcp-adapter-mail.mjs    # cel_act adapter_action → mail adapter
node tests/cortex/mcp-adapter-calendar.mjs # cel_act adapter_action → calendar adapter
node tests/cortex/mcp-adapter-reminders.mjs # cel_act adapter_action → reminders adapter
node tests/cortex/mcp-adapter-messages.mjs # cel_act adapter_action → messages (read-only)
node tests/cortex/mcp-list-view-labels.mjs # AX row-label cascade
node tests/cortex/mcp-display-and-windows.mjs # screenshot + window enum
```

The Mail / Calendar / Reminders / Messages adapter tests require the
corresponding macOS app to be installed and the host process to hold the
relevant Automation permission (or Full Disk Access for Messages, which
needs `~/Library/Messages/chat.db`). They self-clean any created artifacts.
The Calendar test reads `CEL_TEST_CALENDAR` for the target calendar name
(default `"Home"`); the Reminders test reads `CEL_TEST_REMINDERS_LIST`
(default `"Reminders"`).

All files exit non-zero on failure — wire them into a `pnpm script` or
shell loop for CI once the GitHub Actions billing situation is resolved.

## Multi-display verification

`mcp-display-and-windows.mjs` covers the windows + screenshot fixes on
whichever displays the test machine has. Most CI machines are
single-display, which means the auto-display-selection branch in
`cel-napi/src/context.rs::resolve_active_display` is never exercised
there.

To verify the multi-display path manually:

1. Plug in a second monitor (or use Sidecar on iPad).
2. Move a window — for example, open Finder on the secondary display
   and a Terminal on the primary.
3. Click the Finder window so it's frontmost.
4. Run `node tests/cortex/mcp-display-and-windows.mjs`.
5. The default screenshot's byte count should approximately match the
   Finder display's resolution (e.g. ~2.8 MB for a 1920×1080 panel,
   ~14 MB for a Retina built-in). If it instead matches the *primary*
   display while you were working on the secondary, the auto-selection
   regressed.
6. Click the Terminal so the primary display is frontmost; rerun.
   The default screenshot's byte count should now match the primary.
7. Optionally pass `display_id` from `cel_see monitors` to force a
   specific monitor and confirm explicit selection still works.

`cel_see windows` should return `is_on_screen: true` for any window on
either display — the CFBoolean fix should make this universal.
