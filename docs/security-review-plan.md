# Security Review Plan

This doc answers: what's the roadmap for bringing CEL to a security posture that enterprises and OSS communities trust? It lays out the threat model, phased review work, and disclosure process.

## Why This Exists

Security is a GTM blocker for enterprise ([gtm-icp.md](gtm-icp.md)) and a credibility signal for OSS adopters. Agents that execute on a user's machine with accessibility permissions and clipboard access are a category that deserves scrutiny. This doc says publicly and explicitly what we're doing about it.

The existing entry points:

- [SECURITY.md](../SECURITY.md) at the repo root — disclosure policy, reporting email, known-dangerous features. Already published.
- [security-replan-hardening.md](security-replan-hardening.md) — planner-specific hardening notes.

This doc adds the forward-looking program.

## Threat Model

The concrete surfaces we need to defend. Each has a current posture and a gap to close.

### 1. Third-party adapter code on the user's machine

**Surface**: once adapters are extensible, community or third-party adapters run with the same privileges as CEL itself — AX, screen capture, input injection, clipboard, filesystem. A malicious adapter is indistinguishable from malicious code the user installed.

**Current posture**: first-party adapters only. No third-party adapter loader shipped yet. No sandbox.

**Gap**: third-party adapter sandboxing decision is still open. Proposed: process isolation (each adapter runs in a subprocess, communicates over stdio/IPC, limited capabilities). MVP-acceptable; not cryptographic isolation.

**Reference**: when `docs/adapter-security.md` exists, it will be the canonical threat model for this surface. **TODO**: that doc does not yet exist.

### 2. MCP server — localhost stdio (today) and HTTP (Phase 1 worker)

**Surface**: CEL's MCP tools are invoked by agents. Today that's over stdio on localhost, which is acceptable for a local runtime where the attacker already has local code execution. Phase 1 ([ROADMAP.md](ROADMAP.md)) adds an HTTP worker — a remote attacker gains a new reachable surface.

**Current posture**: localhost stdio, unauthenticated. Sufficient for local use; not sufficient for HTTP.

**Gap**: HTTP mode needs:

- **Bearer token auth** (env-var-configured, rotated by deployer).
- **TLS** — recommended for LAN, required for WAN exposure.
- **Rate limiting** per token.
- **Request size limits** to bound DoS exposure.

All are on the Phase 1 deliverable list in [ROADMAP.md](ROADMAP.md) and [worker-protocol.md](worker-protocol.md).

### 3. LLM data exfiltration

**Surface**: the agent sends screen contents, AX trees, and CDP snapshots to cloud LLMs during planning. A sensitive document visible on screen — a bank password prompt, a client's PII — flows to the LLM provider.

**Current posture**: `redact_on_password_focus` exists (redacts the focused password field when a password input is active). That's narrow.

**Gap**: no generic secret-scrub before LLM calls. Need:

- Pattern-based redaction for common secrets (API keys, tokens matching known formats, credit card numbers, email addresses configurably).
- A user-configurable redaction allowlist/denylist per domain or app.
- Opt-in per-task redaction for known-sensitive contexts.
- Audit log of what left the device to what provider.

Priority: high. This is a standard enterprise ask and missing it blocks ICP C.

### 4. Supply chain — Cargo + npm dependencies

**Surface**: transitive dependencies. A compromised dependency runs with full process privileges.

**Current posture**: **TODO: verify** — `cargo audit` and `pnpm audit` presence in CI. Likely present given the project maturity; needs confirmation.

**Gap** (if audits aren't wired up):

- `cargo audit` in CI, fail-on-vuln at medium+.
- `pnpm audit` in CI, fail-on-vuln at medium+.
- Renovate / Dependabot for dependency updates.
- Lockfile pinning verified on every merge.
- Reproducible builds documented (bonus, not blocker).

### 5. macOS permissions — Accessibility, Screen Recording, Automation

**Surface**: CEL requests Accessibility, Screen Recording, and potentially Automation permissions from macOS. Once granted, they persist. A compromised CEL binary or adapter has broad access.

**Current posture**: permissions are user-granted; we don't silently escalate.

**Gap**:

- Clear in-product documentation of which permission is used for which feature.
- No silent prompt-bundling — each permission requested separately with justification.
- "Disable now" toggle in the GUI for users who want to temporarily revoke.
- Binary signing + notarization for the commercial app (`app/`).

## Review Phases

The work, sequenced. Each phase is a reviewable milestone with deliverables.

### Phase A — Immediate (0–4 weeks)

Focus: close the most obvious gaps, publish the threat model, wire up basic supply-chain hygiene.

**Deliverables**:

1. **Threat model doc** published. This doc, refined from first principles review. Published under `docs/` and linked from [SECURITY.md](../SECURITY.md).
2. **`cargo audit` in CI.** Fail-on-vuln at medium+, allow-list documented for known-false-positives.
3. **`pnpm audit` in CI.** Same rule.
4. **Basic secret-scrub in LLM path.** Pattern-based redaction for common API-key formats, JWTs, credit card numbers. Configurable on/off, default on.
5. **Auth on the worker protocol.** Bearer token + TLS guidance. Merged before the worker image ships publicly. Co-owned with [ROADMAP.md](ROADMAP.md) Phase 1.

**Exit criteria**: threat model reviewed by one external security-minded reader; audits green on main; secret-scrub integration-tested end-to-end; worker auth tested in `e2e/remote/`.

### Phase B — Near-term (1–3 months)

Focus: third-party adapter safety, signing, disclosure.

**Deliverables**:

1. **Third-party adapter sandboxing decision.** Write a decision doc evaluating process isolation vs. WASM isolation vs. "trust the developer, document the risk." Implement the chosen path as an MVP. Proposed default: process isolation.
2. **Adapter signing and verification.** Community-contributed adapters are signed by maintainers before inclusion. Signature verification on load. Key management documented.
3. **Documented disclosure policy.** [SECURITY.md](../SECURITY.md) already covers the basics. Verify the `dimpagk92@gmail.com` mailbox is monitored, PGP key published, and the coordinated-disclosure timeline matches this doc. **TODO: confirm mailbox operation.**
4. **Permission UX review.** Walk through every macOS permission prompt in the install + run flow. Document justification for each. Pare down any that aren't necessary.
5. **Dependency update automation.** Renovate or Dependabot, triaged weekly.

**Exit criteria**: at least one external adapter has gone through the signing flow; dependency updates land weekly without backlog; `dimpagk92@gmail.com` has answered a real (or test) report within the 72-hour SLA.

### Phase C — Medium-term (3–6 months)

Focus: external validation, fuzzing, telemetry review.

**Deliverables**:

1. **External security audit.** Engage a third-party firm. Scope: CEL core crates, MCP server, worker protocol, adapter sandbox. **TODO: user decision** — NGI grant may cover; confirm scope and budget.
2. **Fuzzing of the MCP tool boundary.** `cargo-fuzz` targets for `cel_see` / `cel_act` / `cel_perceive` / `cel_think` input parsers. Run in CI on every merge.
3. **Telemetry opt-in audit.** If any telemetry is collected, publish exactly what, where it goes, and the opt-in path. If none is collected, state so publicly.
4. **Red-team exercise.** Internal or external team tries to exfiltrate data or escalate privileges through a crafted adapter / crafted goal. Document findings.

**Exit criteria**: external audit report published (with any embargo windows respected); fuzzing has run 1 million+ iterations without new crashes; red-team findings resolved or documented as known-residual-risk.

## Public-Facing Deliverables

- **`SECURITY.md`** at repo root — **exists** today. Review quarterly to keep current. Contains reporting email, SLA, scope, known-dangerous features.
- **`docs/security-review-plan.md`** — this file. Updated as phases complete.
- **`docs/adapter-security.md`** — the threat model specific to adapters. **TODO**: not yet written. Create alongside Phase B adapter-sandbox work.
- **CVE advisories** — filed via GitHub Security Advisories for confirmed issues. Coordinated-disclosure window: 90 days default, shorter if in-the-wild exploitation confirmed.

## Disclosure

- **Report to**: `dimpagk92@gmail.com` (see [SECURITY.md](../SECURITY.md)).
- **Response SLA**: acknowledge within 72 hours; confirm or dispute within 7 days.
- **Fix SLA**: high-severity within 30 days; medium/low within 90 days.
- **Coordinated disclosure**: 90 days default. Extendable by mutual agreement. Shorter if active exploitation.
- **Credit**: reporters credited in release notes with their permission. No bug bounty program at this time. **TODO: user decision** — establish bounty program before or after external audit?
- **CVE requests**: filed via GitHub Security Advisories, published once the fix is live.

## Open Questions — TODO: user decision

| Question                                                             | Phase    |
|----------------------------------------------------------------------|----------|
| Does the NGI grant cover the external audit?                         | Phase C  |
| Timeline per phase — concrete dates based on headcount.              | All      |
| Bug bounty program: yes/no and budget.                               | Phase C  |
| Adapter sandbox technology: process isolation / WASM / trust model.  | Phase B  |
| Confirm `dimpagk92@gmail.com` mailbox is monitored.                 | Phase A  |
| Verify `cargo audit` / `pnpm audit` are already in CI.               | Phase A  |
| Telemetry: is any collected today, and is the opt-in path published? | Phase C  |

## Related Reading

- [SECURITY.md](../SECURITY.md) — disclosure policy and known-dangerous features.
- [security-replan-hardening.md](security-replan-hardening.md) — planner hardening history.
- [adapters-cel-agents.md](adapters-cel-agents.md) — the three-layer architecture the threat model maps onto.
- [ROADMAP.md](ROADMAP.md) — Phase 1 worker protocol deliverables the security work co-owns.
- [worker-protocol.md](worker-protocol.md) — the HTTP surface needing auth + TLS.
- [building-adapters.md](building-adapters.md) — the adapter contract sandboxing will extend.
- [stability.md](stability.md) — stability commitment the security exception can override.
- [README.md](../README.md)
