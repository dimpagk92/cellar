# CEL Hybrid Runtime Demo

> "We handle the workflows that break when all you have is screenshots."

## Quick Start

```bash
# Full demo: CEL vs Computer Use on 5 hybrid scenarios
./scripts/demo.sh

# CEL only (faster, no comparison)
./scripts/demo.sh --cel-only

# Single scenario
./scripts/demo.sh --task hybrid-stale-state
```

## What You're Showing

CEL uses a **hybrid runtime** — accessibility tree + CDP + vision fusion — instead of screenshots alone. This means it can:

1. **Follow actions across app boundaries** (browser → desktop)
2. **Detect and recover from stale state** (dynamic content changed between read and act)
3. **Disambiguate identical-looking elements** (8 "Remove" buttons, only one is correct)
4. **Detect unintended side effects** (modal/popup the plan didn't expect)
5. **Stop instead of looping** when an action is genuinely impossible

Screenshot-only agents fail at all five.

## The 5 Scenarios

### 1. Browser → Desktop Handoff (`hybrid-browser-desktop-handoff`)
- **Setup:** Support ticket dashboard. Agent acknowledges a ticket, then clicks "Reply via Email" which opens native Mail.
- **What to watch:** Cortex detects the context shift from Browser to Mail.app. Live-view shows the side-effect being recorded.
- **Screenshot agents:** Lose track entirely when the browser loses focus.

### 2. Stale State (`hybrid-stale-state`)
- **Setup:** Live deployment queue that reshuffles every 2 seconds.
- **What to watch:** The freshness indicator goes yellow/red. The router picks `refresh` route before acting. Live-view shows stale recoveries.
- **Screenshot agents:** Click where the button *was*, not where it *is*.

### 3. Ambiguous Targets (`hybrid-ambiguous-targets`)
- **Setup:** User table with 8 similar names (Jamie Chen, James Rodriguez, Jamie Rodriguez...). All "Remove" buttons look identical.
- **What to watch:** Router picks `semantic` route. The a11y tree has `aria-label` with email/role, resolving the ambiguity.
- **Screenshot agents:** Can't distinguish visually identical buttons. ~12.5% chance of picking the right one.

### 4. Side-Effect Detection (`hybrid-side-effect-detection`)
- **Setup:** Invoice manager. Sending an invoice to a client with overdue balance triggers an unexpected collections escalation modal.
- **What to watch:** Cortex detects the modal as a side effect. Live-view shows `sideEffectWarnings` incrementing.
- **Screenshot agents:** Either get stuck on the modal or blindly click through it.

### 5. Terminal Failure (`hybrid-terminal-failure`)
- **Setup:** Admin panel with auth-blocked danger zone (SSO + hardware key required). Agent should recognize it can't proceed and fall back to a task it *can* do.
- **What to watch:** Escalation ladder walks through structured → semantic → vision → terminal_failure. Agent stops and does the fallback task.
- **Screenshot agents:** Loop on the re-auth button indefinitely, or timeout.

## During the Demo

### Live View (http://127.0.0.1:6080)

The live-view shows three things in real-time:

1. **Screen** — what the agent sees
2. **Runtime Decisions** (top-right panel):
   - **Route** — which strategy the router chose (structured/semantic/vision/refresh/terminal)
   - **Freshness** — current context freshness (green=fresh, yellow=soft-stale, red=hard-stale)
   - **Confidence** — how confident the router is in the current action
   - **Escalation ladder** — highlights where we are in the structured → terminal path
   - **Event log** — scrolling list of every routing decision with reasons
3. **Context Feed** (bottom-right) — element count and active app

### What to Point At

- When freshness goes **yellow** → "The runtime detected the page changed since we last read it"
- When route shows **semantic** → "The a11y tree resolved which element to target"
- When **side-effect** appears in red → "The runtime caught an unplanned consequence"
- When escalation reaches **terminal** → "Instead of retrying forever, it recognized this is impossible"

## Metrics That Matter

The metrics that matter when comparing runtimes:

| Metric | Why It Matters |
|--------|---------------|
| `successRate` | Head-to-head: CEL completes tasks that screenshot agents can't |
| `semanticRoutes` | How often the a11y tree provided information vision couldn't |
| `staleRecoveries` | How often freshness detection prevented acting on stale state |
| `sideEffectWarnings` | Unintended consequences that were caught |
| `terminalFailures` | Clean stops vs infinite loops |
| `totalTaskMs` | Time to terminal failure (CEL stops fast, others loop) |
