# Go-To-Market & Ideal Customer Profile

This doc answers: who is CEL for, in what order, and how do we reach them? It's a strategic doc — items marked **TODO: user decision** are the ones where the user (not a maintainer draft) needs to commit.

## The Thesis, Restated

Planning is commoditizing. Perception and execution on real devices are not. CEL's wedge is: **"the device-mastery layer any agent can plug into."** GTM must match that wedge — we sell infrastructure-shaped value to infrastructure-shaped buyers, not finished automations to end users. See [what-cel-is.md](what-cel-is.md).

## Three Candidate ICPs

We have three viable ICPs. Each has a different ACV, sales cycle, and CAC profile. We should focus on one primary and sequence the rest. **TODO: user decision** — confirm or replace the recommended order below.

### ICP A — Agent-platform builders

**Who**: teams building agent platforms. Mastra, LangChain, LlamaIndex, in-house agent platforms at mid-market and enterprise companies. Includes both commercial vendors and internal engineering teams.

**Pain**: they have agents. They don't have device understanding. Building perception is a multi-year distraction from their actual product.

**Why CEL**: drop CEL behind their planner over MCP. Keep their orchestration. Stop reinventing AX + CDP + vision fusion. Immediate "desktop automation" as a product feature.

**Economics**: medium ACV, medium cycle (3–6 months), technical buyer. Integrations require engineering from both sides; once integrated, CEL becomes load-bearing.

**Risk**: only a few of these exist; losing a big one hurts.

### ICP B — Individual developers and hobbyists

**Who**: developers building personal or prototype agents. Reachable via MCP-compatible tools (Claude Code, Cursor, generic MCP clients). Indie hackers, researchers, weekend-project builders.

**Pain**: they want a quick way to make their agent "do stuff on the computer" without building a perception stack.

**Why CEL**: one `cellar init`, an MCP config snippet, and their agent can see and act. Zero per-app code.

**Economics**: low ACV (mostly free-tier), very large funnel, drives OSS momentum + GitHub stars + cookbook adoption. Converts into ICP A/C over time as builders graduate to platforms or enterprises.

**Risk**: hobbyist engagement doesn't pay bills directly. Has to be treated as top-of-funnel, not revenue.

### ICP C — Enterprise automation teams

**Who**: teams currently using UiPath, Automation Anywhere, or Power Automate. Big FS, healthcare, insurance — legacy desktop apps + regulatory reasons for on-prem.

**Pain**: existing RPA is brittle, per-selector, expensive to maintain. AI-native approaches exist but are usually web-only.

**Why CEL**: the adapter layer is the wedge. An enterprise's pain is usually concentrated in one or two apps (Excel, Bloomberg, SAP, a bespoke in-house system). A production-grade adapter for any of those is a wedge into a whole account.

**Economics**: high ACV ($100K+), long cycle (6–12 months), procurement/security/legal overhead. High LTV. Adapter development becomes a revenue-sharing surface.

**Risk**: long cycles before revenue. Enterprise features (SSO, audit, on-prem) need to exist before conversations get real.

## Recommended Sequencing

**Proposal** (mark as **TODO: user decision** until confirmed):

1. **ICP B first** — OSS momentum, low CAC, proves the agent-agnostic thesis with public evals + cookbooks. 3–6 months.
2. **ICP A next** — Mastra partnership first (shared TS language, aligned philosophy), then LangChain/LlamaIndex integrations. Co-published content, joint demos. 6–12 months.
3. **ICP C last** — enterprise team needs to exist. Don't chase before ICP A/B prove the platform. 12+ months.

**Why this order**:

- OSS traction from B makes A conversations credible ("we're the layer Claude Code / Cursor users already run").
- Platform partnerships from A make C conversations credible ("our runtime is what Mastra recommends").
- C without A/B is a services business disguised as a product business.

**Counter-arguments the user should weigh**:

- If a named enterprise is ready to pay seven figures in Q2, skip to C. Revenue beats momentum.
- If no platform partnership materializes in 6 months, B alone may not convert fast enough to fund the team.

## Wedge Moves

Concrete actions per ICP.

### For ICP B (developers)

- **Claude Code cookbook.** Claude Code is the highest-reach MCP client as of April 2026. A "Claude Code + CEL in 5 minutes" path is the single most important adoption artifact. **TODO: user decision** — who owns this and when does it ship?
- **Cursor cookbook.** Same pattern, second-highest-reach MCP client.
- **MCP quickstart.** Generic MCP client + CEL, not tied to a vendor, for the long tail.
- **"Build a \_\_\_ agent in 30 minutes" posts.** Desktop form-filler, screenshot-to-JIRA, inbox triage — concrete goals that showcase perception+execution.

### For ICP A (platforms)

- **Mastra partnership.** Shared TS org-language, both in the agent-runtime category. Target: joint blog post, cross-linked docs, a reference app. **TODO: user decision** — who owns the Mastra conversation?
- **LangGraph integration guide.** Already a supported client; needs a canonical reference doc + recipe.
- **Framework-comparison content.** "CEL + LangGraph vs CEL + Mastra" — honest tradeoffs, drives both communities to read our docs.

### For ICP C (enterprise)

- **Adapter development as a service.** Offer paid adapter development for a named enterprise app. Delivers an asset the enterprise needs + a reusable adapter for the community.
- **Security review package.** SOC2 path, SSO, audit logging, on-prem deployment guide. Precondition for most enterprise conversations. See [security-review-plan.md](security-review-plan.md).
- **Reference customer.** One named enterprise win > fifty hobbyist tweets. **TODO: user decision** — who's the target first enterprise?

### Cross-cutting

- **Public eval leaderboard.** Loudest marketing surface under the agent-agnostic thesis. See [eval-leaderboard.md](eval-leaderboard.md). "We measure any agent × CEL, here's the score."
- **NGI grant momentum.** EU community adoption via the grant is an underused asset. Apache 2.0 made EU adoption viable; now we need the community moves to follow.

## Channels

| Channel                               | ICP   | Notes                                                         |
|---------------------------------------|-------|---------------------------------------------------------------|
| GitHub                                | B, A  | Stars, releases, cookbook repos, issues-as-support.           |
| HackerNews                            | B, A  | Launch moments, benchmark results, architectural blog posts.  |
| Twitter/X                             | B, A  | Agent dev community still concentrated here.                  |
| Agent framework communities (Discord) | A     | Mastra, LangChain, Claude Code Discords. Long-horizon relationship building. |
| Targeted SaaS blogs                   | A, C  | Guest posts on agent-platform vendor blogs.                   |
| NGI / EU OSS channels                 | B, C  | Grant-adjacent OSS directories, conferences, FOSDEM.          |
| Direct enterprise outbound            | C     | Later. Not before ICP A proves the platform.                  |
| Paid ads                              | —     | Not recommended pre-PMF. CAC won't be legible.                |

## Success Metrics

**Early (0–6 months)** — all measuring adoption, not revenue:

- GitHub stars and weekly net-adds.
- MCP cookbook adoption: Claude Code + CEL installs, Cursor + CEL installs.
- External-agent eval submissions to the leaderboard ([eval-leaderboard.md](eval-leaderboard.md)).
- First 10 named production users — list maintained publicly.
- Third-party adapter PRs opened, adapters shipped outside the monorepo.

**Mid (6–12 months)** — platform traction:

- Number of ICP A partnerships signed (joint-publishable relationships).
- Number of ICP C paid conversations in flight.
- Number of external contributors to core crates.

**Late (12+ months)** — revenue:

- Paid conversions, MRR, enterprise contracts.
- Deferred until the sequencing above plays out.

## Anti-Goals

Things we are explicitly **not** doing, to keep GTM focus:

- **Full end-user product.** CEL is infrastructure. The Tauri app (`app/`) is a GUI on top, not an end-user automation product like Zapier.
- **Competing with UiPath head-on.** UiPath is a decade-old sales-led motion. We compete sideways through adapters + agent-platform partnerships, not a feature-for-feature bake-off.
- **Training our own frontier model.** Explicit ROADMAP anti-goal. Frontier LLMs + Gemma 4 locally cover the model space; we compete on perception + execution, not on the model.
- **Windows-first.** Explicit ROADMAP anti-goal. macOS-first, Linux worker Phase 1, Windows only if customer demand forces it.

## Open Questions — TODO: user decision

| Question                                                                | Needed by                                |
|-------------------------------------------------------------------------|------------------------------------------|
| Confirm the B → A → C sequencing, or replace with a different order.    | Before any GTM budget decisions.         |
| Who owns the Claude Code cookbook and when does it ship?                | This quarter. It's the highest-leverage move. |
| Who owns the Mastra partnership conversation?                           | Before Q3.                               |
| First enterprise target for ICP C.                                      | Before ICP C push begins.                |
| Do we run direct enterprise outbound, or wait for inbound from content? | Before any BDR hire.                     |
| Is there a sales hire on the timeline, or does the founder run it?      | Before any ICP C push.                   |
| Budget for public eval compute (runs cost money).                       | Before leaderboard launches.             |

## Related Reading

- [what-cel-is.md](what-cel-is.md) — top-of-funnel positioning.
- [commercial-model.md](commercial-model.md) — how we monetize the ICPs above.
- [eval-leaderboard.md](eval-leaderboard.md) — the single biggest GTM artifact.
- [adapters-cel-agents.md](adapters-cel-agents.md) — the architecture the GTM thesis derives from.
- [ROADMAP.md](ROADMAP.md) — Phase 1–3 timing the GTM has to align with.
- [README.md](../README.md)
