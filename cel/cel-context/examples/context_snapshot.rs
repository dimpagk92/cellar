//! Live context snapshot — captures the current ScreenContext and prints it.
//!
//! Usage:
//!   cargo run -p cel-context --example context_snapshot
//!   cargo run -p cel-context --example context_snapshot -- --json
//!   cargo run -p cel-context --example context_snapshot -- --vision

fn main() {
    // Initialize tracing for debug output
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("cel=info".parse().unwrap()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let json_output = args.iter().any(|a| a == "--json");
    let use_vision = args.iter().any(|a| a == "--vision");

    // Build the context merger with all available streams
    let a11y = cel_accessibility::create_tree();
    let display = cel_display::create_capture();
    let network = cel_network::create_monitor();

    let mut merger = cel_context::ContextMerger::with_all(a11y, display, network);

    // Optionally attach vision provider from env vars
    if use_vision {
        match cel_vision::create_provider_from_env() {
            Ok(vision) => {
                // Need a tokio runtime for async vision calls
                let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
                merger = merger.with_vision(vision).with_runtime(rt.handle().clone());
                eprintln!("[info] Vision provider attached (will fallback if a11y insufficient)");
            }
            Err(e) => {
                eprintln!("[warn] Vision not configured: {}", e);
                eprintln!("[hint] Set CEL_LLM_PROVIDER, CEL_LLM_API_KEY env vars for vision fallback");
            }
        }
    }

    // Capture context
    let ctx = merger.get_context();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&ctx).unwrap());
    } else {
        println!("=== Screen Context Snapshot ===");
        println!("App:       {}", if ctx.app.is_empty() { "(unknown)" } else { &ctx.app });
        println!("Window:    {}", if ctx.window.is_empty() { "(unknown)" } else { &ctx.window });
        println!("Timestamp: {} ms", ctx.timestamp_ms);
        println!("Elements:  {}", ctx.elements.len());
        println!("Network:   {} events", ctx.network_events.len());
        println!();

        // Summary by source
        let a11y_count = ctx.elements.iter().filter(|e| e.source == cel_context::ContextSource::AccessibilityTree).count();
        let vision_count = ctx.elements.iter().filter(|e| e.source == cel_context::ContextSource::Vision).count();
        let native_count = ctx.elements.iter().filter(|e| e.source == cel_context::ContextSource::NativeApi).count();
        println!("Sources: {} a11y, {} vision, {} native", a11y_count, vision_count, native_count);

        // Vision fallback indication
        let actionable_count = ctx.elements.iter()
            .filter(|e| e.source == cel_context::ContextSource::AccessibilityTree)
            .filter(|e| matches!(e.element_type.as_str(),
                "button" | "input" | "link" | "checkbox" | "radio_button" |
                "combobox" | "menu_item" | "tab_item" | "slider" | "list_item" | "tree_item"))
            .count();
        if vision_count > 0 {
            println!("Vision fallback: YES ({} a11y actionable < 5 threshold)", actionable_count);
        } else {
            println!("Vision fallback: NO ({} a11y actionable >= 5 threshold)", actionable_count);
        }
        println!();

        // Print elements (truncated for readability)
        let max_display = 30;
        for (i, elem) in ctx.elements.iter().enumerate().take(max_display) {
            let label = elem.label.as_deref().unwrap_or("(no label)");
            let bounds_str = match &elem.bounds {
                Some(b) => format!("[{},{} {}x{}]", b.x, b.y, b.width, b.height),
                None => "(no bounds)".into(),
            };
            let state_str = {
                let s = &elem.state;
                let mut flags = Vec::new();
                if s.focused { flags.push("focused"); }
                if s.enabled { flags.push("enabled"); }
                if s.visible { flags.push("visible"); }
                if s.selected { flags.push("selected"); }
                match s.expanded {
                    Some(true) => flags.push("expanded"),
                    Some(false) => flags.push("collapsed"),
                    None => {}
                }
                match s.checked {
                    Some(true) => flags.push("checked"),
                    Some(false) => flags.push("unchecked"),
                    None => {}
                }
                flags.join("|")
            };
            let parent_str = match &elem.parent_id {
                Some(p) => format!(" ^{}", truncate(p, 12)),
                None => String::new(),
            };
            let desc_str = elem.description.as_deref()
                .map(|d| format!(" desc={}", truncate(d, 15)))
                .unwrap_or_default();
            let value_str = elem.value.as_deref()
                .map(|v| format!(" val={}", truncate(v, 15)))
                .unwrap_or_default();
            let actions_str = if elem.actions.is_empty() {
                String::new()
            } else {
                format!(" actions=[{}]", elem.actions.join(","))
            };
            println!(
                "  {:>3}. [{:.2}] {:12} {:20} {} {}{}{}{}{} {:?}",
                i + 1,
                elem.confidence,
                elem.element_type,
                truncate(label, 20),
                bounds_str,
                state_str,
                parent_str,
                desc_str,
                value_str,
                actions_str,
                elem.source,
            );
        }

        if ctx.elements.len() > max_display {
            println!("  ... and {} more elements", ctx.elements.len() - max_display);
        }

        if !ctx.network_events.is_empty() {
            println!();
            println!("Network connections:");
            for evt in ctx.network_events.iter().take(10) {
                let service = evt.service.as_deref().unwrap_or("?");
                let process = evt.process_name.as_deref().unwrap_or("?");
                println!("  {} {}:{} -> {}:{} [{}] ({})",
                    evt.protocol, evt.local_addr, evt.local_port,
                    evt.remote_addr, evt.remote_port, evt.state, service);
                println!("    process: {}", process);
            }
        }

        if !ctx.http_events.is_empty() {
            println!();
            println!("HTTP events:");
            for evt in ctx.http_events.iter().take(10) {
                let status = evt.status_code.map(|s| s.to_string()).unwrap_or_else(|| "-".into());
                println!("  {} {} [{}]", evt.method, evt.url, status);
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
