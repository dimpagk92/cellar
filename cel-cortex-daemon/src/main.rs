//! `cel-cortex-daemon` binary entry point.
//!
//! v1 Phase 0 skeleton: wire subsystems, log readiness, then exit. The Phase 1
//! work attaches an event bus, the Cortex goalless loop, the process poller,
//! the FSEvents adapter, and the `cel_act` gateway; this binary will run
//! indefinitely once those land.

use cel_cortex_daemon::Daemon;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,cel_cortex_daemon=debug".into()),
        )
        .init();

    tracing::info!("cel-cortex-daemon starting (v1 Phase 0 skeleton)");

    let daemon = Daemon::wire_subsystems();

    let stats = daemon.memory.stats().await?;
    tracing::info!(
        ?stats,
        "memory subsystem wired (BasicMemoryProvider — v1 stub)"
    );
    tracing::info!(
        "gateway subsystem wired (RecordingActuator + AutoAllowBroker + \
         empty StaticRules — v1 Phase 1 skeleton)"
    );
    // Quick sanity-check that the gateway routes a synthetic action all the
    // way through the matcher and into the memory audit trail.
    let outcome = daemon
        .gateway
        .intercept(cel_act_gateway::ProposedAction {
            caller: "system".into(),
            action_type: "ping".into(),
            action_args: serde_json::Value::Null,
            agent_session_id: None,
            project_root: None,
        })
        .await?;
    tracing::info!(?outcome, "gateway round-trip succeeded");

    let stats = daemon.memory.stats().await?;
    tracing::info!(
        ?stats,
        "memory updated after gateway round-trip (expect 1 Action chunk)"
    );

    // IPC handler is wired but not listening (binding the UDS socket is a
    // lifecycle decision per `cellar-app-v1.md` §14 — defer to integration
    // tests for now).
    tracing::info!(
        "ipc handler wired (StubHandler, protocol v1) — UDS bind deferred \
         to phase 1.x lifecycle work"
    );

    tracing::info!(
        "phase 0/1 skeleton ready. event bus, Cortex, FSEvents, webhook \
         sender, MCP server land in phase 1+. exiting."
    );

    Ok(())
}
