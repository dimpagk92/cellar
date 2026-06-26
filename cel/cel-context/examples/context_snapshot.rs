//! Generic context snapshot — builds a ContextSnapshot from arbitrary streams.
//!
//! Usage:
//!   cargo run -p cel-context --example context_snapshot
//!   cargo run -p cel-context --example context_snapshot -- --json

use std::collections::HashMap;

use cel_context::{
    ContentRole, ContextContribution, ContextElement, ContextMerger, ContextSource, ElementState,
    StreamStatus,
};

fn main() {
    let json_output = std::env::args().any(|arg| arg == "--json");

    let mut merger = ContextMerger::new().with_defaults("Incident Review", "Checkout Flow");
    merger.push(
        ContextContribution::new(
            "metrics_stream",
            vec![
                element(
                    "metric:checkout:error_rate",
                    "metric",
                    "Checkout error rate is above threshold",
                    Some("7.2%"),
                    ContextSource::External,
                    ContentRole::Content,
                ),
                element(
                    "metric:checkout:p95_latency",
                    "metric",
                    "Checkout p95 latency is elevated",
                    Some("1840ms"),
                    ContextSource::External,
                    ContentRole::Content,
                ),
            ],
        )
        .with_app("Incident Review")
        .with_window("Checkout Flow")
        .with_stream_status(StreamStatus {
            accessibility: false,
            display: false,
            network: true,
            signals: true,
            vision: false,
            audio_capture: false,
        }),
    );
    merger.push(ContextContribution::new(
        "support_tickets",
        vec![element(
            "ticket:1842",
            "ticket",
            "Users report payment confirmation page timing out",
            Some("priority=high"),
            ContextSource::External,
            ContentRole::Content,
        )],
    ));
    merger.push(ContextContribution::new(
        "browser_dom",
        vec![element(
            "dom:button:retry-payment",
            "button",
            "Retry payment",
            None,
            ContextSource::Cdp,
            ContentRole::Interactive,
        )],
    ));
    merger.push(ContextContribution::new(
        "service_logs",
        vec![element(
            "log:payment-service:timeout",
            "log_event",
            "payment-service timed out calling card processor",
            Some("Timeout after 3000ms"),
            ContextSource::External,
            ContentRole::Content,
        )],
    ));

    let ctx = merger.build();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&ctx).unwrap());
        return;
    }

    println!("=== Agent Context Snapshot ===");
    println!("App:       {}", ctx.app);
    println!("Window:    {}", ctx.window);
    println!("Timestamp: {} ms", ctx.timestamp_ms);
    println!("Elements:  {}", ctx.elements.len());
    println!();

    for (i, elem) in ctx.elements.iter().enumerate() {
        let label = elem.label.as_deref().unwrap_or("(no label)");
        let value = elem.value.as_deref().unwrap_or("");
        println!(
            "  {:>2}. [{:.2}] {:12} {:48} {:18} {:?}",
            i + 1,
            elem.confidence,
            elem.element_type,
            label,
            value,
            elem.source
        );
    }
}

fn element(
    id: &str,
    element_type: &str,
    label: &str,
    value: Option<&str>,
    source: ContextSource,
    content_role: ContentRole,
) -> ContextElement {
    ContextElement {
        id: id.into(),
        label: Some(label.into()),
        description: None,
        element_type: element_type.into(),
        value: value.map(String::from),
        bounds: None,
        state: ElementState::default(),
        parent_id: None,
        actions: Vec::new(),
        confidence: 0.0,
        source,
        content_role,
        properties: HashMap::new(),
    }
}
