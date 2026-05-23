//! Integration tests for the `subscription.gap` backpressure path
//! (IPC RFC §6).
//!
//! Each test wires a real [`Daemon`] + [`serve_connection`] over an
//! in-memory duplex pipe, sets up one of the subscription paths, and
//! either:
//! - publishes many frames while the client is not draining (standard
//!   subscriptions — `events.subscribe` is exercised here as the
//!   representative case), and verifies that the daemon emits a
//!   `subscription.gap` notification once the client catches up; or
//! - publishes frames on a critical subscription (`confirmation.subscribe`,
//!   `agent.chat.subscribe`) while the client is not draining, and
//!   verifies the daemon closes the connection rather than silently
//!   dropping frames.

use std::sync::Arc;
use std::time::Duration;

use cel_cortex_daemon::Daemon;
use cellar_ipc::params::confirmation::{
    ConfirmationSubscribeParams, PendingConfirmation, PendingRule,
};
use cellar_ipc::params::events::EventsSubscribeParams;
use cellar_ipc::params::stream_filter::StreamFilter;
use cellar_ipc::params::system::SystemHelloParams;
use cellar_ipc::params::{agent::AgentChatSubscribeParams, agent::AgentSessionsCreateParams};
use cellar_ipc::results::agent::AgentSessionsCreateResult;
use cellar_ipc::results::system::SystemHelloResult;
use cellar_ipc::results::SubscribeResult;
use cellar_ipc::subscription::StreamPayload;
use cellar_ipc::{serve_connection, Client};
use cellar_types::{Event, EventKind, EventSource};

/// Spawn a real daemon + serve_connection over a duplex stream. Returns
/// the daemon (so the test can publish into its buses), the client, and
/// the notification receiver.
///
/// Uses a tiny duplex buffer + leaves the notification receiver
/// undrained so the server-side per-connection mpsc fills quickly.
async fn wire_daemon_with_client() -> (
    Arc<Daemon>,
    Client,
    tokio::sync::mpsc::Receiver<cellar_ipc::client::NotificationMessage>,
    tokio::task::JoinHandle<()>,
) {
    let daemon = Arc::new(Daemon::wire_subsystems());
    let handler = Arc::clone(&daemon.ipc_handler);

    // Small duplex buffer so write-side backpressure surfaces quickly.
    let (server_stream, client_stream) = tokio::io::duplex(512);
    let server_task = tokio::spawn(async move {
        let _ = serve_connection(server_stream, handler).await;
    });
    let (client, notif_rx) = Client::from_stream(client_stream).await.unwrap();

    let _hello: SystemHelloResult = client
        .call(
            "system.hello",
            SystemHelloParams {
                client_name: "gap-test".into(),
                client_version: "0.0.1".into(),
                supported_protocol_versions: vec!["1".into()],
            },
        )
        .await
        .unwrap();

    (daemon, client, notif_rx, server_task)
}

/// Standard subscription (`events.subscribe`): subscribe, fire a huge
/// burst while leaving notifications undrained, then drain them and
/// look for the `subscription.gap` notification.
///
/// We use a generous burst (well above both the server-side per-connection
/// mpsc capacity AND the client-side notification buffer) so the test is
/// not flaky against the duplex / mpsc / pipe sizes.
#[tokio::test(flavor = "current_thread")]
async fn events_subscribe_emits_subscription_gap_on_slow_consumer() {
    let (daemon, client, mut notif_rx, server_task) = wire_daemon_with_client().await;

    let _sub: SubscribeResult = client
        .call(
            "events.subscribe",
            EventsSubscribeParams {
                filter: StreamFilter::default(),
            },
        )
        .await
        .unwrap();
    // Give the forwarder task time to attach.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Burst — enough to overrun the server's per-connection mpsc (256)
    // even after the duplex + client notification buffer (256) absorb
    // their share. 4000 events is comfortably above all three.
    for _ in 0..4000 {
        daemon
            .event_bus
            .publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));
    }
    // Let the forwarder process the burst.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now drain notifications and look for a Gap.
    let mut saw_gap = false;
    let mut total = 0u32;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), notif_rx.recv()).await {
            Ok(Some(n)) => {
                total += 1;
                if n.method == "subscription.gap" {
                    saw_gap = true;
                    // Sanity-check the payload shape per RFC §6.
                    let frame = n.params;
                    let dropped = frame
                        .get("dropped")
                        .and_then(|v| v.as_u64())
                        .expect("gap frame must carry numeric `dropped`");
                    assert!(
                        dropped > 0,
                        "gap should report at least one dropped frame, got 0"
                    );
                    let _ = frame
                        .get("since")
                        .expect("gap frame must carry `since` timestamp");
                    let _ = frame
                        .get("subscription_id")
                        .expect("gap frame must carry `subscription_id`");
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(
        saw_gap,
        "expected at least one subscription.gap notification \
         after a slow-consumer burst (saw {total} notifications)"
    );

    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(1), server_task).await;
}

/// Critical subscription (`confirmation.subscribe`): fill the
/// per-connection buffer; the daemon must drop the connection rather
/// than silently skip frames.
///
/// Uses the default multi-threaded runtime so the publisher, server
/// forwarder, and client reader can interleave the way they would in a
/// real daemon.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmation_subscribe_drops_connection_on_slow_consumer() {
    let (daemon, client, notif_rx, server_task) = wire_daemon_with_client().await;

    let _sub: SubscribeResult = client
        .call(
            "confirmation.subscribe",
            ConfirmationSubscribeParams {},
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Drop the notification receiver so the client's reader task exits
    // on the first frame it can't deliver. That stops the client side
    // from draining the duplex pipe, which in turn fills the server's
    // per-connection mpsc and trips critical overflow.
    drop(notif_rx);

    // Publish confirmation frames in small batches with brief sleeps so
    // the forwarder keeps draining the broadcast bus into the
    // per-connection mpsc — that's the layer whose backpressure this
    // test is exercising. Without the sleeps we'd just lag the bus
    // (capacity 256) and the per-connection mpsc would never fill.
    for batch in 0..40 {
        for i in 0..50 {
            let n = batch * 50 + i;
            daemon.confirmation_bus.publish(PendingConfirmation {
                id: format!("conf_{n}"),
                created_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::Duration::seconds(60),
                rule: PendingRule {
                    id: "r".into(),
                    name: "r".into(),
                    nl_original: "test".into(),
                },
                event: serde_json::json!({}),
                originating_action: serde_json::json!({}),
                caller: "test".into(),
                agent_session_id: None,
            });
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // The serve_connection task should observe the close hint and exit
    // within the test deadline. We don't care about the duplex teardown
    // ordering; we only care that the server task is gone.
    let outcome = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("server task must exit after critical-subscription overflow");
    assert!(
        outcome.is_ok(),
        "server task exit must not panic: {outcome:?}"
    );

    // The client should also observe its socket close on the next call.
    // We don't strictly assert this — the close mechanism is the server
    // side terminating, which the duplex stream surfaces to the client
    // as EOF the next time it reads. Asserting server-task exit is the
    // RFC-mandated behavior.
    drop(client);
}

/// Same critical-subscription check, but for `agent.chat.subscribe`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_chat_subscribe_drops_connection_on_slow_consumer() {
    let (daemon, client, notif_rx, server_task) = wire_daemon_with_client().await;

    // Create a session first — agent.chat.subscribe needs a session_id.
    let session: AgentSessionsCreateResult = client
        .call(
            "agent.sessions.create",
            AgentSessionsCreateParams {
                title: Some("gap test".into()),
            },
        )
        .await
        .unwrap();

    let _sub: SubscribeResult = client
        .call(
            "agent.chat.subscribe",
            AgentChatSubscribeParams {
                session_id: session.session_id.clone(),
            },
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Drop the notification receiver so the client's reader task exits
    // on the first frame it can't deliver — see the parallel comment in
    // `confirmation_subscribe_drops_connection_on_slow_consumer`.
    drop(notif_rx);

    // Hammer the chat bus with token frames for this session, in
    // batches so the forwarder keeps draining and the per-connection
    // mpsc actually fills (rather than the bus lagging first).
    for batch in 0..40 {
        for i in 0..50 {
            let n = batch * 50 + i;
            daemon
                .chat_bus
                .publish(cel_cortex_daemon::chat_bus::ChatBroadcast {
                    session_id: session.session_id.clone(),
                    payload: StreamPayload::Token {
                        request_id: "req_1".into(),
                        message_id: format!("msg_{n}"),
                        delta: format!("tok {n}"),
                    },
                });
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let outcome = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("server task must exit after agent.chat critical overflow");
    assert!(outcome.is_ok(), "server task exit must not panic");

    drop(client);
}
