//! `DaemonCortexPerception` — the daemon-side [`cel_brief::PerceptionSnapshot`]
//! adapter over a hosted Cortex (`cellar-daemon-cortex.md` Phase D).
//!
//! Uses an unbooted Cortex (default empty mental model): no perception tick,
//! no AX / display access, so the test runs headless anywhere. The point is
//! the projection plumbing, not live perception content.

use std::sync::Arc;

use cel_brief::PerceptionSnapshot;
use cel_cortex_daemon::agent_runtime::DaemonCortexPerception;

#[tokio::test]
async fn projections_render_from_an_unbooted_cortex() {
    let cortex = Arc::new(cel_cortex::Cortex::new("perception-test".into()));
    let p = DaemonCortexPerception::new(cortex);

    let summary = p.as_screen_summary().await.unwrap();
    assert!(summary.contains("0 elements"), "summary: {summary}");
    assert!(summary.contains("App: (none)"), "summary: {summary}");

    let focus = p.as_focus_only().await.unwrap();
    assert!(focus.contains("Focused: (none)"), "focus: {focus}");

    let tree = p.as_ax_tree().await.unwrap();
    assert!(tree.contains("0 elements"), "tree: {tree}");
}
