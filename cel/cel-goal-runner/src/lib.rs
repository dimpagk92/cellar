//! CEL Goal Runner — canonical agent loop.
//!
//! One entry point (`CanonicalGoalRunner::run`), one loop, one retry
//! semantic (3 strikes per step → `FailureReport`), one outcome shape.
//! Callers are thin shims: CLI, MCP server, eval harness, benchmarks
//! each do 10–20 lines and hand off here.
//!
//! See `docs/canonical-agent-plan.md` for the motivating plan and the
//! invariants the loop guarantees.

pub mod canonical_runner;
pub mod config;
pub mod outcome;
pub mod runtime_backend;

pub use canonical_runner::{CanonicalGoalRunner, CortexStepExecutor, StepExecutor};
pub use config::GoalConfig;
pub use outcome::{ActionRecord, GoalMetrics, GoalResult, GoalStatus, StepOutcome};
pub use runtime_backend::{resolve_runtime_backend, RuntimeBackend};
