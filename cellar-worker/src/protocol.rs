//! Wire types for the worker protocol (see `docs/worker-protocol.md`).
//!
//! These are the JSON shapes exchanged between client and server. Kept free of
//! cel-goal-runner dependencies so both sides can serialize/deserialize without
//! pulling the full runner. `GoalConfig` overrides travel as opaque
//! `serde_json::Value` — the server parses them against its own schema.

use serde::{Deserialize, Serialize};

/// POST /v1/goals request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitGoalRequest {
    pub goal: String,
    /// Optional `GoalConfig` override. Schema matches `cel-goal-runner::GoalConfig`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// POST /v1/goals response body (202 Accepted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitGoalResponse {
    pub job_id: String,
    pub status: JobStatus,
    pub created_at: i64,
}

/// Lifecycle state of a submitted job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

/// GET /v1/jobs/{id} response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobDetails {
    pub job_id: String,
    pub status: JobStatus,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// GET /health response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Standard error envelope for non-2xx responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Machine-readable tag: `unauthorized`, `not_found`, `bad_request`, `internal`.
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_status_serde() {
        assert_eq!(
            serde_json::to_string(&JobStatus::Queued).unwrap(),
            "\"queued\""
        );
        let parsed: JobStatus = serde_json::from_str("\"succeeded\"").unwrap();
        assert_eq!(parsed, JobStatus::Succeeded);
    }

    #[test]
    fn test_is_terminal() {
        assert!(!JobStatus::Queued.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
        assert!(JobStatus::Succeeded.is_terminal());
        assert!(JobStatus::Failed.is_terminal());
    }

    #[test]
    fn test_submit_goal_request_minimal() {
        let json = r#"{"goal":"open github"}"#;
        let req: SubmitGoalRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.goal, "open github");
        assert!(req.config.is_none());
    }

    #[test]
    fn test_job_details_roundtrip() {
        let details = JobDetails {
            job_id: "job_123".into(),
            status: JobStatus::Succeeded,
            created_at: 1000,
            updated_at: 1100,
            result: Some(serde_json::json!({"ok": true})),
            error: None,
        };
        let json = serde_json::to_string(&details).unwrap();
        let back: JobDetails = serde_json::from_str(&json).unwrap();
        assert_eq!(back.job_id, "job_123");
        assert_eq!(back.status, JobStatus::Succeeded);
        assert!(back.error.is_none());
    }
}
