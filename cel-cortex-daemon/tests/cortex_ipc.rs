//! `cortex.*` IPC methods against a daemon-hosted Cortex.
//!
//! Lives in its own integration-test binary (own process) because hosting
//! sets the process-global daemon cortex (`OnceLock`) — the lib test binary
//! relies on it staying unset to exercise the `CortexUnavailable` path.
//!
//! The Cortex here is constructed but NOT booted: no perception tick, no
//! AX / display / input access, so the test runs headless anywhere. `wait`
//! is a pure control action that dispatches without device IO.

use std::sync::Arc;

use cel_cortex_daemon::ipc::DaemonIpcHandler;
use cellar_ipc::error::IpcError;
use cellar_ipc::params::cortex::{CortexActParams, CortexPerceiveStartParams};
use cellar_ipc::Handler;
use cellar_rules_store::SqliteRulesStore;
use serde_json::json;

#[tokio::test]
async fn cortex_methods_drive_the_hosted_cortex() {
    cel_cortex_daemon::set_daemon_cortex(Arc::new(cel_cortex::Cortex::new("itest".into())));
    let store = SqliteRulesStore::in_memory().unwrap();
    let h = DaemonIpcHandler::new("test", store);

    // see → the (empty, unbooted) fused context serialises as an object.
    let see = h.cortex_see().await.unwrap();
    assert!(see.context.is_object());

    // act → a pure control action executes and reports success.
    let act = h
        .cortex_act(CortexActParams {
            action: json!({"type": "wait", "ms": 1}),
        })
        .await
        .unwrap();
    assert!(act.success, "wait should succeed: {:?}", act.error);

    // A payload that isn't a canonical action → typed InvalidParams.
    let err = h
        .cortex_act(CortexActParams {
            action: json!({"type": "not_a_real_action"}),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, IpcError::InvalidParams(_)));

    // perceive.start/stop scope the receipt run id (Receipt-Backed Run
    // Timeline) — receipts emitted while active group under this id.
    h.cortex_perceive_start(CortexPerceiveStartParams {
        run_id: "ipc-itest-run".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        cel_cortex::current_run_id().as_deref(),
        Some("ipc-itest-run")
    );

    let read = h.cortex_perceive_read().await.unwrap();
    assert!(read.model.is_object());

    h.cortex_perceive_stop().await.unwrap();
    assert!(cel_cortex::current_run_id().is_none());
}
