//! Background cron sweeper for the memory subsystem.
//!
//! Runs three scheduled jobs per day on the daemon's tokio runtime:
//!
//! - **Aging sweep** — every day at the configured `aging_hour_utc`,
//!   calls [`MemoryProvider::run_aging_sweep`] to prune low-importance
//!   chunks past the retention horizon (see `cellar-memory-manager.md`
//!   §10.1).
//! - **Daily rollup** — every day at the configured `daily_rollup_hour_utc`,
//!   calls [`MemoryProvider::rollup_day`] for yesterday's chunks. The
//!   provider short-circuits when a rollup already exists.
//! - **Weekly rule rollup** — every Monday at the configured
//!   `weekly_rule_rollup_hour_utc`, walks the previous week's `Fire`
//!   chunks grouped by `rule_id` and calls
//!   [`MemoryProvider::rollup_rule_week`] for each distinct rule.
//!
//! The schedule lives in [`SweeperConfig`]. The default config is
//! disabled (`enabled = false`) so unit tests that spin up a `Daemon`
//! don't accidentally fire cron jobs. The daemon binary explicitly
//! enables the sweeper at boot.
//!
//! Cancellation: the returned [`tokio::task::JoinHandle`] is aborted by
//! the daemon's shutdown path. The sweeper has no other shutdown
//! signal — the `select!` loop wakes once a minute (the default tick)
//! to check the wall-clock against the next scheduled fire time.

use std::sync::Arc;
use std::time::Duration;

use cel_memory::{ChunkKind, MemoryProvider, MemoryQuery, RetrievalProfile};
use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, TimeZone, Timelike, Utc};
use tokio::task::JoinHandle;

/// Configuration for the sweeper (see [`spawn`]).
///
/// All times are interpreted in UTC. The hour fields are 0..24.
///
/// Defaults: disabled, so tests that construct a `Daemon` don't fire
/// cron jobs unintentionally. The daemon binary explicitly enables the
/// sweeper after wiring subsystems.
#[derive(Debug, Clone)]
pub struct SweeperConfig {
    /// Whether the sweeper runs at all. Default `false` for tests.
    pub enabled: bool,
    /// UTC hour-of-day (0..24) at which to run the aging sweep.
    pub aging_hour_utc: u32,
    /// UTC hour-of-day (0..24) at which to run the daily rollup.
    pub daily_rollup_hour_utc: u32,
    /// UTC hour-of-day (0..24) at which to run the weekly rule rollup
    /// (only fires on Mondays).
    pub weekly_rule_rollup_hour_utc: u32,
    /// How often the sweeper wakes to check whether a job is due.
    /// Default 60 s — fine-grained enough that a missed fire window
    /// can't slip a whole day.
    pub tick_interval: Duration,
}

impl Default for SweeperConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            aging_hour_utc: 4,
            daily_rollup_hour_utc: 4,
            weekly_rule_rollup_hour_utc: 5,
            tick_interval: Duration::from_secs(60),
        }
    }
}

impl SweeperConfig {
    /// Build a config wired for production: enabled, default hours.
    pub fn production() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

/// A clock seam. The sweeper consults `now()` to decide what to fire
/// and which day/week to roll up. Tests inject a [`FixedClock`] (or
/// any custom clock) to drive time without sleeping.
pub trait SweeperClock: Send + Sync + 'static {
    /// Return the current wall-clock time in UTC.
    fn now(&self) -> DateTime<Utc>;
}

/// Real wall-clock for production. Always returns [`Utc::now`].
pub struct WallClock;

impl SweeperClock for WallClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Test-only clock that returns a value the test can advance via
/// interior mutability. `now()` returns whatever was last `set()`.
pub struct FixedClock {
    inner: std::sync::Mutex<DateTime<Utc>>,
}

impl FixedClock {
    /// Construct a clock pinned to `t`.
    pub fn new(t: DateTime<Utc>) -> Self {
        Self {
            inner: std::sync::Mutex::new(t),
        }
    }

    /// Move the clock to a new time. Tests use this to simulate the
    /// passage of hours/days without sleeping.
    pub fn set(&self, t: DateTime<Utc>) {
        *self.inner.lock().unwrap() = t;
    }
}

impl SweeperClock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        *self.inner.lock().unwrap()
    }
}

/// State the sweeper carries across ticks so a single day's jobs
/// don't double-fire. Each `Option<NaiveDate>` is the date the
/// matching job last ran for; `None` means it hasn't fired yet this
/// process lifetime.
///
/// `pub` for the integration tests in `tests/sweeper.rs` which drive
/// [`run_once`] directly. Production callers use [`spawn`] and never
/// touch this.
#[derive(Debug, Default)]
pub struct SweeperState {
    last_aging_date: Option<NaiveDate>,
    last_daily_rollup_date: Option<NaiveDate>,
    last_weekly_rollup_week_start: Option<NaiveDate>,
}

/// Run one sweeper iteration. Public for tests so they can step the
/// loop deterministically without spawning a task.
///
/// `state` is mutated to record which jobs fired. Returns the set of
/// job kinds that executed in this pass.
pub async fn run_once(
    memory: &dyn MemoryProvider,
    cfg: &SweeperConfig,
    clock: &dyn SweeperClock,
    state: &mut SweeperState,
) -> Vec<SweepJobKind> {
    let mut fired = Vec::new();
    if !cfg.enabled {
        return fired;
    }
    let now = clock.now();
    let today = now.date_naive();

    // Aging sweep: at-most-once per UTC day, at or after the
    // configured hour.
    if now.hour() >= cfg.aging_hour_utc && state.last_aging_date != Some(today) {
        match memory.run_aging_sweep().await {
            Ok(report) => {
                tracing::info!(
                    deleted = report.deleted,
                    promoted = report.tier_promoted,
                    "sweeper: aging sweep complete"
                );
                fired.push(SweepJobKind::Aging);
            }
            Err(e) => {
                tracing::warn!(error = %e, "sweeper: aging sweep failed");
            }
        }
        state.last_aging_date = Some(today);
    }

    // Daily rollup: rolls up *yesterday's* chunks. Fires at or after
    // the configured hour, at-most-once per UTC day.
    if now.hour() >= cfg.daily_rollup_hour_utc && state.last_daily_rollup_date != Some(today) {
        let yesterday = today.pred_opt().unwrap_or(today);
        match memory.rollup_day(yesterday).await {
            Ok(rollups) => {
                tracing::info!(
                    date = %yesterday,
                    new_rollups = rollups.len(),
                    "sweeper: daily rollup complete"
                );
                fired.push(SweepJobKind::DailyRollup);
            }
            Err(cel_memory::MemoryError::NotImplemented(_)) => {
                tracing::debug!(
                    date = %yesterday,
                    "sweeper: daily rollup skipped (no summarizer attached)"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, date = %yesterday, "sweeper: daily rollup failed");
            }
        }
        state.last_daily_rollup_date = Some(today);
    }

    // Weekly rule rollup: runs Mondays at the configured hour. Rolls
    // up the *previous* ISO week (Mon..Sun ending on yesterday).
    if today.weekday() == chrono::Weekday::Mon && now.hour() >= cfg.weekly_rule_rollup_hour_utc {
        let last_week_start = today
            .checked_sub_days(chrono::Days::new(7))
            .unwrap_or(today);
        if state.last_weekly_rollup_week_start != Some(last_week_start) {
            match rollup_all_rules_for_week(memory, last_week_start).await {
                Ok(count) => {
                    tracing::info!(
                        week_start = %last_week_start,
                        rules_rolled_up = count,
                        "sweeper: weekly rule rollup complete"
                    );
                    fired.push(SweepJobKind::WeeklyRuleRollup);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        week_start = %last_week_start,
                        "sweeper: weekly rule rollup failed"
                    );
                }
            }
            state.last_weekly_rollup_week_start = Some(last_week_start);
        }
    }

    fired
}

/// Which sweep job ran. Returned by [`run_once`] so tests can assert
/// on the order of operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepJobKind {
    /// `MemoryProvider::run_aging_sweep` fired.
    Aging,
    /// `MemoryProvider::rollup_day` fired for yesterday.
    DailyRollup,
    /// `MemoryProvider::rollup_rule_week` fired for every rule with
    /// fires in the prior week.
    WeeklyRuleRollup,
}

/// Helper: discover every distinct `rule_id` in last week's `Fire`
/// chunks and roll each up. Returns how many rule_ids we attempted.
/// Errors from individual rule rollups are logged + swallowed so one
/// bad rule doesn't take the whole sweep down.
async fn rollup_all_rules_for_week(
    memory: &dyn MemoryProvider,
    week_start: NaiveDate,
) -> Result<usize, cel_memory::MemoryError> {
    let since = Utc.from_utc_datetime(&week_start.and_hms_opt(0, 0, 0).expect("midnight is valid"));
    let until = since + ChronoDuration::days(7);
    // Retrieve uses `Global` scope + caller_id "system" so the matcher
    // chunks (caller_id="matcher") are in scope. Up to 1024 fires per
    // week is plenty for v1; pathological weeks log a warning.
    let q = MemoryQuery {
        text: "rule fired".into(),
        kinds: Some(vec![ChunkKind::Fire]),
        since: Some(since),
        until: Some(until),
        session_id: None,
        caller_scope: cel_memory::CallerScope::Global,
        project_root_prefix: None,
        k: 1024,
        include_rollups: false,
        min_importance: None,
        profile: RetrievalProfile::AuditTimeline,
        caller_id: "system".into(),
    };
    let fires = memory.retrieve(q).await?;
    if fires.len() >= 1024 {
        tracing::warn!(
            count = fires.len(),
            "sweeper: weekly rule rollup hit the 1024 fire cap; some rules may be missed"
        );
    }
    let mut rule_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for f in &fires {
        if let Some(rid) = f.metadata.get("rule_id").and_then(|v| v.as_str()) {
            rule_ids.insert(rid.to_string());
        }
    }
    for rid in &rule_ids {
        match memory.rollup_rule_week(rid, week_start).await {
            Ok(r) => {
                tracing::debug!(rule_id = %rid, chunk_id = %r.id, "rollup_rule_week ok");
            }
            Err(cel_memory::MemoryError::NotImplemented(_)) => {
                tracing::debug!(
                    rule_id = %rid,
                    "sweeper: rollup_rule_week skipped (no summarizer)"
                );
                break; // no point trying further rules
            }
            Err(cel_memory::MemoryError::InvalidArgument(_)) => {
                // Already rolled up; expected on re-runs.
            }
            Err(e) => {
                tracing::warn!(error = %e, rule_id = %rid, "rollup_rule_week failed");
            }
        }
    }
    Ok(rule_ids.len())
}

/// Spawn the sweeper as a tokio task. Returns the [`JoinHandle`] for
/// the daemon shutdown path to abort.
///
/// The task wakes every `cfg.tick_interval`, checks the wall clock via
/// `clock`, and fires the matching jobs. The default clock is
/// [`WallClock`]; tests pass a [`FixedClock`] to drive time.
pub fn spawn(
    memory: Arc<dyn MemoryProvider>,
    cfg: SweeperConfig,
    clock: Arc<dyn SweeperClock>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if !cfg.enabled {
            tracing::info!("memory sweeper: disabled (cfg.enabled = false)");
            return;
        }
        tracing::info!(
            aging_hour = cfg.aging_hour_utc,
            daily_rollup_hour = cfg.daily_rollup_hour_utc,
            weekly_rollup_hour = cfg.weekly_rule_rollup_hour_utc,
            tick = ?cfg.tick_interval,
            "memory sweeper started"
        );
        let mut state = SweeperState::default();
        let mut ticker = tokio::time::interval(cfg.tick_interval);
        // First tick fires immediately; skip it so we don't fire a
        // half-config sweep before the daemon is fully booted.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let _ = run_once(memory.as_ref(), &cfg, clock.as_ref(), &mut state).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_memory::BasicMemoryProvider;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    #[tokio::test]
    async fn disabled_sweeper_does_nothing() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let cfg = SweeperConfig::default(); // enabled = false
        let clock = FixedClock::new(at(2026, 5, 22, 4));
        let mut state = SweeperState::default();
        let fired = run_once(memory.as_ref(), &cfg, &clock, &mut state).await;
        assert!(fired.is_empty());
    }

    #[tokio::test]
    async fn aging_fires_after_configured_hour() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let cfg = SweeperConfig::production();
        let clock = FixedClock::new(at(2026, 5, 22, 3)); // 3 AM, before 4
        let mut state = SweeperState::default();
        let fired = run_once(memory.as_ref(), &cfg, &clock, &mut state).await;
        assert!(
            fired.is_empty(),
            "no jobs should fire before configured hour"
        );
        clock.set(at(2026, 5, 22, 4)); // 4 AM, aging hour
        let fired = run_once(memory.as_ref(), &cfg, &clock, &mut state).await;
        assert!(fired.contains(&SweepJobKind::Aging));
    }

    #[tokio::test]
    async fn aging_fires_at_most_once_per_day() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let cfg = SweeperConfig::production();
        let clock = FixedClock::new(at(2026, 5, 22, 4));
        let mut state = SweeperState::default();
        let first = run_once(memory.as_ref(), &cfg, &clock, &mut state).await;
        assert!(first.contains(&SweepJobKind::Aging));
        // 6 AM same day — aging already ran today.
        clock.set(at(2026, 5, 22, 6));
        let second = run_once(memory.as_ref(), &cfg, &clock, &mut state).await;
        assert!(!second.contains(&SweepJobKind::Aging));
        // Next day — fires again.
        clock.set(at(2026, 5, 23, 4));
        let third = run_once(memory.as_ref(), &cfg, &clock, &mut state).await;
        assert!(third.contains(&SweepJobKind::Aging));
    }

    #[tokio::test]
    async fn daily_rollup_does_not_throw_on_no_summarizer() {
        // BasicMemoryProvider returns NotImplemented for rollup_day —
        // the sweeper must swallow that gracefully so the aging sweep
        // (which can run without a summarizer) still gets a chance.
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let cfg = SweeperConfig::production();
        let clock = FixedClock::new(at(2026, 5, 22, 4));
        let mut state = SweeperState::default();
        let fired = run_once(memory.as_ref(), &cfg, &clock, &mut state).await;
        // Aging fires; daily rollup attempted but swallowed.
        assert!(fired.contains(&SweepJobKind::Aging));
        assert!(
            !fired.contains(&SweepJobKind::DailyRollup),
            "NotImplemented should not count as a fire"
        );
    }

    #[tokio::test]
    async fn weekly_rule_rollup_fires_only_on_monday() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let cfg = SweeperConfig::production();
        // 2026-05-22 is a Friday. Move past both hour gates.
        let clock = FixedClock::new(at(2026, 5, 22, 6));
        let mut state = SweeperState::default();
        let fri = run_once(memory.as_ref(), &cfg, &clock, &mut state).await;
        assert!(!fri.contains(&SweepJobKind::WeeklyRuleRollup));

        // Monday 2026-05-25 at 5 AM (weekly rollup hour). The
        // BasicMemoryProvider has retrieve but its rollup_rule_week
        // is NotImplemented — our helper still records the Monday gate
        // by fired-job kind.
        let mut state2 = SweeperState::default();
        clock.set(at(2026, 5, 25, 5));
        let mon = run_once(memory.as_ref(), &cfg, &clock, &mut state2).await;
        assert!(
            mon.contains(&SweepJobKind::WeeklyRuleRollup),
            "weekly rollup should fire on Monday after the hour gate"
        );
    }
}
