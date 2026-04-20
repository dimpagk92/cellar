//! Integration tests: start the server on an ephemeral port, exercise the
//! full submit → poll → result flow via the real `WorkerClient`.

use cellar_worker::{
    protocol::{JobStatus, SubmitGoalRequest},
    router, ClientError, ServerState, WorkerClient,
};

/// Spawn the server on `127.0.0.1:0` and return the base URL once it's accepting connections.
async fn spawn_server(auth_token: Option<String>) -> String {
    let mut state = ServerState::stub("test");
    state.auth_token = auth_token;
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // axum::serve binds immediately on modern tokio, but give the scheduler a tick
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn end_to_end_submit_and_poll() {
    let base = spawn_server(None).await;
    let client = WorkerClient::new(&base, None);

    let health = client.health().await.unwrap();
    assert_eq!(health.status, "ok");
    assert_eq!(health.version, "test");

    let resp = client
        .submit_goal(SubmitGoalRequest {
            goal: "open github and star anthropic/anthropic-sdk-python".into(),
            config: None,
        })
        .await
        .unwrap();
    assert!(resp.job_id.starts_with("job_"));
    assert_eq!(resp.status, JobStatus::Queued);

    let final_details = client.wait_for_job(&resp.job_id, 5).await.unwrap();
    assert_eq!(final_details.status, JobStatus::Succeeded);
    assert_eq!(final_details.job_id, resp.job_id);

    let result = final_details.result.expect("stub produces a result");
    assert_eq!(result["status"], "stubbed");
    assert!(result["submitted_goal"]
        .as_str()
        .unwrap()
        .contains("github"));
}

#[tokio::test]
async fn get_unknown_job_returns_404() {
    let base = spawn_server(None).await;
    let client = WorkerClient::new(&base, None);

    let result = client.get_job("job_does_not_exist").await;
    match result {
        Err(ClientError::Status { status: 404, .. }) => {}
        other => panic!("expected 404, got: {other:?}"),
    }
}

#[tokio::test]
async fn auth_enforced_when_token_configured() {
    let base = spawn_server(Some("secret-token".into())).await;

    // No token → 401
    let no_token_client = WorkerClient::new(&base, None);
    let result = no_token_client
        .submit_goal(SubmitGoalRequest {
            goal: "x".into(),
            config: None,
        })
        .await;
    assert!(
        matches!(result, Err(ClientError::Status { status: 401, .. })),
        "expected 401 without token, got: {result:?}"
    );

    // Wrong token → 401
    let wrong_client = WorkerClient::new(&base, Some("wrong".into()));
    let result = wrong_client
        .submit_goal(SubmitGoalRequest {
            goal: "x".into(),
            config: None,
        })
        .await;
    assert!(
        matches!(result, Err(ClientError::Status { status: 401, .. })),
        "expected 401 with wrong token, got: {result:?}"
    );

    // Correct token → 202
    let ok_client = WorkerClient::new(&base, Some("secret-token".into()));
    let resp = ok_client
        .submit_goal(SubmitGoalRequest {
            goal: "x".into(),
            config: None,
        })
        .await
        .unwrap();
    assert_eq!(resp.status, JobStatus::Queued);

    // Health is always unauthenticated
    let health = no_token_client.health().await.unwrap();
    assert_eq!(health.status, "ok");
}
