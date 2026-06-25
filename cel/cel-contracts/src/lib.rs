//! Shared AI agent boundary contracts.
//!
//! This crate holds the data types that span the planner/runtime boundary so
//! neither side needs to depend on the other's implementation. Planners produce
//! these contract types; runtimes execute them and emit receipts.
//!
//! **Strict policy:** depends only on `cel-context`, `serde`, `serde_json`, and
//! `async-trait`. No runtime, no prompt logic, no LLM dependencies. Anything
//! that needs more belongs in a downstream planner or runtime crate.

pub mod actions;
pub mod canonical;
pub mod producer;
pub mod receipt;
pub mod view;

pub use actions::{CellWrite, EffectExpectation, PlannedAction};
pub use canonical::{
    AttemptRecord, FailureReport, GoalOutcome, NextMove, RunLimits, RuntimeCaps, Step, StepKind,
    StepResult,
};
pub use producer::{DoneVerdict, NextActionHint, PlanProducer};
pub use receipt::{
    DispatchRoute, ExecutionReceipt, ObservedEffect, ObservedKind, ObservedStatus, ReceiptStatus,
};
pub use view::{
    AdapterActionRef, AdapterFactRef, AnomalyRef, Blocker, CapabilityRef, EventRef, EvidenceRef,
    KnowledgeRef, MemoryRef, OmittedCounts, PlanningBudget, PlanningElement, PlanningElementState,
    PlanningScreen, PlanningView, RunProgress,
};
