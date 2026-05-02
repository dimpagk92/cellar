# Claude Code Working Memory

This repository should be built around three layers:

- `Adapters`
- `CEL / crates`
- `Agents`

The durable value is in device understanding and execution, not in owning one planner.

## Repo Direction

- Adapters should be easy to build and extend.
- CEL should own context fusion, stream normalization, execution, and adapter routing.
- Agents should be pluggable: LangGraph, Mastra, Codex, Claude Code, GPT, Gemini, Cursor, n8n, or future in-house runtimes.

## What CEL Owns

- fused context from AX, CDP, vision, signals, network, audio, and adapters
- screenshot capture and runtime capability reporting
- adapter lifecycle and dispatch
- canonical action execution
- stable MCP / CLI / SDK / N-API surfaces
- memory/context management when it serves understanding and execution

## What CEL Does Not Need To Own Right Now

- one mandatory planner
- one mandatory orchestration runtime
- retry / branching / checkpoint policy as a repo-defining concern

Built-in planners and runners can exist, but they should be treated as clients, examples, or transitional implementations unless proven otherwise.

## Boundary Rules

- Keep the agent boundary generic.
- Preserve stable context, action, result, and adapter contracts.
- Keep improving AX and the shared crates even when an app later gets an adapter.
- Prefer app-specific structured truth in adapters over forcing everything through generic UI perception.
- Do not make LangGraph, Mastra, or any single runtime the identity of the platform.
- Do not design evals so they only make sense for one agent backend.

## Eval Rule

Prefer agent-agnostic evals that test CEL and adapter capabilities.
Runtime-specific evals are allowed, but they should be clearly isolated and secondary.

## Files To Read First

- [docs/adapters-cel-agents.md](docs/adapters-cel-agents.md)
- [docs/architecture.md](docs/architecture.md)
- [eval/scenarios/README.md](eval/scenarios/README.md)

Keep this file and `AGENTS.md` aligned.

## Bidirectional Sync Workflow (cellar-private ↔ dimpagk92/cellar)

**Source of truth: this private repo (`cellar-private`).** The public repo
(`dimpagk92/cellar`, checked out at `../cellar-oss/`) is generated from this
one via post-sync transforms (npm scope, repo URL, LICENSE copyright, CLI
bin name). External contributions land directly on the public repo and must
be reverse-synced back into private to survive future forward-syncs.

### The five-step rule

For ANY change that should ship to the public repo, follow this sequence
without skipping:

1. **Edit in `cellar-private`** (this repo). Never hand-edit `cellar-oss/`.
2. **Commit to `cellar-private`** before forward-syncing. The private commit
   is the durable record of the work; the OSS commit is generated.
3. **Forward sync**: `cd /Users/dimitriospagkratis/cellar && ./sync-to-oss.sh --apply`.
   Verifies build, runs post-sync transforms, mirrors files into `cellar-oss/`.
4. **Open PR + merge** on `dimpagk92/cellar`. Branch protection requires PR
   review and signed commits (the `cellar-oss/` git config has the right
   signing key via `includeIf`).
5. **Reverse sync if any external PR has merged**:
   `cd /Users/dimitriospagkratis/cellar && ./sync-from-oss.sh --apply`.
   Pulls non-owner OSS commits back into private with original attribution.

### Why this order matters

- **Skipping step 2 (commit private first)** means files exist only in
  `cellar-oss` working tree. If the OSS PR merges and someone runs
  `sync-to-oss.sh --apply` later, `rsync --delete` wipes the merged work.
  This is the silent-loss failure mode.
- **Skipping step 5 (reverse sync after external PRs)** has the same
  effect for contributor work: the next forward-sync deletes their files.

### Pre-flight: always run before pushing to OSS

Run the local mirror of GitHub Actions checks before any OSS push. It
catches Linux build failures and CodeQL high-severity alerts that would
fail CI:

```bash
/Users/dimitriospagkratis/cellar/preflight-cellar-oss.sh
```

Mirrors `Rust Linux` (Docker `rust:1-bookworm` + apt deps + libclang +
`cargo check --workspace --lib --bins --tests`) and `CodeQL` (JS/TS
query suite, filtered to alerts not pre-existing on `main`).

### Sanity check before declaring "done"

After pushing to OSS and the PR merges, verify:

```bash
# In cellar-private:
git status        # working tree clean (or only contains unrelated WIP)
git log -1        # the change is here

# In cellar-oss:
git fetch origin main
git log origin/main -1   # the squash-merge commit is here
diff <(cd /Users/dimitriospagkratis/cellar/cellar && git ls-files <changed-paths>) \
     <(cd /Users/dimitriospagkratis/cellar/cellar-oss && git ls-files <changed-paths>)
# expect empty diff for the changed files
```

If anything is in `cellar-oss/` but not `cellar-private/` (other than the
post-sync transforms), run `sync-from-oss.sh --apply` to reverse-sync it.

### What sync-to-oss.sh transforms (and what reverse-sync inverts)

| Transform | Forward (private → OSS) | Reverse (OSS → private) |
|---|---|---|
| CLI bin name | `cellar` → `cellar` | applied via patch sed (rare; most external PRs don't touch CLI bins) |
| npm scope | `@dpagk/cellar-napi` → `@dpagk/cellar-napi`, `@dpagk/cellar-mcp` → `@dpagk/cellar-mcp` | sed-inverted in patches |
| Repo URLs | `dimpagk92/cellar` → `dimpagk92/cellar` | not inverted (rarely matters in code patches) |
| LICENSE copyright | `Dilipod` → `Cellar Contributors` | not inverted (LICENSE rarely changed) |
| SECURITY.md email | `security@cellar.com` → GitHub advisory | not inverted |
| `mcp-server/package.json`, `cli/package.json`, CI workflows | authoritatively rewritten | **skipped** in reverse-sync (regenerates on next forward) |
| Excluded private-only paths | `benchmarks/`, `eval/`, `recorder/`, `live-view/`, `app/`, etc. | not relevant (don't exist in OSS) |
