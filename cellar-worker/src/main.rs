//! `cellar-worker` binary entry point.
//!
//! Reads config from env vars:
//! - `CEL_WORKER_PORT`  — listen port (default 7777)
//! - `CEL_WORKER_BIND`  — bind address (default 0.0.0.0)
//! - `CEL_WORKER_TOKEN` — bearer token to require; unset = no auth
//! - `CEL_WORKER_STUB`  — set to `1` to skip Cortex boot and run in stub-only mode

use std::sync::Arc;

use cellar_worker::{router, JobStore, ServerState, VERSION};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let port: u16 = std::env::var("CEL_WORKER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7777);
    let bind = std::env::var("CEL_WORKER_BIND").unwrap_or_else(|_| "0.0.0.0".into());
    let auth_token = std::env::var("CEL_WORKER_TOKEN").ok();
    let stub_mode = matches!(
        std::env::var("CEL_WORKER_STUB").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    );

    if auth_token.is_none() {
        tracing::warn!(
            "CEL_WORKER_TOKEN is unset — worker will accept unauthenticated requests. \
             Set CEL_WORKER_TOKEN for any non-localhost / non-trusted-network deployment."
        );
    }

    let cortex = if stub_mode {
        tracing::warn!("CEL_WORKER_STUB=1 — skipping Cortex boot, executions will be stubbed");
        None
    } else {
        match boot_cortex().await {
            Ok(c) => {
                tracing::info!("Cortex booted — worker will execute goals for real");
                Some(c)
            }
            Err(e) => {
                tracing::warn!(
                    "Cortex boot failed: {e}. Continuing in stub mode. \
                     Check accessibility permissions / platform support."
                );
                None
            }
        }
    };

    let state = ServerState {
        store: JobStore::new(),
        auth_token,
        version: VERSION.into(),
        cortex,
        exec_lock: Arc::new(tokio::sync::Mutex::new(())),
    };
    let app = router(state);

    let addr = format!("{bind}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("cellar-worker v{} listening on {}", VERSION, addr);
    axum::serve(listener, app).await?;

    Ok(())
}

/// Boot a Cortex instance with the platform's accessibility tree.
///
/// Mirrors `cel-napi::cortex::boot_cortex`. Intentionally minimal — no adapter
/// discovery. Add adapter directories here when the worker needs first-party
/// adapters (Excel / SAP / etc.) loaded alongside.
async fn boot_cortex() -> Result<Arc<cel_cortex::Cortex>, String> {
    let a11y = cel_accessibility::create_tree();
    let merger = cel_context::ContextMerger::new(a11y);
    let observer = cel_accessibility::create_tree();

    // cellar-worker runs as a server process driving the worker's own
    // dedicated session — opt into native input. Eval/test runners should
    // not use this path.
    let mut cortex = cel_cortex::Cortex::new("cellar-worker".into()).with_native_input_unsafe();
    cortex
        .boot(merger, observer)
        .await
        .map_err(|e| format!("Cortex::boot: {e}"))?;

    Ok(Arc::new(cortex))
}
