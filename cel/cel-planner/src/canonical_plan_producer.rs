//! `PlanProducer` trait re-export.
//!
//! The trait and its `DoneVerdict` outcome moved to `cel-contracts` so the
//! cortex/runner side can describe the contract without depending on
//! `cel-planner`. They are re-exported here for backward compatibility;
//! prefer importing them from `cel_contracts` in new code.

pub use cel_contracts::{DoneVerdict, PlanProducer};
