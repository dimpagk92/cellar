//! Dump the full unified context — shows what an LLM agent receives.
//!
//! Usage: cargo run -p cel-context --example dump_context
//!
//! On macOS, requires Accessibility permission for the full tree.
//! Without it, falls back to stub (minimal elements) + vision fallback if configured.

fn main() {
    let a11y = cel_accessibility::create_tree();
    let display = cel_display::create_capture();
    let network = cel_network::create_monitor();
    let mut merger = cel_context::ContextMerger::with_all(a11y, display, network);

    let ctx = merger.get_context();

    println!("=== CEL Unified Context ===");
    println!("App: {}", ctx.app);
    println!("Window: {}", ctx.window);
    println!("Elements: {}", ctx.elements.len());
    println!("Network events: {}", ctx.network_events.len());
    println!("Timestamp: {}", ctx.timestamp_ms);
    println!();

    if ctx.elements.is_empty() {
        println!("(no elements — accessibility permission may be needed)");
    } else {
        for (i, el) in ctx.elements.iter().enumerate() {
            let bounds = el
                .bounds
                .as_ref()
                .map(|b| format!("({},{} {}x{})", b.x, b.y, b.width, b.height))
                .unwrap_or_else(|| "(no bounds)".to_string());
            let label = el.label.as_deref().unwrap_or("(none)");
            println!(
                "  [{:2}] [{:.0}%] {} \"{}\" {} {:?}{}",
                i,
                el.confidence * 100.0,
                el.element_type,
                label,
                bounds,
                el.source,
                if el.state.focused { " *focused*" } else { "" }
            );
        }
    }

    println!();
    println!("=== Context References Test ===");
    if let Some(first) = ctx.elements.first() {
        let reference = first.to_reference(1920, 1080);
        println!("Created reference for element '{}': {:?}", first.id, reference);

        let resolved = cel_context::resolve_reference(&ctx, &reference);
        match resolved {
            Some(el) => println!("Resolved back to: '{}' ({})", el.id, el.element_type),
            None => println!("Failed to resolve reference!"),
        }
    }

    println!();
    println!("=== Focused Context Test ===");
    if let Some(first) = ctx.elements.first() {
        let focused = merger.get_context_focused(&first.id);
        match focused {
            Some(fc) => {
                println!(
                    "Focused on '{}': {} subtree elements, ancestor path: {:?}",
                    fc.element.id,
                    fc.subtree.len(),
                    fc.ancestor_path
                );
            }
            None => println!("Failed to get focused context"),
        }
    }

    println!();
    println!("=== Watchdog Test ===");
    let mut wd = cel_context::ContextWatchdog::new();
    let events1 = wd.tick(&ctx, true);
    println!("First tick (init): {} events", events1.len());

    let ctx2 = merger.get_context();
    let events2 = wd.tick(&ctx2, true);
    println!(
        "Second tick: {} events (0 = stable, >0 = changed)",
        events2.len()
    );
    for event in &events2 {
        println!("  Event: {:?}", event);
    }
}
