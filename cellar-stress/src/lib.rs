//! `cellar-stress` — synthetic load harness for the Cellar daemon.
//!
//! Drives the in-process daemon (`Daemon::wire_subsystems()`) with a mix of
//! filesystem, gateway, agent-chat, and memory-write load while sampling
//! daemon health every minute. Outputs a stream of JSONL metric lines and a
//! summary verdict at end-of-run.
//!
//! ## Phase 5 acceptance gate (from `cellar-app-v1.md` §16)
//!
//! The binary is **the** stress test entry point. The acceptance gate is
//! encoded in [`thresholds`] — the binary exits non-zero if any threshold
//! tripped during the run:
//!
//! - resident memory > 500 MiB
//! - per-method `retrieve` p95 > 200 ms (per `cellar-memory-manager.md` §14.4)
//! - daemon log error rate > 0.1/min (we use IPC-error rate as the proxy
//!   since the harness can't tap the daemon's tracing layer in-process)
//!
//! ## What this harness *does* drive
//!
//! - **File system load:** `tempfile::TempDir` + `tokio::fs` create/modify/delete.
//!   Note: the daemon's FSEvents adapter is **not** wired by [`Daemon::wire_subsystems`]
//!   (the daemon binary spawns it, the library does not). The harness publishes
//!   synthetic `Fsevents` events onto the in-process [`EventBus`] instead, which
//!   exercises the matcher pipeline end-to-end without needing the macOS adapter.
//! - **Process events:** synthetic `ProcessStarted`/`ProcessStopped` published
//!   to the event bus — same reason as above.
//! - **Gateway calls:** [`AgentGateway::intercept_tool_call`] with a benign
//!   `ping` action, which writes Action + Fire chunks to memory.
//! - **Agent chats:** memory writes through `MemoryProvider::write` with
//!   `ChunkKind::Chat` and an open session. The harness does **not** invoke
//!   the embedded agent runtime (requires an LLM provider; not available in
//!   the bench environment).
//! - **Memory writes:** direct calls to `MemoryProvider::write` plus paired
//!   `retrieve` calls for latency benchmarking.
//!
//! ## What this harness *does not* drive (gaps)
//!
//! - The real macOS FSEvents adapter, process poller, and signals poller.
//!   These spawn in the daemon binary's `main.rs` and aren't reachable from
//!   library-level wiring. The synthetic event-bus injection covers the
//!   matcher consumer task and the IPC `events.*` forwarders, which is what
//!   we actually want to stress-test.
//! - End-to-end IPC over the Unix socket. The harness drives the
//!   `DaemonIpcHandler` trait directly (no socket round-trip) so the
//!   per-call wall-clock numbers exclude framing and codec overhead. This
//!   is intentional: the gateway and memory provider are the hot paths.
//!   An IPC-level benchmark would belong in a separate fixture.
//! - The webhook sender. Webhooks fan out off the hot path; including them
//!   in the stress mix would just exercise `reqwest` against localhost
//!   refusing connections.

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod cli;
pub mod harness;
pub mod load;
pub mod metrics;
pub mod report;
pub mod thresholds;

pub use cli::{Args, LoadProfileArg};
pub use harness::{run, HarnessExit, HarnessOutcome};
pub use load::LoadProfile;
pub use metrics::{LatencyDistribution, MetricSample, MetricStream};
pub use report::Summary;
pub use thresholds::{ThresholdBreach, ThresholdViolation, Thresholds};
