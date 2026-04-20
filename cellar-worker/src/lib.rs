//! Cellar remote execution worker.
//!
//! Exposes the CEL goal runner over HTTP via the worker protocol documented at
//! `docs/worker-protocol.md`. Ships both a server (used by the `cellar-worker`
//! binary) and a client (`WorkerClient`) so other crates can talk to a remote
//! worker without duplicating wire-type definitions.
//!
//! ## Milestone 1.0 scope
//!
//! The server accepts goals and returns stubbed results. Real goal execution
//! (wiring up `cel-goal-runner::GoalRunner`) lands in Milestone 1.1.

pub mod client;
pub mod protocol;
pub mod server;
pub mod state;

pub use client::{ClientError, WorkerClient};
pub use protocol::{
    ErrorDetail, ErrorResponse, HealthResponse, JobDetails, JobStatus, SubmitGoalRequest,
    SubmitGoalResponse,
};
pub use server::{router, ServerState};
pub use state::JobStore;

/// Worker protocol version, surfaced on `GET /health`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
