//! Live E2E smoke test — validates the full CEL pipeline on a real desktop.
//!
//! This test requires a running desktop environment with AT-SPI2.
//! It is NOT meant for CI — run manually:
//!
//!   cargo test -p cel-context --test live_e2e -- --ignored
//!
//! The test verifies:
//! 1. AT-SPI2 connects and returns a tree
//! 2. Context merger produces valid ScreenContext
//! 3. Elements have valid confidence scores, IDs, and state
//! 4. Network monitor initializes without error

#[test]
#[ignore] // Requires a running desktop with AT-SPI2
fn live_desktop_produces_valid_context() {
    let a11y = cel_accessibility::create_tree();
    let display = cel_display::create_capture();
    let network = cel_network::create_monitor();

    let mut merger = cel_context::ContextMerger::with_all(a11y, display, network);
    let ctx = merger.get_context();

    // Basic sanity checks
    assert!(
        !ctx.app.is_empty() || !ctx.elements.is_empty(),
        "Should detect either a foreground app or at least one element"
    );
    assert!(ctx.timestamp_ms > 0, "Timestamp should be set");
    assert!(
        ctx.timestamp_ms > 1_577_836_800_000,
        "Timestamp should be after 2020-01-01"
    );

    // Verify element quality
    for elem in &ctx.elements {
        assert!(
            elem.confidence >= 0.0 && elem.confidence <= 1.0,
            "Element {} has invalid confidence: {}",
            elem.id,
            elem.confidence
        );
        assert!(!elem.id.is_empty(), "Element ID should not be empty");
        assert!(
            !elem.element_type.is_empty(),
            "Element {} has empty type",
            elem.id
        );

        // State is now non-optional — verify it's populated
        // (at minimum, state struct exists even if all fields are false)
        let _state = &elem.state; // compiles — state is not Option

        // If bounds exist, they should be non-negative dimensions
        if let Some(ref b) = elem.bounds {
            assert!(b.width > 0 || b.height > 0, "Element {} has zero-area bounds", elem.id);
        }
    }

    // Elements should be sorted by confidence (highest first)
    for i in 0..ctx.elements.len().saturating_sub(1) {
        assert!(
            ctx.elements[i].confidence >= ctx.elements[i + 1].confidence,
            "Elements should be sorted by confidence: {} ({}) > {} ({})",
            ctx.elements[i].id,
            ctx.elements[i].confidence,
            ctx.elements[i + 1].id,
            ctx.elements[i + 1].confidence,
        );
    }

    // Context should serialize to JSON without error
    let json = serde_json::to_string(&ctx).expect("ScreenContext should serialize to JSON");
    assert!(!json.is_empty());

    eprintln!(
        "Live E2E: app={:?} window={:?} elements={} network_events={}",
        ctx.app,
        ctx.window,
        ctx.elements.len(),
        ctx.network_events.len()
    );
}
