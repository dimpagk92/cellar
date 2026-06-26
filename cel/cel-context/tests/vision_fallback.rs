//! Integration tests for the ContextMerger vision fallback path,
//! stress/edge-case behavior, and adversarial inputs.
//!
//! These tests use mock implementations of AccessibilityTree, ScreenCapture,
//! and VisionProvider to exercise paths that unit tests cannot reach.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use cel_accessibility::{
    AccessibilityElement, AccessibilityError, AccessibilityTree, Bounds as A11yBounds, ElementRole,
    ElementState,
};
use cel_context::{ContextMerger, ContextSource};
use cel_display::{CaptureError, Frame, MonitorInfo, ScreenCapture, WindowInfo};
use cel_vision::{VisionBounds, VisionElement, VisionError, VisionProvider};

// ============================================================================
// Mock implementations
// ============================================================================

fn default_state() -> ElementState {
    ElementState {
        focused: false,
        enabled: true,
        visible: true,
        selected: false,
        expanded: None,
        checked: None,
    }
}

/// Accessibility tree that returns only non-actionable elements (text/window).
/// This should trigger the vision fallback (< 3 actionable elements).
struct SparseAccessibility;

impl AccessibilityTree for SparseAccessibility {
    fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError> {
        Ok(AccessibilityElement {
            id: "root".into(),
            role: ElementRole::Window,
            label: Some("Legacy App".into()),
            description: None,
            value: None,
            bounds: Some(A11yBounds {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            }),
            state: default_state(),
            parent_id: None,
            actions: vec![],
            properties: std::collections::HashMap::new(),
            children: vec![AccessibilityElement {
                id: "title".into(),
                role: ElementRole::Text,
                label: Some("Title".into()),
                description: None,
                value: None,
                bounds: Some(A11yBounds {
                    x: 10,
                    y: 10,
                    width: 200,
                    height: 30,
                }),
                state: default_state(),
                parent_id: Some("root".into()),
                actions: vec![],
                properties: std::collections::HashMap::new(),
                children: vec![],
                ..Default::default()
            }],
            ..Default::default()
        })
    }

    fn find_elements(
        &self,
        _role: Option<&ElementRole>,
        _label: Option<&str>,
    ) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
        Ok(vec![])
    }

    fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError> {
        Ok(Some(AccessibilityElement {
            id: "root".into(),
            role: ElementRole::Window,
            label: Some("Legacy App".into()),
            description: None,
            value: None,
            bounds: None,
            state: default_state(),
            parent_id: None,
            actions: vec![],
            properties: std::collections::HashMap::new(),
            children: vec![],
            ..Default::default()
        }))
    }
}

/// Accessibility tree with plenty of actionable elements (should NOT trigger vision).
struct RichAccessibility;

impl AccessibilityTree for RichAccessibility {
    fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError> {
        Ok(AccessibilityElement {
            id: "root".into(),
            role: ElementRole::Window,
            label: Some("Rich App".into()),
            description: None,
            value: None,
            bounds: Some(A11yBounds {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            }),
            state: default_state(),
            parent_id: None,
            actions: vec![],
            properties: std::collections::HashMap::new(),
            children: vec![
                AccessibilityElement {
                    id: "btn-1".into(),
                    role: ElementRole::Button,
                    label: Some("OK".into()),
                    description: None,
                    value: None,
                    bounds: Some(A11yBounds {
                        x: 100,
                        y: 100,
                        width: 80,
                        height: 30,
                    }),
                    state: default_state(),
                    parent_id: Some("root".into()),
                    actions: vec![],
                    properties: std::collections::HashMap::new(),
                    children: vec![],
                    ..Default::default()
                },
                AccessibilityElement {
                    id: "input-1".into(),
                    role: ElementRole::Input,
                    label: Some("Name".into()),
                    description: None,
                    value: Some("John".into()),
                    bounds: Some(A11yBounds {
                        x: 100,
                        y: 150,
                        width: 200,
                        height: 30,
                    }),
                    state: default_state(),
                    parent_id: Some("root".into()),
                    actions: vec![],
                    properties: std::collections::HashMap::new(),
                    children: vec![],
                    ..Default::default()
                },
                AccessibilityElement {
                    id: "link-1".into(),
                    role: ElementRole::Link,
                    label: Some("Help".into()),
                    description: None,
                    value: None,
                    bounds: Some(A11yBounds {
                        x: 100,
                        y: 200,
                        width: 60,
                        height: 20,
                    }),
                    state: default_state(),
                    parent_id: Some("root".into()),
                    actions: vec![],
                    properties: std::collections::HashMap::new(),
                    children: vec![],
                    ..Default::default()
                },
                AccessibilityElement {
                    id: "checkbox-1".into(),
                    role: ElementRole::Checkbox,
                    label: Some("Remember me".into()),
                    description: None,
                    value: None,
                    bounds: Some(A11yBounds {
                        x: 100,
                        y: 250,
                        width: 120,
                        height: 20,
                    }),
                    state: default_state(),
                    parent_id: Some("root".into()),
                    actions: vec![],
                    properties: std::collections::HashMap::new(),
                    children: vec![],
                    ..Default::default()
                },
                AccessibilityElement {
                    id: "btn-2".into(),
                    role: ElementRole::Button,
                    label: Some("Cancel".into()),
                    description: None,
                    value: None,
                    bounds: Some(A11yBounds {
                        x: 200,
                        y: 100,
                        width: 80,
                        height: 30,
                    }),
                    state: default_state(),
                    parent_id: Some("root".into()),
                    actions: vec![],
                    properties: std::collections::HashMap::new(),
                    children: vec![],
                    ..Default::default()
                },
            ],
            ..Default::default()
        })
    }

    fn find_elements(
        &self,
        _role: Option<&ElementRole>,
        _label: Option<&str>,
    ) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
        Ok(vec![])
    }

    fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError> {
        Ok(None)
    }
}

/// Accessibility tree that fails completely.
struct FailingAccessibility;

impl AccessibilityTree for FailingAccessibility {
    fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError> {
        Err(AccessibilityError::QueryFailed("D-Bus timeout".into()))
    }
    fn find_elements(
        &self,
        _role: Option<&ElementRole>,
        _label: Option<&str>,
    ) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
        Err(AccessibilityError::Unavailable)
    }
    fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError> {
        Err(AccessibilityError::Unavailable)
    }
}

/// Accessibility tree with a huge number of elements to stress-test flattening.
struct HugeAccessibility {
    depth: usize,
    breadth: usize,
}

impl HugeAccessibility {
    fn build_tree(&self, depth: usize, prefix: &str) -> AccessibilityElement {
        let children = if depth > 0 {
            (0..self.breadth)
                .map(|i| self.build_tree(depth - 1, &format!("{}-{}", prefix, i)))
                .collect()
        } else {
            vec![]
        };

        AccessibilityElement {
            id: prefix.into(),
            role: if depth == 0 {
                ElementRole::Button
            } else {
                ElementRole::Group
            },
            label: Some(format!("Node {}", prefix)),
            description: None,
            value: None,
            bounds: Some(A11yBounds {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            }),
            state: default_state(),
            parent_id: None,
            actions: vec![],
            properties: std::collections::HashMap::new(),
            children,
            ..Default::default()
        }
    }
}

impl AccessibilityTree for HugeAccessibility {
    fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError> {
        Ok(self.build_tree(self.depth, "root"))
    }
    fn find_elements(
        &self,
        _role: Option<&ElementRole>,
        _label: Option<&str>,
    ) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
        Ok(vec![])
    }
    fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError> {
        Ok(None)
    }
}

/// Minimal screen capture mock that returns a 1x1 red pixel frame.
struct MockCapture {
    capture_count: AtomicUsize,
}

impl MockCapture {
    fn new() -> Self {
        Self {
            capture_count: AtomicUsize::new(0),
        }
    }
}

impl ScreenCapture for MockCapture {
    fn init(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
    fn capture_frame(&mut self) -> Result<Frame, CaptureError> {
        self.capture_count.fetch_add(1, Ordering::SeqCst);
        Ok(Frame {
            data: vec![255, 0, 0, 255], // 1x1 red pixel RGBA
            width: 1,
            height: 1,
            timestamp_ms: 1000,
        })
    }
    fn capture_monitor(&mut self, _id: u32) -> Result<Frame, CaptureError> {
        self.capture_frame()
    }
    fn capture_window(&mut self, _id: u32) -> Result<Frame, CaptureError> {
        self.capture_frame()
    }
    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        Ok(vec![MonitorInfo {
            id: 0,
            name: "Mock".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            is_primary: true,
            scale_factor: 1.0,
        }])
    }
    fn list_windows(&self) -> Result<Vec<WindowInfo>, CaptureError> {
        Ok(vec![WindowInfo {
            id: 1,
            title: "Test Window".into(),
            app_name: "TestApp".into(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            is_minimized: false,
        }])
    }
    fn resolution(&self) -> (u32, u32) {
        (1920, 1080)
    }
}

/// Screen capture that always fails.
struct FailingCapture;

impl ScreenCapture for FailingCapture {
    fn init(&mut self) -> Result<(), CaptureError> {
        Err(CaptureError::Unavailable)
    }
    fn capture_frame(&mut self) -> Result<Frame, CaptureError> {
        Err(CaptureError::CaptureFailed("Display server crashed".into()))
    }
    fn capture_monitor(&mut self, _id: u32) -> Result<Frame, CaptureError> {
        Err(CaptureError::Unavailable)
    }
    fn capture_window(&mut self, _id: u32) -> Result<Frame, CaptureError> {
        Err(CaptureError::Unavailable)
    }
    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        Err(CaptureError::Unavailable)
    }
    fn list_windows(&self) -> Result<Vec<WindowInfo>, CaptureError> {
        Err(CaptureError::Unavailable)
    }
    fn resolution(&self) -> (u32, u32) {
        (0, 0)
    }
}

/// Mock vision provider that returns deterministic elements.
struct MockVision {
    call_count: Arc<AtomicUsize>,
    elements: Vec<VisionElement>,
}

impl MockVision {
    fn new(elements: Vec<VisionElement>) -> Self {
        Self {
            call_count: Arc::new(AtomicUsize::new(0)),
            elements,
        }
    }

    fn call_count(&self) -> Arc<AtomicUsize> {
        self.call_count.clone()
    }
}

#[async_trait]
impl VisionProvider for MockVision {
    async fn analyze(
        &self,
        _frame: &Frame,
        _prompt: &str,
        _detail: Option<&str>,
    ) -> Result<Vec<VisionElement>, VisionError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(self.elements.clone())
    }
    fn name(&self) -> &str {
        "mock"
    }
}

/// Vision provider that always fails.
struct FailingVision;

#[async_trait]
impl VisionProvider for FailingVision {
    async fn analyze(
        &self,
        _frame: &Frame,
        _prompt: &str,
        _detail: Option<&str>,
    ) -> Result<Vec<VisionElement>, VisionError> {
        Err(VisionError::ApiFailed("API rate limited".into()))
    }
    fn name(&self) -> &str {
        "failing"
    }
}

/// Vision provider that returns empty results.
struct EmptyVision;

#[async_trait]
impl VisionProvider for EmptyVision {
    async fn analyze(
        &self,
        _frame: &Frame,
        _prompt: &str,
        _detail: Option<&str>,
    ) -> Result<Vec<VisionElement>, VisionError> {
        Ok(vec![])
    }
    fn name(&self) -> &str {
        "empty"
    }
}

/// Vision provider that returns garbage overlapping elements.
struct GarbageVision;

#[async_trait]
impl VisionProvider for GarbageVision {
    async fn analyze(
        &self,
        _frame: &Frame,
        _prompt: &str,
        _detail: Option<&str>,
    ) -> Result<Vec<VisionElement>, VisionError> {
        // Return 100 elements all overlapping at the same position
        Ok((0..100)
            .map(|i| VisionElement {
                label: format!("garbage-{}", i),
                element_type: "button".into(),
                bounds: Some(VisionBounds {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 30,
                }),
                confidence: 0.5 + (i as f64 * 0.001),
            })
            .collect())
    }
    fn name(&self) -> &str {
        "garbage"
    }
}

// ============================================================================
// Helper
// ============================================================================

fn make_vision_elements() -> Vec<VisionElement> {
    vec![
        VisionElement {
            label: "Submit".into(),
            element_type: "button".into(),
            bounds: Some(VisionBounds {
                x: 300,
                y: 400,
                width: 100,
                height: 35,
            }),
            confidence: 0.82,
        },
        VisionElement {
            label: "Cancel".into(),
            element_type: "button".into(),
            bounds: Some(VisionBounds {
                x: 420,
                y: 400,
                width: 100,
                height: 35,
            }),
            confidence: 0.78,
        },
        VisionElement {
            label: "Email".into(),
            element_type: "input".into(),
            bounds: Some(VisionBounds {
                x: 200,
                y: 300,
                width: 300,
                height: 30,
            }),
            confidence: 0.75,
        },
    ]
}

// ============================================================================
// Vision Fallback Tests
// ============================================================================

#[test]
fn test_vision_fallback_triggers_on_sparse_a11y() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let vision = MockVision::new(make_vision_elements());
    let call_count = vision.call_count();

    let mut merger =
        ContextMerger::with_display(Box::new(SparseAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(vision))
            .with_runtime(rt.handle().clone());

    let ctx = merger.get_context();

    // Vision should have been called (sparse a11y = 0 actionable elements)
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    // Should have a11y elements + vision elements
    let vision_elements: Vec<_> = ctx
        .elements
        .iter()
        .filter(|e| e.source == ContextSource::Vision)
        .collect();
    assert!(
        vision_elements.len() >= 2,
        "Expected at least 2 vision elements, got {}",
        vision_elements.len()
    );

    // Vision elements should have correct IDs and types
    let submit = vision_elements
        .iter()
        .find(|e| e.label.as_deref() == Some("Submit"));
    assert!(submit.is_some(), "Should find Submit button from vision");
    assert_eq!(submit.unwrap().element_type, "button");
    assert_eq!(submit.unwrap().source, ContextSource::Vision);
}

#[test]
fn test_vision_fallback_does_not_trigger_on_rich_a11y() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let vision = MockVision::new(make_vision_elements());
    let call_count = vision.call_count();

    let mut merger =
        ContextMerger::with_display(Box::new(RichAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(vision))
            .with_runtime(rt.handle().clone());

    let ctx = merger.get_context();

    // Vision should NOT have been called (rich a11y = 3+ actionable elements)
    assert_eq!(call_count.load(Ordering::SeqCst), 0);

    // All elements should be from accessibility
    assert!(ctx
        .elements
        .iter()
        .all(|e| e.source == ContextSource::AccessibilityTree));
}

#[test]
fn test_vision_fallback_graceful_on_capture_failure() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut merger =
        ContextMerger::with_display(Box::new(SparseAccessibility), Box::new(FailingCapture))
            .with_vision(Box::new(MockVision::new(make_vision_elements())))
            .with_runtime(rt.handle().clone());

    // Should not panic — gracefully falls back to a11y-only
    let ctx = merger.get_context();
    assert!(!ctx.elements.is_empty());
    assert!(ctx
        .elements
        .iter()
        .all(|e| e.source == ContextSource::AccessibilityTree));
}

#[test]
fn test_vision_fallback_graceful_on_vision_api_failure() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut merger =
        ContextMerger::with_display(Box::new(SparseAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(FailingVision))
            .with_runtime(rt.handle().clone());

    let ctx = merger.get_context();
    // Should still have a11y elements, no vision elements
    assert!(!ctx.elements.is_empty());
    assert!(ctx
        .elements
        .iter()
        .all(|e| e.source == ContextSource::AccessibilityTree));
}

#[test]
fn test_vision_fallback_empty_results_produces_no_vision_elements() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut merger =
        ContextMerger::with_display(Box::new(SparseAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(EmptyVision))
            .with_runtime(rt.handle().clone());

    let ctx = merger.get_context();
    assert!(ctx
        .elements
        .iter()
        .all(|e| e.source == ContextSource::AccessibilityTree));
}

#[test]
fn test_vision_fallback_without_runtime_does_nothing() {
    // Outside a tokio context, Handle::try_current() returns None,
    // so the vision fallback path has no runtime and should be skipped.
    // with_display() calls try_current() in the constructor.
    let mut merger =
        ContextMerger::with_display(Box::new(SparseAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(MockVision::new(make_vision_elements())));
    // No with_runtime() call — runtime is whatever try_current() got (None outside tokio).

    // Should not panic — gracefully falls back to a11y-only
    let ctx = merger.get_context();
    assert!(!ctx.elements.is_empty());
}

#[test]
fn test_vision_supplements_overlapping_a11y_element() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // Vision returns element overlapping with the a11y title bounds (10,10 200x30)
    let overlapping_vision = vec![VisionElement {
        label: "Title (vision)".into(),
        element_type: "text".into(),
        bounds: Some(VisionBounds {
            x: 10,
            y: 10,
            width: 200,
            height: 30,
        }), // Same as a11y title
        confidence: 0.7,
    }];

    let mut merger =
        ContextMerger::with_display(Box::new(SparseAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(MockVision::new(overlapping_vision)))
            .with_runtime(rt.handle().clone());

    let ctx = merger.get_context();

    // Overlapping vision element should NOT appear as a separate element
    let vision_count = ctx
        .elements
        .iter()
        .filter(|e| e.source == ContextSource::Vision)
        .count();
    assert_eq!(
        vision_count, 0,
        "Overlapping vision element should be merged into a11y, not added separately"
    );

    // The a11y element should have gotten a confidence boost from cross-source confirmation
    let title = ctx.elements.iter().find(|e| e.id == "title").unwrap();
    assert_eq!(title.source, ContextSource::AccessibilityTree);
    // Should be boosted: original confidence + 0.05
    assert!(
        title.confidence > 0.70,
        "Title confidence should be boosted by cross-source confirmation"
    );
}

#[test]
fn test_vision_upgrades_bounds_when_more_precise() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // Vision returns a smaller, more precise bounding box for the same region
    // A11y title is at (10,10 200x30) = 6000px². Vision sees (15,12 180x26) = 4680px².
    let precise_vision = vec![VisionElement {
        label: "Title (precise)".into(),
        element_type: "text".into(),
        bounds: Some(VisionBounds {
            x: 15,
            y: 12,
            width: 180,
            height: 26,
        }),
        confidence: 0.7,
    }];

    let mut merger =
        ContextMerger::with_display(Box::new(SparseAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(MockVision::new(precise_vision)))
            .with_runtime(rt.handle().clone());

    let ctx = merger.get_context();

    let title = ctx.elements.iter().find(|e| e.id == "title").unwrap();
    // Bounds should have been upgraded to the more precise vision bounds
    let b = title.bounds.as_ref().unwrap();
    assert_eq!(b.x, 15, "Should have vision's more precise x");
    assert_eq!(b.y, 12, "Should have vision's more precise y");
    assert_eq!(b.width, 180, "Should have vision's more precise width");
    assert_eq!(b.height, 26, "Should have vision's more precise height");
}

#[test]
fn test_vision_adds_non_overlapping_elements() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // Vision returns element that doesn't overlap any a11y element
    let new_vision = vec![VisionElement {
        label: "Hidden Button".into(),
        element_type: "button".into(),
        bounds: Some(VisionBounds {
            x: 500,
            y: 500,
            width: 100,
            height: 35,
        }),
        confidence: 0.8,
    }];

    let mut merger =
        ContextMerger::with_display(Box::new(SparseAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(MockVision::new(new_vision)))
            .with_runtime(rt.handle().clone());

    let ctx = merger.get_context();

    // Non-overlapping vision element should be added
    let vision_elems: Vec<_> = ctx
        .elements
        .iter()
        .filter(|e| e.source == ContextSource::Vision)
        .collect();
    assert_eq!(
        vision_elems.len(),
        1,
        "Non-overlapping vision element should be added"
    );
    assert_eq!(vision_elems[0].label.as_deref(), Some("Hidden Button"));
}

// ============================================================================
// Foreground Detection Tests
// ============================================================================

#[test]
fn test_foreground_from_accessibility_focused_element() {
    let mut merger = ContextMerger::new(Box::new(SparseAccessibility));
    let ctx = merger.get_context();

    // SparseAccessibility.focused_element returns "Legacy App"
    assert_eq!(ctx.app, "Legacy App");
    assert_eq!(ctx.window, "Legacy App");
}

#[test]
fn test_foreground_falls_back_to_display_window_list() {
    // RichAccessibility returns None for focused_element.
    // Without signals wired, detect_foreground falls back to display.list_windows()
    let mut merger =
        ContextMerger::with_display(Box::new(RichAccessibility), Box::new(MockCapture::new()));
    let ctx = merger.get_context();

    // detect_foreground_from_a11y gets window title from tree root, and app name
    // from display layer's window list (MockCapture returns "TestApp")
    assert_eq!(ctx.app, "TestApp");
    assert_eq!(ctx.window, "Rich App");
}

#[test]
fn test_foreground_empty_when_all_fail() {
    let mut merger =
        ContextMerger::with_display(Box::new(FailingAccessibility), Box::new(FailingCapture));
    let ctx = merger.get_context();

    assert_eq!(ctx.app, "");
    assert_eq!(ctx.window, "");
}

// ============================================================================
// Stress Tests
// ============================================================================

#[test]
fn test_huge_tree_flattening_performance() {
    // 5 levels deep, 5 children each = 5^0 + 5^1 + 5^2 + 5^3 + 5^4 + 5^5 = 3906 elements
    let huge = HugeAccessibility {
        depth: 5,
        breadth: 5,
    };
    let mut merger = ContextMerger::new(Box::new(huge));

    let start = std::time::Instant::now();
    let ctx = merger.get_context();
    let elapsed = start.elapsed();

    assert!(
        ctx.elements.len() > 1000,
        "Should have thousands of elements"
    );
    let budget_ms = if cfg!(debug_assertions) { 5000 } else { 1000 };
    assert!(
        elapsed.as_millis() < budget_ms,
        "Flattening {} elements took {}ms (budget {}ms in {} mode)",
        ctx.elements.len(),
        elapsed.as_millis(),
        budget_ms,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    // All elements should have valid data
    for elem in &ctx.elements {
        assert!(!elem.id.is_empty());
        assert!(!elem.element_type.is_empty());
        assert!(elem.confidence > 0.0);
    }
}

#[test]
fn test_deep_tree_does_not_stack_overflow() {
    // Very deep but narrow tree (300 levels, 1 child each)
    struct DeepAccessibility;
    impl AccessibilityTree for DeepAccessibility {
        fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError> {
            let mut current = AccessibilityElement {
                id: "leaf".into(),
                role: ElementRole::Button,
                label: Some("Deep Button".into()),
                description: None,
                value: None,
                bounds: None,
                state: ElementState {
                    focused: false,
                    enabled: true,
                    visible: true,
                    selected: false,
                    expanded: None,
                    checked: None,
                },
                parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                children: vec![],
                ..Default::default()
            };
            for i in 0..300 {
                current = AccessibilityElement {
                    id: format!("level-{}", i),
                    role: ElementRole::Group,
                    label: None,
                    description: None,
                    value: None,
                    bounds: None,
                    state: ElementState {
                        focused: false,
                        enabled: true,
                        visible: true,
                        selected: false,
                        expanded: None,
                        checked: None,
                    },
                    parent_id: None,
                    actions: vec![],
                    properties: std::collections::HashMap::new(),
                    children: vec![current],
                    ..Default::default()
                };
            }
            Ok(current)
        }
        fn find_elements(
            &self,
            _: Option<&ElementRole>,
            _: Option<&str>,
        ) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
            Ok(vec![])
        }
        fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError> {
            Ok(None)
        }
    }

    let mut merger = ContextMerger::new(Box::new(DeepAccessibility));
    let ctx = merger.get_context();

    // Should have processed the deep tree without stack overflow.
    // Noise filter may remove unlabeled leaf groups, but the leaf button must survive.
    assert!(
        !ctx.elements.is_empty(),
        "Deep tree should produce at least the leaf element"
    );

    // The leaf button should be present
    let leaf = ctx.elements.iter().find(|e| e.element_type == "button");
    assert!(leaf.is_some());
    assert_eq!(leaf.unwrap().element_type, "button");
}

#[test]
fn test_empty_tree_produces_valid_context() {
    struct EmptyA11y;
    impl AccessibilityTree for EmptyA11y {
        fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError> {
            Ok(AccessibilityElement {
                id: "root".into(),
                role: ElementRole::Window,
                label: None,
                description: None,
                value: None,
                bounds: None,
                state: ElementState {
                    focused: false,
                    enabled: true,
                    visible: true,
                    selected: false,
                    expanded: None,
                    checked: None,
                },
                parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                children: vec![],
                ..Default::default()
            })
        }
        fn find_elements(
            &self,
            _: Option<&ElementRole>,
            _: Option<&str>,
        ) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
            Ok(vec![])
        }
        fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError> {
            Ok(None)
        }
    }

    let mut merger = ContextMerger::new(Box::new(EmptyA11y));
    let ctx = merger.get_context();

    // Should have exactly 1 element (the root window)
    assert_eq!(ctx.elements.len(), 1);
    assert_eq!(ctx.elements[0].element_type, "window");
    assert!(ctx.timestamp_ms > 0);
}

// ============================================================================
// Adversarial Vision Tests
// ============================================================================

#[test]
fn test_garbage_vision_output_deduplicated() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // Vision returns 100 overlapping elements at (0,0 100x30)
    let mut merger =
        ContextMerger::with_display(Box::new(SparseAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(GarbageVision))
            .with_runtime(rt.handle().clone());

    let ctx = merger.get_context();

    // Most garbage elements should be deduplicated (they all overlap each other)
    let vision_count = ctx
        .elements
        .iter()
        .filter(|e| e.source == ContextSource::Vision)
        .count();

    // Only the first non-dominated element should survive; subsequent ones overlap with it
    // The exact count depends on whether the first vision element overlaps with a11y elements,
    // but it should be far fewer than 100
    assert!(
        vision_count < 10,
        "Expected deduplication to reduce 100 → <10 elements, got {}",
        vision_count
    );
}

#[test]
fn test_a11y_failure_still_produces_context_with_vision_fallback() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut merger =
        ContextMerger::with_display(Box::new(FailingAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(MockVision::new(make_vision_elements())))
            .with_runtime(rt.handle().clone());

    let ctx = merger.get_context();

    // A11y failed, but vision should have kicked in
    let vision_count = ctx
        .elements
        .iter()
        .filter(|e| e.source == ContextSource::Vision)
        .count();
    assert!(
        vision_count >= 2,
        "Vision should provide elements when a11y fails entirely, got {}",
        vision_count
    );
}

// ============================================================================
// Repeated get_context Calls
// ============================================================================

#[test]
fn test_repeated_calls_are_consistent() {
    let mut merger = ContextMerger::new(Box::new(RichAccessibility));

    let ctx1 = merger.get_context();
    let ctx2 = merger.get_context();

    assert_eq!(ctx1.elements.len(), ctx2.elements.len());
    for (a, b) in ctx1.elements.iter().zip(ctx2.elements.iter()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.element_type, b.element_type);
        assert_eq!(a.confidence, b.confidence);
    }
}

#[test]
fn test_multiple_get_context_calls_stable() {
    // Use a simple merger (no network) and verify repeated calls are stable
    let mut merger = ContextMerger::new(Box::new(RichAccessibility));

    let ctx1 = merger.get_context();
    let ctx2 = merger.get_context();
    let ctx3 = merger.get_context();

    // All calls should produce the same number of elements
    assert_eq!(ctx1.elements.len(), ctx2.elements.len());
    assert_eq!(ctx2.elements.len(), ctx3.elements.len());
}

// ============================================================================
// Context Sorting
// ============================================================================

#[test]
fn test_elements_always_sorted_by_confidence_desc() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut merger =
        ContextMerger::with_display(Box::new(SparseAccessibility), Box::new(MockCapture::new()))
            .with_vision(Box::new(MockVision::new(make_vision_elements())))
            .with_runtime(rt.handle().clone());

    let ctx = merger.get_context();

    // Verify descending confidence order
    for i in 0..ctx.elements.len().saturating_sub(1) {
        assert!(
            ctx.elements[i].confidence >= ctx.elements[i + 1].confidence,
            "Element {} (conf={}) should be >= element {} (conf={})",
            i,
            ctx.elements[i].confidence,
            i + 1,
            ctx.elements[i + 1].confidence
        );
    }
}
