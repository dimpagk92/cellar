# Changelog

All notable changes to `cel-brief` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-`0.1.0` versions developed in-workspace; the first published crates.io
release is `0.1.0`.

## [Unreleased]

### Added — Phase 2 (`BriefBuilder` + tokenizer + budget)
- `BriefBuilder` with chainable `.source()`, `.governance()`, `.tokenizer()`,
  `.budget()`, `.strategy()`, and `.build(ctx)` methods. Fan-out via
  `futures_util::future::join_all`, ground-truth tokenization with the
  configured `Tokenizer`, importance-aware pruning, governance hook, and a
  fully populated `BriefReceipt`.
- `Tokenizer` trait + default `CharApproxTokenizer` (≈ 4 chars / token).
  Optional `TiktokenCl100k` behind a new `tiktoken` feature for ground-truth
  OpenAI / `cl100k_base` counts.
- `PruneStrategy` (`ImportanceFirst` default + `RoundRobin`) and
  `apply_budget` helper that honours per-priority floors with a borrow pass
  ("higher priority can borrow from a lower floor when over budget").
- `Governance` trait + `NoOpGovernance` default; `GovernanceVerdict` with
  `Allow` / `Redacted` / `Rejected` arms. Concrete policy implementations live
  downstream, not in this crate.
- Full `BriefReceipt` populated by the builder: per-source `SourceStats`,
  `DroppedContribution` records, redactions, and per-phase `Timings`
  (`fanout`, `tokenize`, `prune`, `governance`, `total`).
- `examples/standalone.rs` rewritten on top of `BriefBuilder` with four
  pluggable sources (system prompt, user message, fake memory, tool
  catalog), a tight budget that exercises pruning, and a printed receipt +
  full Brief JSON. Imports nothing from outside `cel-brief` (per OSS proof
  rule).

### Changed
- `cel-brief`'s dep set now includes `futures-util` (parallel fan-out) and a
  tokio runtime feature footprint of `["macros", "time"]`. Extraction
  allowlist in `scripts/lint-guard-extraction-crates.sh` updated to match.

## [0.1.0-pre] — 2026-05-23

### Added
- Phase 0 scaffolding: crate skeleton with module layout for `types`,
  `source`, `builder`, `governance`, `budget`, `tokenizer`, `error`,
  `receipt`. Bodies remain stubs — Phase 1+ implementation landed in the
  releases above.
- `BriefError` placeholder type with `Result` alias.
- `examples/standalone.rs` placeholder. Builds end-to-end once Phase 2
  (`BriefBuilder` + default `ImportanceFirst` pruning) lands.
- `memory` feature — opt-in dependency on `cel-memory` (the trait crate
  only; `cel-memory-sqlite` stays out of `cel-brief`'s graph).
- `perception` feature — placeholder for the upcoming `PerceptionSource`
  trait (Phase 4).

### Notes
- Imports only `cel-memory` (and only behind the `memory` feature) from
  the workspace — verified by
  `scripts/lint-guard-extraction-crates.sh` (added 2026-05-23).
- The trait surface (`Source`, `BriefBuilder`, `Governance`, `Tokenizer`)
  WILL break during the `0.1.0-pre` series. Extraction to crates.io is
  gated on the surface reaching v0.5+ internal stability.
