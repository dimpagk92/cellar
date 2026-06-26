# Commercial Model

This doc answers: how does Cellar make money while keeping the core standards open-source? It sketches the revenue hypotheses, the open-core boundary, and the tradeoffs of protecting the live runtime and operations layer.

## Premise

The OSS core is the **context / memory / brief / contract layer** ([oss-boundary.md](oss-boundary.md)). Developers can use CEL-shaped context snapshots, memory providers, governed briefs, transport schemas, and receipt contracts without a commercial agreement.

Commercial value accrues in the **live operating layer**: the continuous cortex runtime, governance and monitoring workflows, the Tauri desktop app, hosted workers, the control plane, the cloud, and billing. This is still an open-core shape, but the open part is the shared data plane and the paid part is the operational plane.

The license flip from BSL 1.1 to Apache 2.0 on 2026-04-19 was deliberate. BSL blocked a hosted-competitor scenario (a third party offering "CellarCloud as a service") by license. Apache 2.0 does not. We accepted that tradeoff because:

1. OSI-approved licenses are table stakes for NGI-grant, enterprise-procurement, and Debian/Fedora packaging pipelines.
2. The hosted-competitor moat was never strong — AWS would have done it anyway.
3. The durable moat is **live context operations, governance, compliance workflows, and commercial product surface**, not the license.

Protection against a hosted competitor now lives in proprietary components (`cel-cortex` by default, `app/`, future `control-plane/`, `cloud/`, `billing/`) plus the quality of the managed governance experience.

## Revenue Hypotheses

These are **hypotheses**, not a committed plan. Each is a candidate direction; the user still owes a decision on prioritization.

### 1. Agent governance and compliance operations

Use the live cortex runtime plus receipts, memory, and brief records to give teams an operational view of what agents discovered, what they implemented, what they saw, what they changed, and whether policy was followed.

- **Strength**: maps directly to enterprise risk: auditability, approval, retention, incident review, governance, compliance exports.
- **Weakness**: requires a polished product surface, clear schemas, and careful policy UX before it can command enterprise pricing.
- **TODO: user decision** — is this the primary commercial product thesis? Current recommendation: yes.

### 2. Hosted remote workers — "Cellar Cloud"

Phase 3 in [ROADMAP.md](ROADMAP.md). Run CEL workers in our infra, expose the same MCP/worker protocol, bill per minute or per subscription tier.

- **Strength**: clean metering, direct mapping from value delivered (automation minutes) to charge.
- **Weakness**: infrastructure capex, macOS-in-cloud is expensive (EC2 Mac / MacStadium), compete with the user's own laptop for many workloads.
- **TODO: user decision** — is this the primary commercial product, or the execution backend for the governance product?

### 3. Managed agent platform

Customer brings their own agent (LangGraph, Mastra, in-house); we run it in our infra against their accounts, wrapped in CEL. Handles credentials, logging, observability, and compliance.

- **Strength**: leverages the agent-agnostic thesis directly. Customer keeps their planner; we sell the operational layer.
- **Weakness**: every customer's agent is a new integration surface. Needs a plugin model for customer-supplied code.
- **TODO: user decision** — is this a packaging of Cellar Cloud + governance, or a distinct product?

### 4. Enterprise support contracts

SLA + security review + dedicated adapter development + named-engineer support.

- **Strength**: high ACV, predictable recurring revenue, funds adapter expansion directly.
- **Weakness**: slow to close (6–12 month cycles), concentrates revenue on a few accounts, team costs scale with contract count.
- **TODO: user decision** — what's the minimum viable support tier and price point?

### 5. Adapter marketplace revenue share

Host a registry of community adapters. Paid tier for verified / signed / supported adapters. Take a percentage of any paid adapters.

- **Strength**: network effect — more adapters attract more agent platforms, which attract more adapters.
- **Weakness**: marketplaces are hard. They require curation, verification, payment rails, and a critical mass of both producers and consumers before they work.
- **TODO: user decision** — do we *run* a marketplace? Take a cut? Or stay pure-OSS on the registry and monetize elsewhere?

### 6. Commercial-license carve-outs

Components that are proprietary from day one:

- `app/` — the Tauri desktop app. The polished GUI most users install.
- `cel-cortex` — the live context/runtime engine by default, unless a later strategy decision explicitly promotes part of it to OSS.
- `cellar-runtime`, `cellar-act-gateway`, rule stores, policy wiring — governance and monitoring product logic.
- `control-plane/` (future) — auth, billing, fleet orchestration for Cellar Cloud.
- `cloud/` (future) — infrastructure-specific code behind the managed service.
- `billing/` (future) — paid product plumbing.

These are the products; the OSS crates are the contracts and data plane they run on.

## What Stays Open

These paths are committed to Apache 2.0 / MIT in the mirror ([oss-boundary.md](oss-boundary.md)):

- `cel-context` and `cel-contracts` — the shared context and action/receipt contracts.
- `cel-memory`, `cel-memory-sqlite`, `cel-brief` — memory and briefing crates.
- Receipt, event, MCP, CLI, and SDK schemas.
- `mcp-server/` and `cli/` as transport surfaces and local entry points.
- Adapter SDK / adapter contracts.
- Benchmarks, eval harness, docs.

No plan to close these. The revised bet is that an open context/memory/brief standard creates adoption and trust, while the live cortex runtime and compliance operations layer are what customers pay for.

## Why This Works — Industry Precedents

| Company    | Open runtime                  | Commercial surface                 |
|------------|-------------------------------|------------------------------------|
| Docker     | Docker engine, CLI, containerd| Docker Desktop, Docker Hub Pro     |
| HashiCorp  | Terraform, Vault, Consul      | HCP (managed), enterprise editions |
| Elastic    | Elasticsearch (Apache 2.0)    | Elastic Cloud, enterprise features |
| GitLab     | CE (open)                     | EE (proprietary features + SaaS)   |

Pattern: the open standard / runtime contract is the *adoption* vector. The commercial product is the *monetization* vector. Users pick up the open thing because it is inspectable and easy to integrate; upgrade to the commercial thing when they need continuous operation, policy, compliance, reliability, or fleet scale.

CEL maps onto this pattern cleanly. The OSS data plane is the adoption vector (agent platforms need a common context, memory, brief, and receipt language). The cortex runtime, governance dashboard, Cellar Cloud, and desktop app are the commercial surface for users who need the system operated and governed.

## What We Sacrificed With the License Flip

Being honest: Apache 2.0 is a real tradeoff, not a costless win.

- **AWS / GCP / Azure can wrap the OSS data plane as a service tomorrow.** They still would not get the commercial cortex runtime, governance surface, or policy workflows unless those are separately opened.
- **A well-funded competitor can fork the contracts.** The protection is healthy maintenance, responsive community review, and staying ahead on the live operating layer.
- **We can't charge for the open contracts themselves.** Revenue must come from surfaces users cannot replicate cheaply: continuous context operations, hosted ops, governance, compliance, support, and commercial-only packaging.

These are features of open-core, not bugs. They force us to be excellent at the commercial surface, not defensive about the license.

## Pricing — TODO

Placeholder. **TODO: user decision** on tiering and price points.

Candidate shape (illustrative only, no commitments):

- **Dev tier** — free forever. Single user, local execution. Includes the Tauri app with a free-use license for individuals.
- **Team tier** — paid. Multi-user, shared workflows, basic observability, community support.
- **Enterprise tier** — paid. SLA, SSO, audit logs, security review, dedicated support engineer, on-prem option.
- **Cloud** — usage-based. Pay per worker-minute for hosted execution.

Target price points, volume expectations, and free-to-paid conversion hypotheses all need user input. This doc flags the shape; it does not commit to numbers.

## Open Questions

| Question                                                        | Needed by                                  |
|-----------------------------------------------------------------|--------------------------------------------|
| Is governance/compliance operations the primary product?        | Before Phase 3 hosted cloud planning.      |
| Do we run an adapter marketplace, or leave it to the community? | Before the adapter contribution flow ships.|
| Enterprise support: minimum viable tier?                        | Before first enterprise conversation.      |
| Cloud pricing model: per-minute vs. subscription vs. hybrid?    | Before Phase 3 pricing page.               |
| How aggressively do we sell against the open data plane?        | Before Phase 3 launch messaging.           |

## Related Reading

- [oss-boundary.md](oss-boundary.md) — the exact OSS/commercial path-level boundary.
- [gtm-icp.md](gtm-icp.md) — who we sell to first and why.
- [stability.md](stability.md) — the API stability commitment enterprises need.
- [ROADMAP.md](ROADMAP.md) — Phase 3 is where Cellar Cloud lands.
- [adapters-cel-agents.md](adapters-cel-agents.md) — the north-star architecture behind the open-core boundary.
- [README.md](../README.md)
