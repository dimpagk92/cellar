//! Integration tests for the generic CEL context contract.

use cel_context::{
    build_from_external, resolve_reference, Bounds, ConfidenceBehavior, ConfidenceThresholds,
    ContentRole, ContextContribution, ContextElement, ContextMerger, ContextSnapshot,
    ContextSource, ElementState, StreamStatus,
};

#[test]
fn confidence_thresholds_default() {
    let thresholds = ConfidenceThresholds::default();
    assert_eq!(
        thresholds.behavior_for(0.95),
        ConfidenceBehavior::ActImmediately
    );
    assert_eq!(thresholds.behavior_for(0.8), ConfidenceBehavior::ActAndLog);
    assert_eq!(
        thresholds.behavior_for(0.6),
        ConfidenceBehavior::ActCautiously
    );
    assert_eq!(
        thresholds.behavior_for(0.3),
        ConfidenceBehavior::PauseAndNotify
    );
}

#[test]
fn context_element_serializes() {
    let element = element("test:button:1", "button", ContextSource::NativeApi);

    let json = serde_json::to_string(&element).unwrap();
    let deserialized: ContextElement = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.id, "test:button:1");
    assert_eq!(deserialized.label.as_deref(), Some("Submit"));
    assert_eq!(deserialized.confidence, 0.0);
}

#[test]
fn screen_context_serializes() {
    let ctx = ContextSnapshot {
        app: "TestApp".into(),
        window: "Main Window".into(),
        elements: vec![
            element("native:btn:1", "button", ContextSource::NativeApi),
            element("vision:text:1", "text", ContextSource::Vision),
            element("metric:error_rate", "metric", ContextSource::External),
        ],
        network_events: vec![],
        http_events: vec![],
        timestamp_ms: 1_700_000_000_000,
        screen_width: None,
        screen_height: None,
        clipboard: None,
        window_list: vec![],
        audio: None,
        power: None,
        running_apps: vec![],
        recent_files: vec![],
        transcripts: vec![],
    };

    let json = serde_json::to_string(&ctx).unwrap();
    let back: ContextSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back.app, "TestApp");
    assert_eq!(back.elements.len(), 3);
}

#[test]
fn build_from_external_normalizes_elements() {
    let ctx = build_from_external(
        vec![element("dom:save", "textbox", ContextSource::Cdp)],
        vec![],
        "Browser".into(),
        "Settings".into(),
    );

    assert_eq!(ctx.app, "Browser");
    assert_eq!(ctx.window, "Settings");
    assert_eq!(ctx.elements[0].element_type, "input");
    assert_eq!(ctx.elements[0].actions, vec!["focus", "set_value"]);
    assert!(ctx.elements[0].confidence > 0.9);
}

#[test]
fn merger_combines_any_normalized_contributions() {
    let mut merger = ContextMerger::new().with_defaults("Fallback App", "Fallback Window");
    merger.push(
        ContextContribution::new(
            "native_adapter",
            vec![element("native:save", "button", ContextSource::NativeApi)],
        )
        .with_app("Native App")
        .with_window("Customer Form")
        .with_stream_status(StreamStatus {
            accessibility: false,
            display: false,
            network: false,
            signals: true,
            vision: false,
            audio_capture: false,
        }),
    );
    merger.push(ContextContribution::new(
        "ocr_adapter",
        vec![element("ocr:title", "text", ContextSource::Ocr)],
    ));
    merger.push(ContextContribution::new(
        "metrics_stream",
        vec![element(
            "metric:error_rate",
            "metric",
            ContextSource::External,
        )],
    ));

    let ctx = merger.build();
    assert_eq!(ctx.app, "Native App");
    assert_eq!(ctx.window, "Customer Form");
    assert_eq!(ctx.elements.len(), 3);

    let status = merger.stream_status();
    assert!(status.signals);
    assert!(!status.vision);
}

#[test]
fn references_resolve_against_snapshot() {
    let ctx = build_from_external(
        vec![element("native:save", "button", ContextSource::NativeApi)],
        vec![],
        "App".into(),
        "Window".into(),
    );
    let reference = ctx.elements[0].to_reference(1920, 1080);
    let resolved = resolve_reference(&ctx, &reference);

    assert_eq!(resolved.map(|el| el.id.as_str()), Some("native:save"));
}

fn element(id: &str, element_type: &str, source: ContextSource) -> ContextElement {
    ContextElement {
        id: id.into(),
        label: Some("Submit".into()),
        description: None,
        element_type: element_type.into(),
        value: None,
        bounds: Some(Bounds {
            x: 100,
            y: 200,
            width: 80,
            height: 30,
        }),
        state: ElementState::default(),
        parent_id: None,
        actions: vec![],
        confidence: 0.0,
        source,
        content_role: ContentRole::Interactive,
        properties: Default::default(),
    }
}
