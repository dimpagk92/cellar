# Security Policy

## Reporting a Vulnerability

**Do not open a public issue.** Report security vulnerabilities through GitHub's [private security advisory](https://github.com/dimpagk92/cellar/security/advisories/new) feature.

We aim to:
- Acknowledge within **72 hours**.
- Confirm or dispute within **7 days**.
- Land a fix within **30 days** for high-severity issues, **90 days** for medium/low.
- Credit reporters in the release notes (with permission).

## Scope

In scope:
- The `cel/` Rust crates (CEL core runtime).
- The TypeScript packages `agent/`, `cli/`, `mcp-server/`.
- The `cellar-worker` daemon + Docker image.
- First-party adapters in `adapters/`.
- Docs in `docs/` (content issues, not typos).

Out of scope:
- Third-party MCP clients / LLM providers.
- Services a user chooses to connect the runtime to.

## Supported Versions

Security fixes land on `main`. When we start tagging releases, the most recent minor version (current + one back) will receive backports for high-severity issues.

## Known-Dangerous Features

A few features can execute attacker-influenced content on the user's machine. Each is documented below with its risk and default posture. If you build on top of Cellar, understand these.

### LLM-generated JavaScript evaluation via CDP

**Location**: `agent/src/cdp-extractor.ts` (`extractByLlmScript`).

**Risk**: An LLM or a prompt-injected page could emit JavaScript that exfiltrates the DOM, cookies, or storage when evaluated against a real browser page.

**Default posture**: **Disabled.** The feature requires an explicit opt-in via `CEL_ENABLE_LLM_JS_EVAL=1`. When disabled, structured extraction paths are used instead.

**If you enable it**: the runtime applies a blocklist of dangerous patterns (`document.cookie`, `fetch(`, `eval(`, etc.) before evaluation. Blocklists are incomplete by design; only enable on trusted pages and trusted LLMs. A creative prompt injection *will* eventually find a pattern we haven't listed.

### Native input injection

**Location**: `cel/cel-input/`.

**Risk**: The runtime can move the mouse, click, and type on the user's active desktop. A malicious goal can take destructive actions on the user's behalf.

**Default posture**: macOS requires Accessibility permissions granted by the user. The runtime respects those boundaries but once granted, assume full local compromise potential. Do not run goals with untrusted instructions on a machine with sensitive UI open.

### Shell command execution in device baseline

**Location**: `agent/src/device-baseline.ts`.

**Risk**: The baseline collector runs local shell commands. Output size is not currently bounded — a pathological baseline can consume memory.

**Default posture**: Runs with the user's own shell privileges; no shell injection from untrusted input.

## Responsible Disclosure

If you find a vulnerability:

1. Open a [private security advisory](https://github.com/dimpagk92/cellar/security/advisories/new) with details and a proof-of-concept if safe to share.
2. Give us the timeline above to respond.
3. Please do not exploit the issue against systems you don't own, and don't access data that isn't yours.

Thank you — security reports make this safer for every Cellar user.
