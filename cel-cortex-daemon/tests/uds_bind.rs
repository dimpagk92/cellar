//! Daemon-level UDS bind test.
//!
//! Wires the daemon's `Daemon::wire_subsystems()`, binds an IPC server on
//! a real `/tmp` UDS socket using the daemon's `Arc<StubHandler>`, connects
//! a [`Client`] over that socket, and round-trips `system.hello` +
//! `daemon.status` end-to-end. This is the closest test we can run to the
//! real `cel-cortex-daemon` binary's serve path without invoking the
//! ctrl-c lifecycle.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cel_cortex_daemon::Daemon;
use cellar_ipc::params::system::SystemHelloParams;
use cellar_ipc::results::daemon::DaemonStatusResult;
use cellar_ipc::results::system::SystemHelloResult;
use cellar_ipc::{Client, Server};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

fn tmp_socket_path() -> PathBuf {
    // macOS UDS paths are limited to SUN_LEN (~104 chars). `std::env::temp_dir`
    // returns `/var/folders/...` which is already past 50 chars on macOS, so
    // we use `/tmp/` directly with a short suffix.
    let suffix: u32 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    PathBuf::from(format!("/tmp/cellar-d{}.sock", suffix))
}

#[tokio::test]
async fn daemon_binds_socket_and_serves_real_clients() {
    let daemon = Daemon::wire_subsystems();
    let socket_path = tmp_socket_path();

    // Bind the IPC server using the daemon's own handler Arc.
    let server = Server::bind_with_arc(&socket_path, Arc::clone(&daemon.ipc_handler))
        .await
        .unwrap();
    let bound_path = server.socket_path().to_path_buf();

    // Verify mode 0600 on the socket file.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&bound_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }

    // Spawn the accept loop.
    let (ready_tx, ready_rx) = oneshot::channel();
    let server_task = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        ready_tx.send(()).unwrap();
        let _ = server.run(&mut tasks).await;
    });
    ready_rx.await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Real client connects over the real UDS socket.
    let (client, _notif_rx) = Client::connect_unix(&bound_path).await.unwrap();

    let hello: SystemHelloResult = client
        .call(
            "system.hello",
            SystemHelloParams {
                client_name: "cellar-daemon-test".into(),
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
    assert_eq!(status.daemon_version, env!("CARGO_PKG_VERSION"));

    // Clean shutdown.
    drop(client);
    server_task.abort();
    let _ = std::fs::remove_file(&bound_path);
}
