//! Daemon-level IPC integration test.
//!
//! Wires the daemon's `StubHandler` into an in-process server, connects a
//! client over `tokio::io::duplex`, and exercises the locked methods the
//! v1 Phase 1 daemon supports: `system.hello` and `daemon.status`.
//!
//! When Phase 2+ overrides more handler methods, this test grows alongside
//! to keep coverage on the locked surface as it activates.

use std::sync::Arc;

use cel_cortex_daemon::Daemon;
use cellar_ipc::params::system::SystemHelloParams;
use cellar_ipc::results::daemon::DaemonStatusResult;
use cellar_ipc::results::system::SystemHelloResult;
use cellar_ipc::{serve_connection, Client};

#[tokio::test]
async fn daemon_ipc_handler_answers_hello_and_status() {
    let daemon = Daemon::wire_subsystems();
    let handler = Arc::clone(&daemon.ipc_handler);

    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        let _ = serve_connection(server_stream, handler).await;
    });
    let (client, _rx) = Client::from_stream(client_stream).await.unwrap();

    let hello: SystemHelloResult = client
        .call(
            "system.hello",
            SystemHelloParams {
                client_name: "cellar-test".into(),
                client_version: "0.0.1".into(),
                supported_protocol_versions: vec!["1".into()],
            },
        )
        .await
        .unwrap();
    assert_eq!(hello.protocol_version, "1");
    assert!(hello.capabilities.contains(&"memory.basic".into()));
    assert!(hello.capabilities.contains(&"gateway".into()));

    let status: DaemonStatusResult = client
        .call("daemon.status", serde_json::json!({}))
        .await
        .unwrap();
    assert!(status.healthy);
    // Daemon version matches the daemon's package version.
    assert_eq!(status.daemon_version, env!("CARGO_PKG_VERSION"));

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), server_task).await;
}
