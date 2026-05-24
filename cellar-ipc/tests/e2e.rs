//! End-to-end IPC tests.
//!
//! Exercise the full chain: typed Rust call on the client → JSON-RPC envelope
//! on the wire → server dispatch → typed Handler method → response envelope
//! → typed result on the client. Two transports are exercised: an in-process
//! tokio::io::duplex pipe and a real Unix domain socket.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cellar_ipc::params::system::{SystemHelloParams, SystemShutdownParams};
use cellar_ipc::results::daemon::DaemonStatusResult;
use cellar_ipc::results::system::{SystemHelloResult, SystemShutdownResult};
use cellar_ipc::{serve_connection, Client, IpcError, Server, StubHandler};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

fn tmp_socket_path() -> PathBuf {
    // macOS UDS paths are limited to SUN_LEN (~104 chars). std::env::temp_dir
    // on macOS returns /var/folders/<hash>/T/... which is already 50+ chars
    // before we append. Drop the UUID and use a short random suffix.
    let suffix: u32 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    PathBuf::from(format!("/tmp/ic{}.sock", suffix))
}

async fn hello(client: &Client) -> SystemHelloResult {
    client
        .call::<_, SystemHelloResult>(
            "system.hello",
            SystemHelloParams {
                client_name: "cellar-test".into(),
                client_version: "0.0.1".into(),
                supported_protocol_versions: vec!["1".into()],
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn duplex_hello_status_shutdown() {
    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let handler = Arc::new(StubHandler::new("0.0.1"));
    let server_task = tokio::spawn(async move {
        let _ = serve_connection(server_stream, handler).await;
    });

    let (client, _notif_rx) = Client::from_stream(client_stream).await.unwrap();

    // system.hello negotiates the protocol version.
    let hello_resp = hello(&client).await;
    assert_eq!(hello_resp.protocol_version, "1");
    assert!(hello_resp.capabilities.contains(&"memory.basic".into()));
    assert!(hello_resp.capabilities.contains(&"gateway".into()));

    // daemon.status returns a real payload.
    let status: DaemonStatusResult = client
        .call("daemon.status", serde_json::json!({}))
        .await
        .unwrap();
    assert!(status.healthy);
    assert_eq!(status.daemon_version, "0.0.1");

    // system.shutdown succeeds; subsequent daemon.status reports the
    // shutting-down state.
    let sd: SystemShutdownResult = client
        .call(
            "system.shutdown",
            SystemShutdownParams { drain_timeout_s: 5 },
        )
        .await
        .unwrap();
    assert!(sd.shutting_down);

    let post: Result<DaemonStatusResult, _> =
        client.call("daemon.status", serde_json::json!({})).await;
    assert!(matches!(post.unwrap_err(), IpcError::ShuttingDown));

    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(1), server_task).await;
}

#[tokio::test]
async fn duplex_unknown_method_returns_method_not_found() {
    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let handler = Arc::new(StubHandler::new("0.0.1"));
    let task = tokio::spawn(async move {
        let _ = serve_connection(server_stream, handler).await;
    });
    let (client, _rx) = Client::from_stream(client_stream).await.unwrap();
    let _ = hello(&client).await;

    let err: Result<DaemonStatusResult, _> = client
        .call("rules.completely_made_up", serde_json::json!({}))
        .await;
    match err.unwrap_err() {
        IpcError::MethodNotFound(name) => assert!(name.contains("rules.completely_made_up")),
        other => panic!("expected MethodNotFound, got {other:?}"),
    }
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
}

#[tokio::test]
async fn duplex_unimplemented_method_returns_not_implemented() {
    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let handler = Arc::new(StubHandler::new("0.0.1"));
    let task = tokio::spawn(async move {
        let _ = serve_connection(server_stream, handler).await;
    });
    let (client, _rx) = Client::from_stream(client_stream).await.unwrap();
    let _ = hello(&client).await;

    // rules.list isn't backed yet in the stub.
    let err: Result<serde_json::Value, _> = client.call("rules.list", serde_json::json!({})).await;
    match err.unwrap_err() {
        IpcError::NotImplemented(method) => assert_eq!(method, "rules.list"),
        other => panic!("expected NotImplemented(rules.list), got {other:?}"),
    }
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
}

#[tokio::test]
async fn unsupported_protocol_version_rejected() {
    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let handler = Arc::new(StubHandler::new("0.0.1"));
    let task = tokio::spawn(async move {
        let _ = serve_connection(server_stream, handler).await;
    });
    let (client, _rx) = Client::from_stream(client_stream).await.unwrap();

    let err: Result<SystemHelloResult, _> = client
        .call(
            "system.hello",
            SystemHelloParams {
                client_name: "future-client".into(),
                client_version: "9.9.9".into(),
                supported_protocol_versions: vec!["99".into()],
            },
        )
        .await;
    match err.unwrap_err() {
        IpcError::UnsupportedProtocolVersion(client_versions) => {
            assert_eq!(client_versions, vec!["99".to_string()]);
        }
        other => panic!("expected UnsupportedProtocolVersion, got {other:?}"),
    }
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
}

#[tokio::test]
async fn uds_real_socket_round_trip() {
    let path = tmp_socket_path();
    let server = Server::bind(&path, StubHandler::new("0.0.1"))
        .await
        .unwrap();
    let socket_path = server.socket_path().to_path_buf();

    // Verify mode 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(&socket_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }

    let (ready_tx, ready_rx) = oneshot::channel();
    let mut tasks = JoinSet::new();
    let server_task = tokio::spawn(async move {
        ready_tx.send(()).unwrap();
        let _ = server.run(&mut tasks).await;
    });
    ready_rx.await.unwrap();
    // Tiny pause to let the listener actually start accepting.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let (client, _rx) = Client::connect_unix(&socket_path).await.unwrap();
    let hello = hello(&client).await;
    assert_eq!(hello.protocol_version, "1");
    let status: DaemonStatusResult = client
        .call("daemon.status", serde_json::json!({}))
        .await
        .unwrap();
    assert!(status.healthy);

    drop(client);
    server_task.abort();
    // Best-effort cleanup of the socket file.
    let _ = std::fs::remove_file(&socket_path);
}
