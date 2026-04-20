//! In-memory job store.
//!
//! Milestone 1.0: jobs live in a `HashMap` guarded by a `Mutex`. Lost on worker
//! restart. Milestone 2 will persist to SQLite (via `cel-store` or a dedicated
//! table) so long-running or batched workloads survive redeploys.

use crate::protocol::{JobDetails, JobStatus};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct JobStore {
    inner: Arc<Mutex<HashMap<String, JobDetails>>>,
}

impl JobStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, job: JobDetails) {
        self.inner
            .lock()
            .expect("JobStore mutex poisoned")
            .insert(job.job_id.clone(), job);
    }

    pub fn get(&self, job_id: &str) -> Option<JobDetails> {
        self.inner
            .lock()
            .expect("JobStore mutex poisoned")
            .get(job_id)
            .cloned()
    }

    pub fn update_status(
        &self,
        job_id: &str,
        status: JobStatus,
        result: Option<serde_json::Value>,
        error: Option<String>,
    ) {
        let mut guard = self.inner.lock().expect("JobStore mutex poisoned");
        if let Some(job) = guard.get_mut(job_id) {
            job.status = status;
            job.updated_at = now_epoch();
            if result.is_some() {
                job.result = result;
            }
            if error.is_some() {
                job.error = error;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("JobStore mutex poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Unix epoch seconds — worker protocol uses this for `created_at` / `updated_at`.
pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Generate a job id from a nanosecond-resolution timestamp.
/// Collision-free across a single worker instance; not globally unique.
pub fn generate_job_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("job_{nanos}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_job(id: &str) -> JobDetails {
        JobDetails {
            job_id: id.into(),
            status: JobStatus::Queued,
            created_at: now_epoch(),
            updated_at: now_epoch(),
            result: None,
            error: None,
        }
    }

    #[test]
    fn test_insert_and_get() {
        let store = JobStore::new();
        store.insert(sample_job("job_a"));
        let fetched = store.get("job_a").unwrap();
        assert_eq!(fetched.job_id, "job_a");
        assert_eq!(fetched.status, JobStatus::Queued);
    }

    #[test]
    fn test_get_missing() {
        let store = JobStore::new();
        assert!(store.get("missing").is_none());
    }

    #[test]
    fn test_update_status() {
        let store = JobStore::new();
        store.insert(sample_job("job_b"));
        store.update_status(
            "job_b",
            JobStatus::Succeeded,
            Some(serde_json::json!({"k": "v"})),
            None,
        );
        let fetched = store.get("job_b").unwrap();
        assert_eq!(fetched.status, JobStatus::Succeeded);
        assert_eq!(fetched.result.unwrap()["k"], "v");
    }

    #[test]
    fn test_generate_job_id_unique() {
        let a = generate_job_id();
        std::thread::sleep(std::time::Duration::from_nanos(1));
        let b = generate_job_id();
        assert_ne!(a, b);
        assert!(a.starts_with("job_"));
    }
}
