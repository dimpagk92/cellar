//! Dump a generic agent context and demonstrate snapshot diffing.
//!
//! Usage: cargo run -p cel-context --example dump_context

use cel_context::{
    ContentRole, ContextContribution, ContextElement, ContextMerger, ContextSource,
    ContextWatchdog, ElementState,
};

fn main() {
    let mut merger = ContextMerger::new().with_defaults("Incident Review", "Payment API");
    merger.push(ContextContribution::new(
        "trace_stream",
        vec![ContextElement {
            id: "trace:payment-api:slow-span".into(),
            label: Some("payment-api span exceeded latency budget".into()),
            description: None,
            element_type: "trace_span".into(),
            value: Some("duration_ms=1840".into()),
            bounds: None,
            state: ElementState::default(),
            parent_id: None,
            actions: Vec::new(),
            confidence: 0.0,
            source: ContextSource::External,
            content_role: ContentRole::Content,
            properties: Default::default(),
        }],
    ));

    let ctx = merger.build();
    println!("{}", serde_json::to_string_pretty(&ctx).unwrap());

    let mut watchdog = ContextWatchdog::new();
    assert!(watchdog.tick(&ctx, true).is_empty());

    let mut changed = ctx.clone();
    changed.elements.push(ContextElement {
        id: "log:payment-api:retry".into(),
        label: Some("payment-api retried card processor request".into()),
        description: None,
        element_type: "log_event".into(),
        value: Some("attempt=2".into()),
        bounds: None,
        state: ElementState::default(),
        parent_id: None,
        actions: Vec::new(),
        confidence: 0.0,
        source: ContextSource::External,
        content_role: ContentRole::Content,
        properties: Default::default(),
    });
    let events = watchdog.tick(&changed, true);
    println!("watchdog events: {events:?}");
}
