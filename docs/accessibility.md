# macOS Accessibility — granting CEL the perception it needs

CEL's perception layer fuses two sources of truth: Chrome DevTools Protocol
(CDP) for browser-resident DOM, and the macOS Accessibility (AX) API for
every native app outside the browser. CDP is automatic — the bound headless
Chrome speaks it without user consent. AX, by contrast, requires explicit
per-binary permission in **System Settings → Privacy & Security →
Accessibility**.

This doc is the canonical place to look when you see:

```
WARN cel_context::merge: Accessibility tree unavailable:
     Failed to query accessibility tree: Failed to build tree from focused window
```

## When AX is required

| Scenario shape                       | Needs AX? | Why                              |
|--------------------------------------|-----------|----------------------------------|
| Browser-only goal (Yahoo Finance…)   | No        | CDP carries the DOM.             |
| Native-app goal (Numbers, Mail, …)   | Yes       | No DOM; AX is the only structured truth. |
| Multi-app handoff (Chrome → Numbers) | Yes       | Same — AX needed once focus leaves the browser. |
| Focus-trail tracking, frontmost app  | Yes       | macOS only exposes focus state via AX. |

The cortex degrades gracefully if AX is denied — browser-only runs still
succeed. But every per-tick perception merge fails with the `WARN` above,
which adds log noise and can mask other issues.

## How CEL prompts for the grant

Every Rust binary that may need AX calls
[`cel_accessibility::ensure_trust_or_log(interactive)`](../cel/cel-accessibility/src/lib.rs)
once during boot. It never aborts the process — it just observes the
current trust state and (optionally) triggers the macOS notification.

| Binary                                  | Mode                |
|-----------------------------------------|---------------------|
| `cellar-worker` (daemon)                | `interactive=false` — log only, no notification. |
| `cel-eval scenarios --live` (CLI)       | `interactive=true` (canonical runtime only — LangGraph runtime is skipped, Node side handles it via N-API). |
| Tauri desktop app (`cellar-cellar`)    | `interactive=true` — prompt on every cold launch where the grant is missing. |
| MCP server (host: Claude Code, Cursor…) | Host process handles it via [`ax_request_permission`](../cel/cel-napi/src/lib.rs) over N-API. No change here. |

`interactive=true` calls `AXIsProcessTrustedWithOptions` with
`kAXTrustedCheckOptionPrompt: true`. The side effect is the macOS system
notification — click it to jump straight into Settings with the binary
pre-selected. `interactive=false` only logs grant instructions; right for
headless services where a notification would be unanswered noise.

## Granting permission, step by step

1. Run the binary once. If AX is denied, you'll see the WARN above and
   (for interactive binaries) a macOS notification.
2. Either click the notification, or open **System Settings → Privacy &
   Security → Accessibility** manually.
3. Click **+** and add the binary. For dev builds:
   `<repo>/target/debug/cel-eval`,
   `<repo>/target/debug/cellar-worker`,
   `<repo>/target/release/cellar-cellar` (or the `.app` bundle in
   release builds).
4. Toggle the row on.
5. **Restart the process.** macOS does not pick up grants mid-process.

## Quirks worth knowing

- **The grant is per-binary-identity.** Signed releases use the
  code-signing fingerprint, so a `brew upgrade` or `.dmg` update keeps the
  grant. Unsigned dev builds use the path + binary checksum — every
  `cargo build` produces a new checksum, so dev rebuilds may require a
  re-grant. Annoying, but it's a macOS-side decision; there's nothing CEL
  can do about it.

- **The prompt notification fires once.** Once the binary appears in the
  Accessibility list (toggled on OR off), macOS will not re-fire the
  notification on subsequent denied calls — you have to open Settings
  yourself. The `ensure_trust_or_log` log line still tells you what to do.

- **`ax_request_process_trust()` is non-blocking.** It posts the
  notification and returns immediately with the current (still false)
  trust state. Don't loop polling — restart the process after the user
  toggles the permission on.

- **CI environments don't need AX.** Headless runners have no native
  apps to perceive; `cellar-worker` in CI runs CDP-only and the WARN
  fires once at boot. Filter the line in your log shipper if it's noisy.

## Debugging

- Quick "am I trusted?" check from any Rust call site:
  ```rust
  if !cel_accessibility::ax_is_process_trusted() {
      tracing::warn!("…");
  }
  ```
- From N-API (Node host side):
  ```js
  const { ax_permission_granted, ax_request_permission } = require("cel-napi");
  if (!ax_permission_granted()) ax_request_permission();
  ```
- If the binary appears in System Settings but AX still fails after a
  process restart, double-check you're running the same binary path the
  Settings entry points at. Stray `~/.cargo/bin` shims and Homebrew
  copies are common sources of confusion.

## Related

- [`docs/eval-isolation.md`](eval-isolation.md) — the three-layer defence
  against accidentally typing into your foreground app during eval runs.
  Accessibility is orthogonal: it's about perceiving the world, not
  acting on it.
