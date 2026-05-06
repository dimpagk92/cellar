//! cel-cortex — Always-on perception engine for CEL.
//!
//! The Cortex maintains a continuously-updated mental model via a background
//! 200ms tick loop. It processes event streams from the accessibility tree
//! watchdog, diffs context snapshots, detects anomalies and loading states,
//! and classifies element stability.
//!
//! The mental model is wrapped in `Arc<RwLock>` for thread-safe concurrent
//! reads from IPC clients, Tauri commands, and the MCP sidecar.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │  Cortex (200ms tick loop)                   │
//! │  ├─ ContextMerger → ScreenContext           │
//! │  ├─ ContextWatchdog → CelEvent[]            │
//! │  ├─ Differ → PerceptionDiff                 │
//! │  ├─ Skeleton/Spinner detection              │
//! │  ├─ Anomaly detection (dedup + TTL)         │
//! │  └─ Element stability classification        │
//! │                                             │
//! │  MentalModel (Arc<RwLock>)                  │
//! │  ├─ current_context                         │
//! │  ├─ focused_element                         │
//! │  ├─ recent_diffs (rolling window)           │
//! │  ├─ temporal (loading, errors, idle)        │
//! │  ├─ stability (stable/volatile sets)        │
//! │  └─ anomaly_queue                           │
//! └─────────────────────────────────────────────┘
//!          │ read                    │ read
//!          ▼                         ▼
//!   IPC (Unix socket)         Tauri commands
//!   └─ MCP sidecar            └─ Frontend UI
//! ```

pub mod adapter;
pub mod anomaly;
pub mod cortex;
pub mod differ;
pub mod manager;
pub mod memory;
pub mod model;
pub mod planning_view;
pub mod process_driver;
pub mod skeleton;

// Re-export primary types for convenience.
pub use adapter::{
    discover_adapters, load_manifest, ActionDeclaration, ActionResult, AdapterDriver, AdapterError,
    AdapterManifest, AdapterState, ContextDeclaration, RegisteredAdapter,
};
pub use cortex::{Cortex, CortexError, TargetValidation};
pub use manager::CortexManager;
pub use memory::{Memory, MemoryEntry, MemoryError, MemoryLenses};
pub use model::{
    Anomaly, AnomalyType, DiffSummary, ElementStability, ErrorState, FocusedElement,
    FreshnessAssessment, FreshnessState, LoadingState, MentalModel, PerceptionDiff,
    SemanticInsight, SourceSummary, StalenessCause, TaskPhase, TemporalFlags,
};
pub use planning_view::{build_planning_view, PlanningViewInputs};
pub use process_driver::ProcessDriver;
