//! Adaptive per-app throttling for accessibility tree walks.
//!
//! Tracks walk cost per foreground app and backs off for expensive ones
//! (e.g. Electron apps whose UIA/AX providers block the UI thread).
//! Pattern adapted from screenpipe-a11y (MIT). Pure logic — no I/O,
//! no platform deps, fully unit-testable.
//!
//! Integrate by calling `should_walk(app)` before a walk and
//! `record_walk(app, duration, truncated)` after. The returned
//! `WalkDecision` carries overrides for `max_nodes` and `walk_timeout`.
//!
//! This is what keeps continuous perception under a few % CPU even
//! when the focused window is a heavy Electron app.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How aggressively to throttle an app's walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkTier {
    /// < 50ms avg — default behavior.
    Light,
    /// 50–150ms avg — reduced frequency.
    Moderate,
    /// 150–250ms avg — significantly reduced.
    Heavy,
    /// > 250ms avg or repeated truncations — minimal walking (60s interval).
    Critical,
}

/// Decision returned by [`AppWalkBudget::should_walk`].
#[derive(Debug, Clone, Copy)]
pub struct WalkDecision {
    /// Whether the caller should proceed with the walk right now.
    pub walk: bool,
    /// Per-walk override for `TreeWalkerConfig::max_nodes`.
    pub max_nodes: usize,
    /// Per-walk override for `TreeWalkerConfig::walk_timeout`.
    pub timeout: Duration,
    /// Current tier (exposed for logging/metrics).
    pub tier: WalkTier,
}

const WINDOW_SIZE: usize = 8;

const MODERATE_THRESHOLD: Duration = Duration::from_millis(50);
const HEAVY_THRESHOLD: Duration = Duration::from_millis(150);
const CRITICAL_THRESHOLD: Duration = Duration::from_millis(250);

/// If this many recent walks in the window were truncated, escalate to Critical.
const TRUNCATION_ESCALATE_COUNT: u32 = 3;

/// After this long in the background, start decaying the tier back to Light.
const DECAY_IDLE_THRESHOLD: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct AppCost {
    durations: Vec<Duration>,
    truncation_count: u32,
    last_walk: Instant,
    tier: WalkTier,
}

impl AppCost {
    fn new() -> Self {
        Self {
            durations: Vec::with_capacity(WINDOW_SIZE),
            truncation_count: 0,
            last_walk: Instant::now()
                .checked_sub(Duration::from_secs(600))
                .unwrap_or_else(Instant::now),
            tier: WalkTier::Light,
        }
    }

    fn avg_duration(&self) -> Duration {
        if self.durations.is_empty() {
            return Duration::ZERO;
        }
        let sum: Duration = self.durations.iter().sum();
        sum / self.durations.len() as u32
    }

    fn record(&mut self, duration: Duration, truncated: bool) {
        if self.durations.len() >= WINDOW_SIZE {
            self.durations.remove(0);
            self.truncation_count = self.truncation_count.saturating_sub(1);
        }
        self.durations.push(duration);
        if truncated {
            self.truncation_count += 1;
        }
        self.last_walk = Instant::now();
        self.tier = self.compute_tier();
    }

    fn compute_tier(&self) -> WalkTier {
        if self.truncation_count >= TRUNCATION_ESCALATE_COUNT {
            return WalkTier::Critical;
        }
        let avg = self.avg_duration();
        if avg >= CRITICAL_THRESHOLD {
            WalkTier::Critical
        } else if avg >= HEAVY_THRESHOLD {
            WalkTier::Heavy
        } else if avg >= MODERATE_THRESHOLD {
            WalkTier::Moderate
        } else {
            WalkTier::Light
        }
    }

    fn maybe_decay(&mut self) {
        if self.last_walk.elapsed() > DECAY_IDLE_THRESHOLD
            && self.tier != WalkTier::Light
            && self.durations.len() > 2
        {
            self.durations.drain(..self.durations.len() / 2);
            self.truncation_count = self.truncation_count.saturating_sub(2);
            self.tier = self.compute_tier();
        }
    }
}

/// Cost tracker keyed by app name. Not thread-safe — owned by the capture loop.
pub struct AppWalkBudget {
    apps: HashMap<String, AppCost>,
}

impl AppWalkBudget {
    pub fn new() -> Self {
        Self {
            apps: HashMap::new(),
        }
    }

    pub fn should_walk(&mut self, app_name: &str) -> WalkDecision {
        let cost = self
            .apps
            .entry(app_name.to_string())
            .or_insert_with(AppCost::new);
        cost.maybe_decay();

        let (min_interval, max_nodes, timeout) = match cost.tier {
            WalkTier::Light => (
                Duration::from_millis(200),
                5_000usize,
                Duration::from_millis(250),
            ),
            WalkTier::Moderate => (
                Duration::from_secs(2),
                2_000usize,
                Duration::from_millis(200),
            ),
            WalkTier::Heavy => (
                Duration::from_secs(5),
                1_000usize,
                Duration::from_millis(150),
            ),
            WalkTier::Critical => (
                Duration::from_secs(60),
                500usize,
                Duration::from_millis(100),
            ),
        };

        WalkDecision {
            walk: cost.last_walk.elapsed() >= min_interval,
            max_nodes,
            timeout,
            tier: cost.tier,
        }
    }

    pub fn record_walk(&mut self, app_name: &str, duration: Duration, truncated: bool) {
        let cost = self
            .apps
            .entry(app_name.to_string())
            .or_insert_with(AppCost::new);
        cost.record(duration, truncated);
    }

    pub fn tier_of(&self, app_name: &str) -> Option<WalkTier> {
        self.apps.get(app_name).map(|c| c.tier)
    }
}

impl Default for AppWalkBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_app_walks_at_light() {
        let mut b = AppWalkBudget::new();
        let d = b.should_walk("discord.exe");
        assert!(d.walk);
        assert_eq!(d.tier, WalkTier::Light);
        assert_eq!(d.max_nodes, 5_000);
    }

    #[test]
    fn escalates_to_moderate_above_50ms() {
        let mut b = AppWalkBudget::new();
        for _ in 0..4 {
            b.record_walk("app", Duration::from_millis(80), false);
        }
        assert_eq!(b.should_walk("app").tier, WalkTier::Moderate);
    }

    #[test]
    fn escalates_to_heavy_above_150ms() {
        let mut b = AppWalkBudget::new();
        for _ in 0..4 {
            b.record_walk("app", Duration::from_millis(200), false);
        }
        assert_eq!(b.should_walk("app").tier, WalkTier::Heavy);
    }

    #[test]
    fn escalates_to_critical_above_250ms() {
        let mut b = AppWalkBudget::new();
        for _ in 0..4 {
            b.record_walk("app", Duration::from_millis(300), false);
        }
        let d = b.should_walk("app");
        assert_eq!(d.tier, WalkTier::Critical);
        assert_eq!(d.max_nodes, 500);
    }

    #[test]
    fn repeated_truncations_force_critical() {
        let mut b = AppWalkBudget::new();
        for _ in 0..3 {
            b.record_walk("app", Duration::from_millis(30), true);
        }
        assert_eq!(b.should_walk("app").tier, WalkTier::Critical);
    }

    #[test]
    fn min_interval_blocks_immediate_rewalk() {
        let mut b = AppWalkBudget::new();
        for _ in 0..4 {
            b.record_walk("app", Duration::from_millis(80), false);
        }
        assert!(!b.should_walk("app").walk);
    }

    #[test]
    fn different_apps_are_independent() {
        let mut b = AppWalkBudget::new();
        for _ in 0..4 {
            b.record_walk("heavy", Duration::from_millis(200), false);
        }
        assert_eq!(b.should_walk("notepad").tier, WalkTier::Light);
        assert_eq!(b.should_walk("heavy").tier, WalkTier::Heavy);
    }

    #[test]
    fn rolling_window_recovers_tier() {
        let mut b = AppWalkBudget::new();
        for _ in 0..WINDOW_SIZE {
            b.record_walk("app", Duration::from_millis(200), false);
        }
        assert_eq!(b.should_walk("app").tier, WalkTier::Heavy);
        for _ in 0..WINDOW_SIZE {
            b.record_walk("app", Duration::from_millis(10), false);
        }
        assert_eq!(b.should_walk("app").tier, WalkTier::Light);
    }
}
