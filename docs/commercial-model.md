# Commercial Model

This doc answers: how does Cellar make money while keeping the core open-source? It sketches the hypotheses for revenue, the open-core boundary, and the tradeoffs of the recent Apache 2.0 license flip.

## Premise

The core is **Apache 2.0** ([oss-boundary.md](oss-boundary.md)). That's the runtime, the adapters, the MCP server, the CLI, and the reference planner. Anyone can use, modify, host, or fork it without a commercial agreement.

Commercial value accrues **above the core**: managed hosting, enterprise support, commercial-only product surfaces (the Tauri desktop app, the control plane, the cloud, the billing layer). This is the Docker / HashiCorp / Elastic shape — a liquid OSS runtime plus a paid commercial surface.

The license flip from BSL 1.1 to Apache 2.0 on 2026-04-19 was deliberate. BSL blocked a hosted-competitor scenario (a third party offering "CellarCloud as a service") by license. Apache 2.0 does not. We accepted that tradeoff because:

1. OSI-approved licenses are table stakes for NGI-grant, enterprise-procurement, and Debian/Fedora packaging pipelines.
2. The hosted-competitor moat was never strong — AWS would have done it anyway.
3. The durable moat is **operational excellence and commercial product surface**, not the license.

Protection against a hosted competitor now lives in proprietary components (`app/`, future `control-plane/`, `cloud/`, `billing/`), not in the runtime license.

## Revenue Hypotheses

These are **hypotheses**, not a committed plan. Each is a candidate direction; the user still owes a decision on prioritization.

### 1. Hosted remote workers — "Cellar Cloud"

Phase 3 in [ROADMAP.md](ROADMAP.md). Run CEL workers in our infra, expose the same MCP/worker protocol, bill per minute or per subscription tier.

- **Strength**: clean metering, direct mapping from value delivered (automation minutes) to charge.
- **Weakness**: infrastructure capex, macOS-in-cloud is expensive (EC2 Mac / MacStadium), compete with the user's own laptop for many workloads.
- **TODO: user decision** — is this the primary commercial product, or a convenience for enterprise?

### 2. Managed agent platform

Customer brings their own agent (LangGraph, Mastra, in-house); we run it in our infra against their accounts, wrapped in CEL. Handles credentials, logging, observability, and compliance.

- **Strength**: leverages the agent-agnostic thesis directly. Customer keeps their planner; we sell the operational layer.
- **Weakness**: every customer's agent is a new integration surface. Needs a plugin model for customer-supplied code.
- **TODO: user decision** — is this a packaging of Cellar Cloud, or a distinct product?

### 3. Enterprise support contracts

SLA + security review + dedicated adapter development + named-engineer support.

- **Strength**: high ACV, predictable recurring revenue, funds adapter expansion directly.
- **Weakness**: slow to close (6–12 month cycles), concentrates revenue on a few accounts, team costs scale with contract count.
- **TODO: user decision** — what's the minimum viable support tier and price point?

### 4. Adapter marketplace revenue share

Host a registry of community adapters. Paid tier for verified / signed / supported adapters. Take a percentage of any paid adapters.

- **Strength**: network effect — more adapters attract more agent platforms, which attract more adapters.
- **Weakness**: marketplaces are hard. They require curation, verification, payment rails, and a critical mass of both producers and consumers before they work.
- **TODO: user decision** — do we *run* a marketplace? Take a cut? Or stay pure-OSS on the registry and monetize elsewhere?

### 5. Commercial-license carve-outs

Components that are proprietary from day one:

- `app/` — the Tauri desktop app. The polished GUI most users install.
- `control-plane/` (future) — auth, billing, fleet orchestration for Cellar Cloud.
- `cloud/` (future) — infrastructure-specific code behind the managed service.
- `billing/` (future) — paid product plumbing.

These are the products; everything else is the runtime they run on.

## What Stays Open-Core Forever

These paths are committed to Apache 2.0 / MIT in the mirror ([oss-boundary.md](oss-boundary.md)):

- `cel/` crates — the Rust runtime.
- `mcp-server/` — the MCP tool surface.
- `cli/` — the `cellar` command-line interface.
- `adapter-common/` — the shared adapter SDK (the contract third parties build against).
- First-party adapters (`adapters/browser`, `adapters/excel`, etc.) — MIT.
- Benchmarks, eval harness, docs.

No plan to close these. The entire bet of the Apache flip is that an agent-platform-grade runtime is more valuable fully open than partially closed.

## Why This Works — Industry Precedents

| Company    | Open runtime                  | Commercial surface                 |
|------------|-------------------------------|------------------------------------|
| Docker     | Docker engine, CLI, containerd| Docker Desktop, Docker Hub Pro     |
| HashiCorp  | Terraform, Vault, Consul      | HCP (managed), enterprise editions |
| Elastic    | Elasticsearch (Apache 2.0)    | Elastic Cloud, enterprise features |
| GitLab     | CE (open)                     | EE (proprietary features + SaaS)   |

Pattern: the open runtime is the *adoption* vector. The commercial product is the *monetization* vector. Users pick up the open thing because it's fast; upgrade to the commercial thing when they cross a value threshold (team size, compliance, reliability).

CEL maps onto this pattern cleanly. The runtime is the adoption vector (agent platforms need a perception/execution layer; CEL is easy to try, run, and self-host). Cellar Cloud + the desktop app are the commercial surface for users who don't want to run infrastructure.

## What We Sacrificed With the License Flip

Being honest: Apache 2.0 is a real tradeoff, not a costless win.

- **AWS / GCP / Azure can wrap CEL as a service tomorrow.** They won't be the first call, because we'll ship managed earlier and integrate better. But they can do it.
- **A well-funded competitor can fork the runtime.** Forks happen when the community thinks the project is mismanaged. The protection is healthy maintenance, responsive community review, and staying ahead on adapter breadth.
- **We can't charge for the runtime itself.** Revenue must come from surfaces users cannot replicate cheaply (hosted ops, support, commercial-only packaging).

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
| Which revenue hypothesis is the primary product?                | Before Phase 3 hosted cloud planning.      |
| Do we run an adapter marketplace, or leave it to the community? | Before the adapter contribution flow ships.|
| Enterprise support: minimum viable tier?                        | Before first enterprise conversation.      |
| Cloud pricing model: per-minute vs. subscription vs. hybrid?    | Before Phase 3 pricing page.               |
| How aggressively do we sell against the "self-host for free"?   | Before Phase 3 launch messaging.           |

## Related Reading

- [oss-boundary.md](oss-boundary.md) — the exact OSS/commercial path-level boundary.
- [gtm-icp.md](gtm-icp.md) — who we sell to first and why.
- [stability.md](stability.md) — the API stability commitment enterprises need.
- [ROADMAP.md](ROADMAP.md) — Phase 3 is where Cellar Cloud lands.
- [adapters-cel-agents.md](adapters-cel-agents.md) — the north-star architecture behind the open-core boundary.
- [README.md](../README.md)
