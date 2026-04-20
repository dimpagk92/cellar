//! CEL Goal Runner — full Rust execution loop.
//!
//! Owns the entire perceive → plan → execute → verify → reflect → gate cycle.
//! Reads from Cortex directly. Plans via cel-planner directly. Dispatches
//! execution through Cortex adapter routing. Only event callbacks cross to TS.

pub mod callbacks;
pub mod checkpoint;
pub mod cognitive_trail;
pub mod config;
pub mod notebook;
pub mod outcome;
pub mod runner;
pub mod runtime_backend;
pub mod state;
pub mod strategy_router;
pub mod strategy_tracker;
pub mod verification;

pub use callbacks::{ExecutionCallbacks, NoOpCallbacks};
pub use checkpoint::CheckpointManager;
pub use cognitive_trail::CognitiveTrail;
pub use config::GoalConfig;
pub use notebook::Notebook;
pub use outcome::{ActionRecord, GoalResult, GoalStatus, GoalMetrics, StepOutcome};
pub use runner::GoalRunner;
pub use runtime_backend::{resolve_runtime_backend, RuntimeBackend};
pub use state::{RunnerState, RunnerPhase};
pub use strategy_router::{StrategyRoute, StrategySelection, select_route};
pub use strategy_tracker::StrategyTracker;
pub use verification::{VerificationResult, verify_action};
