//! Liveness API tests (Phase 1 of runner-production plan).
//!
//! Exercises the atomic mirrors (`tick_count`, `stalled_ticks`,
//! `last_tick_age_ms`) and `refresh_now()` against an isolated Cortex with
//! StubAccessibility — no OS permissions, no real merger latency.

use cel_accessibility::StubAccessibility;
use cel_cortex::{Cortex, CortexError};
use std::time::Duration;

// ─── Baseline state (pre-boot) ──────────────────────────────────────────────

#[tokio::test]
async fn tick_count_is_zero_before_boot() {
    let (cortex, _) = Cortex::isolated("liveness-pre-boot");
    assert_eq!(cortex.tick_count(), 0);
    assert_eq!(cortex.stalled_ticks(), 0);
    assert!(cortex.last_tick_age_ms().is_none());
}

#[tokio::test]
async fn refresh_now_errors_when_not_running() {
    let (cortex, _) = Cortex::isolated("liveness-not-running");
    let err = cortex.refresh_now(Some(50)).await.unwrap_err();
    matches!(err, CortexError::NotRunning(_));
}

// ─── After boot, the tick loop advances state ───────────────────────────────

#[tokio::test]
async fn tick_count_advances_under_natural_cadence() {
    let (mut cortex, merger) = Cortex::isolated("liveness-natural");
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    // Give the loop a few 200ms ticks worth of headroom.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let count = cortex.tick_count();
    assert!(
        count >= 2,
        "expected ≥2 natural ticks in 700ms, got {count}"
    );
    assert!(
        cortex.last_tick_age_ms().is_some(),
        "age must be reported after first tick"
    );
    cortex.shutdown();
}

#[tokio::test]
async fn last_tick_age_is_small_right_after_tick() {
    let (mut cortex, merger) = Cortex::isolated("liveness-age-small");
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    // Force a tick, then immediately sample age.
    let _ = cortex.refresh_now(Some(500)).await.unwrap();
    let age = cortex
        .last_tick_age_ms()
        .expect("age must be set after refresh");
    assert!(
        age < 100,
        "age right after refresh should be <100ms, got {age}ms"
    );
    cortex.shutdown();
}

// ─── refresh_now behavior ───────────────────────────────────────────────────

#[tokio::test]
async fn refresh_now_advances_tick_count() {
    let (mut cortex, merger) = Cortex::isolated("liveness-refresh");
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    let baseline = cortex.tick_count();
    let after = cortex.refresh_now(Some(500)).await.unwrap();
    assert!(
        after > baseline,
        "refresh_now should advance tick_count past baseline ({baseline}), got {after}"
    );
    cortex.shutdown();
}

#[tokio::test]
async fn refresh_now_concurrent_callers_all_succeed() {
    let (mut cortex, merger) = Cortex::isolated("liveness-concurrent");
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    // Ten simultaneous refresh_now calls; each should eventually see a
    // tick_count greater than the baseline they captured. We're not asserting
    // all ten trigger distinct ticks — the notify_one semantics coalesce
    // wake-ups — only that none time out.
    let cortex_arc = std::sync::Arc::new(cortex);
    let mut handles = Vec::new();
    for _ in 0..10 {
        let c = cortex_arc.clone();
        handles.push(tokio::spawn(
            async move { c.refresh_now(Some(1_000)).await },
        ));
    }
    for h in handles {
        let result = h.await.expect("task panicked");
        assert!(
            result.is_ok(),
            "concurrent refresh_now returned {:?}",
            result
        );
    }
    cortex_arc.shutdown();
}

#[tokio::test]
async fn refresh_now_does_not_break_natural_cadence() {
    let (mut cortex, merger) = Cortex::isolated("liveness-cadence");
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    // Do one forced tick, then rely on the natural cadence for more. The
    // select! loop must still wake on the interval — otherwise forcing a
    // refresh would accidentally reset or starve the timer.
    let after_refresh = cortex.refresh_now(Some(500)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let later = cortex.tick_count();
    assert!(
        later >= after_refresh + 1,
        "natural ticks must keep firing after refresh_now: was {after_refresh}, now {later}"
    );
    cortex.shutdown();
}

// NOTE: The RefreshTimeout / stalled_ticks error path is not testable here
// without a hanging-merger fixture — against StubAccessibility every tick
// completes in microseconds, so any shutdown race loses to an in-flight
// tick (confirmed empirically: shutdown 20ms after refresh_now still sees
// the tick fire first). Phase 5's soak suite introduces a mock merger
// that can be told to sleep arbitrarily, which will cover this branch.

#[tokio::test]
async fn stalled_ticks_stays_zero_in_healthy_run() {
    let (mut cortex, merger) = Cortex::isolated("liveness-healthy");
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();

    // A bunch of forced + natural ticks.
    for _ in 0..5 {
        cortex.refresh_now(Some(500)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        cortex.stalled_ticks(),
        0,
        "no stalls expected against StubAccessibility"
    );
    cortex.shutdown();
}
