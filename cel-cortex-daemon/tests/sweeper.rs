//! Integration tests for the memory cron sweeper.
//!
//! These tests use the SQLite memory provider + a mock summarizer so
//! no LLM calls are issued. The clock is a [`FixedClock`] so we can
//! advance time deterministically.

use std::sync::Arc;

use cel_cortex_daemon::sweeper::{self, FixedClock, SweepJobKind, SweeperConfig};
use cel_memory::{ChunkKind, MemoryProvider, NewMemoryChunk};
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};
use chrono::{DateTime, TimeZone, Utc};

fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
}

fn chat(content: &str) -> NewMemoryChunk {
    NewMemoryChunk {
        kind: ChunkKind::Chat,
        source: cel_memory::ChunkSource::Embedded,
        session_id: None,
        project_root: None,
        caller_id: "embedded".into(),
        content: content.into(),
        metadata: serde_json::json!({}),
        importance: None,
        shareable: false,
        pinned: false,
    }
}

async fn backdate(provider: &SqliteMemoryProvider, chunk_id: &str, t: DateTime<Utc>) {
    use rusqlite::params;
    let conn = provider.conn_for_test();
    let guard = conn.lock().await;
    guard
        .execute(
            "UPDATE memory_chunks SET created_at = ? WHERE id = ?",
            params![t.timestamp_millis(), chunk_id],
        )
        .unwrap();
}

#[tokio::test]
async fn sweeper_runs_daily_rollup_after_time_advance() {
    // The full Phase 3 contract: a tick after the configured hour
    // produces a Rollup chunk for yesterday's chunks.
    let embedder = Arc::new(MockEmbedder::new());
    let summarizer = cel_memory::MockSummarizer::new("the day in review");
    let provider = Arc::new(
        SqliteMemoryProvider::open_in_memory(embedder)
            .await
            .unwrap()
            .with_summarizer(summarizer.clone()),
    );

    // Use dates relative to wall clock so the aging sweep (30-day horizon
    // from Utc::now()) does not evict backdated chunks before rollup runs.
    let today = Utc::now().date_naive();
    let yesterday = today.pred_opt().unwrap_or(today);
    let tick_at = Utc
        .from_utc_datetime(&today.and_hms_opt(4, 0, 0).expect("valid tick time"));

    // Plant two chunks "yesterday" and tick today at the rollup hour.
    let a = provider.write(chat("morning")).await.unwrap();
    let b = provider.write(chat("afternoon")).await.unwrap();
    backdate(
        provider.as_ref(),
        &a.id,
        Utc.from_utc_datetime(&yesterday.and_hms_opt(12, 0, 0).expect("valid time")),
    )
    .await;
    backdate(
        provider.as_ref(),
        &b.id,
        Utc.from_utc_datetime(&yesterday.and_hms_opt(14, 0, 0).expect("valid time")),
    )
    .await;

    let cfg = SweeperConfig::production();
    let clock = FixedClock::new(tick_at);
    let mut state = sweeper::SweeperState::default();
    let fired = sweeper::run_once(provider.as_ref(), &cfg, &clock, &mut state).await;
    assert!(
        fired.contains(&SweepJobKind::DailyRollup),
        "expected daily rollup; fired: {fired:?}"
    );

    // A rollup chunk now exists for yesterday.
    let stats = provider.stats().await.unwrap();
    // 2 original chunks + 1 rollup.
    assert_eq!(stats.total_chunks, 3);
    let calls = summarizer.calls();
    assert_eq!(calls.len(), 1);
    let expected_label = format!("day {yesterday}");
    assert_eq!(calls[0].kind_label.as_deref(), Some(expected_label.as_str()));
    assert_eq!(calls[0].chunk_ids.len(), 2);
}

#[tokio::test]
async fn sweeper_disabled_by_default_in_tests() {
    // Default config is `enabled = false` so unit tests can spin up
    // the daemon without firing cron jobs. This is the core safety
    // guarantee — production explicitly opts in via
    // `SweeperConfig::production()`.
    let embedder = Arc::new(MockEmbedder::new());
    let provider = Arc::new(
        SqliteMemoryProvider::open_in_memory(embedder)
            .await
            .unwrap(),
    );
    let cfg = SweeperConfig::default(); // enabled = false
    let clock = FixedClock::new(at(2026, 5, 23, 12));
    let mut state = sweeper::SweeperState::default();
    let fired = sweeper::run_once(provider.as_ref(), &cfg, &clock, &mut state).await;
    assert!(fired.is_empty());
}

#[tokio::test]
async fn sweeper_does_not_re_fire_within_same_day() {
    // Two ticks on the same UTC day should fire the aging sweep at
    // most once. The daily rollup is similarly idempotent (driven by
    // both the sweeper state AND the provider's own existing-rollup
    // check).
    let embedder = Arc::new(MockEmbedder::new());
    let summarizer = cel_memory::MockSummarizer::new("synthesis");
    let provider = Arc::new(
        SqliteMemoryProvider::open_in_memory(embedder)
            .await
            .unwrap()
            .with_summarizer(summarizer.clone()),
    );

    let cfg = SweeperConfig::production();
    let clock = FixedClock::new(at(2026, 5, 23, 4));
    let mut state = sweeper::SweeperState::default();

    let first = sweeper::run_once(provider.as_ref(), &cfg, &clock, &mut state).await;
    assert!(first.contains(&SweepJobKind::Aging));

    clock.set(at(2026, 5, 23, 12));
    let second = sweeper::run_once(provider.as_ref(), &cfg, &clock, &mut state).await;
    assert!(!second.contains(&SweepJobKind::Aging));
}

#[tokio::test]
async fn sweeper_skips_re_rollup_via_provider_idempotency() {
    // The sweeper's per-process state prevents double-fires within a
    // single day. Across daemon restarts, the *provider's* own
    // existing-rollup check is the safety net — make sure it engages.
    let embedder = Arc::new(MockEmbedder::new());
    let summarizer = cel_memory::MockSummarizer::new("synthesis");
    let provider = Arc::new(
        SqliteMemoryProvider::open_in_memory(embedder)
            .await
            .unwrap()
            .with_summarizer(summarizer.clone()),
    );
    let today = Utc::now().date_naive();
    let yesterday = today.pred_opt().unwrap_or(today);
    let tick_at = Utc
        .from_utc_datetime(&today.and_hms_opt(4, 0, 0).expect("valid tick time"));

    let a = provider.write(chat("morning")).await.unwrap();
    backdate(
        provider.as_ref(),
        &a.id,
        Utc.from_utc_datetime(&yesterday.and_hms_opt(12, 0, 0).expect("valid time")),
    )
    .await;

    let cfg = SweeperConfig::production();
    let clock = FixedClock::new(tick_at);

    // First pass: rollup fires.
    let mut state1 = sweeper::SweeperState::default();
    let fired1 = sweeper::run_once(provider.as_ref(), &cfg, &clock, &mut state1).await;
    assert!(fired1.contains(&SweepJobKind::DailyRollup));
    assert_eq!(summarizer.call_count(), 1);

    // Simulate a daemon restart (fresh state). The provider returns
    // empty rollups because one already exists; the sweeper still
    // counts as "fired" because rollup_day returned Ok — that's fine,
    // the summarizer was never reached.
    let mut state2 = sweeper::SweeperState::default();
    let _ = sweeper::run_once(provider.as_ref(), &cfg, &clock, &mut state2).await;
    assert_eq!(
        summarizer.call_count(),
        1,
        "summarizer must not be re-called for the same day"
    );
}
