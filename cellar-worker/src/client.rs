//! Reqwest-based client for the worker protocol.
//!
//! The eventual `RuntimeBackend::Remote` path in `cel-goal-runner` will use
//! this client to dispatch goals to a worker. Kept in the same crate as the
//! server so the wire types stay in lockstep.

use crate::protocol::{HealthResponse, JobDetails, SubmitGoalRequest, SubmitGoalResponse};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("http request failed: {0}")]
    Http(String),
    #[error("server returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("parse error: {0}")]
    Parse(String),
}

/// Client for talking to a `cellar-worker`.
#[derive(Clone)]
pub struct WorkerClient {
    base_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl WorkerClient {
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            token,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("build reqwest client"),
        }
    }

    /// `GET /health`. Always unauthenticated on the server side.
    pub async fn health(&self) -> Result<HealthResponse, ClientError> {
        let resp = self
            .http
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .map_err(|e| ClientError::Http(e.to_string()))?;
        parse_response(resp).await
    }

    /// `POST /v1/goals` — submit a goal for execution.
    pub async fn submit_goal(
        &self,
        req: SubmitGoalRequest,
    ) -> Result<SubmitGoalResponse, ClientError> {
        let mut builder = self
            .http
            .post(format!("{}/v1/goals", self.base_url))
            .json(&req);
        if let Some(t) = &self.token {
            builder = builder.bearer_auth(t);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| ClientError::Http(e.to_string()))?;
        parse_response(resp).await
    }

    /// `GET /v1/jobs/{id}` — fetch a job's current status + result.
    pub async fn get_job(&self, job_id: &str) -> Result<JobDetails, ClientError> {
        let mut builder = self
            .http
            .get(format!("{}/v1/jobs/{}", self.base_url, job_id));
        if let Some(t) = &self.token {
            builder = builder.bearer_auth(t);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| ClientError::Http(e.to_string()))?;
        parse_response(resp).await
    }

    /// Poll a job until it reaches a terminal status or `timeout_secs` elapses.
    /// Uses exponential backoff (50ms → 1s) between polls.
    ///
    /// On timeout, returns the most recent `JobDetails` (non-terminal). Callers
    /// that need to distinguish "timed out" from "finished" should check `status`.
    pub async fn wait_for_job(
        &self,
        job_id: &str,
        timeout_secs: u64,
    ) -> Result<JobDetails, ClientError> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(timeout_secs);
        let mut delay_ms = 50u64;
        loop {
            let details = self.get_job(job_id).await?;
            if details.status.is_terminal() {
                return Ok(details);
            }
            if start.elapsed() >= timeout {
                return Ok(details);
            }
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            delay_ms = (delay_ms.saturating_mul(2)).min(1000);
        }
    }
}

async fn parse_response<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, ClientError> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| ClientError::Http(e.to_string()))?;
    if !status.is_success() {
        return Err(ClientError::Status {
            status: status.as_u16(),
            body,
        });
    }
    serde_json::from_str(&body).map_err(|e| ClientError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::JobStatus;

    #[test]
    fn test_client_builds() {
        let client = WorkerClient::new("http://localhost:7777", None);
        assert_eq!(client.base_url, "http://localhost:7777");
        assert!(client.token.is_none());
    }

    #[test]
    fn test_terminal_status() {
        assert!(JobStatus::Succeeded.is_terminal());
        assert!(!JobStatus::Queued.is_terminal());
    }
}
