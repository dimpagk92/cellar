//! Unit tests for the Cortex engine.

use super::cdp::{
    build_extract_expression, build_set_value_js, cdp_value_to_string, key_to_cdp_event,
    parse_extracted, DOM_SNAPSHOT_BODY_JS, DOM_SNAPSHOT_JS,
};
use super::numbers::should_attempt_numbers_document_bootstrap;
use super::targets::normalise_url;
use super::tick::{
    ax_event_to_bridge_event, cdp_event_to_bridge_event, input_to_bridge_event,
    is_significant_event,
};
use super::*;

#[test]
fn cdp_event_mapping_page_loaded_and_ignored() {
    // Page.loadEventFired → page_loaded, carrying the CDP timestamp.
    let frame = serde_json::json!({
        "method": "Page.loadEventFired",
        "params": { "timestamp": 12345.67 }
    });
    let ev = cdp_event_to_bridge_event(&frame).expect("Page.loadEventFired maps");
    assert_eq!(ev.source, EventSource::CortexCdp);
    assert_eq!(ev.kind, EventKind::PageLoaded);
    assert_eq!(
        ev.data.get("timestamp").and_then(|v| v.as_f64()),
        Some(12345.67)
    );

    // Unmodelled CDP methods are ignored.
    let other = serde_json::json!({ "method": "Network.requestWillBeSent", "params": {} });
    assert!(cdp_event_to_bridge_event(&other).is_none());

    // A frame with no method (e.g. a stray response) is ignored.
    let resp = serde_json::json!({ "id": 7, "result": {} });
    assert!(cdp_event_to_bridge_event(&resp).is_none());
}

#[test]
fn input_mapping_emits_documented_data_keys() {
    use cel_input::{CapturedInput, MouseButton};

    // KeyDown → keyboard_input: keycode + pressed; text only when content on.
    let down = CapturedInput::KeyDown {
        keycode: 4,
        chars: Some("h".into()),
    };
    let ev = input_to_bridge_event(&down, true);
    assert_eq!(ev.source, EventSource::CortexInput);
    assert_eq!(ev.kind, EventKind::KeyboardInput);
    assert_eq!(ev.data.get("keycode").and_then(|v| v.as_u64()), Some(4));
    assert_eq!(ev.data.get("pressed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(ev.data.get("text").and_then(|v| v.as_str()), Some("h"));
    // Catalog documents keycode/pressed — the old action/key keys must be gone.
    assert!(!ev.data.contains_key("action"));
    assert!(!ev.data.contains_key("key"));

    // Content gate off → keycode kept, text withheld.
    let gated = input_to_bridge_event(&down, false);
    assert_eq!(gated.data.get("keycode").and_then(|v| v.as_u64()), Some(4));
    assert!(!gated.data.contains_key("text"));

    // KeyUp → pressed = false.
    let up = input_to_bridge_event(&CapturedInput::KeyUp { keycode: 4 }, false);
    assert_eq!(
        up.data.get("pressed").and_then(|v| v.as_bool()),
        Some(false)
    );

    // Scroll → delta_x/delta_y (the documented keys, not dx/dy).
    let scroll = input_to_bridge_event(&CapturedInput::Scroll { dx: -1, dy: 3 }, false);
    assert_eq!(scroll.kind, EventKind::PointerScroll);
    assert_eq!(
        scroll.data.get("delta_x").and_then(|v| v.as_i64()),
        Some(-1)
    );
    assert_eq!(scroll.data.get("delta_y").and_then(|v| v.as_i64()), Some(3));
    assert!(!scroll.data.contains_key("dx"));

    // Button → label + pressed + coordinates.
    let btn = input_to_bridge_event(
        &CapturedInput::MouseButton {
            button: MouseButton::Right,
            pressed: true,
            x: 1.0,
            y: 2.0,
        },
        false,
    );
    assert_eq!(btn.kind, EventKind::PointerButton);
    assert_eq!(
        btn.data.get("button").and_then(|v| v.as_str()),
        Some("right")
    );
    assert_eq!(
        btn.data.get("pressed").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn ax_event_mapping_covers_payload_and_bare_variants() {
    // App activation → app_focused with the app name in data.
    let e = ax_event_to_bridge_event(&AccessibilityEvent::AppActivated {
        app_name: Some("Safari".into()),
    });
    assert_eq!(e.source, EventSource::CortexAx);
    assert_eq!(e.kind, EventKind::AppFocused);
    assert_eq!(e.data.get("app").and_then(|v| v.as_str()), Some("Safari"));

    // Missing optional payload → the data key is omitted entirely.
    let e = ax_event_to_bridge_event(&AccessibilityEvent::AppActivated { app_name: None });
    assert_eq!(e.kind, EventKind::AppFocused);
    assert!(!e.data.contains_key("app"));

    // ValueChanged carries the (required) element_id plus optional value.
    let e = ax_event_to_bridge_event(&AccessibilityEvent::ValueChanged {
        element_id: "field-7".into(),
        new_value: Some("hello".into()),
    });
    assert_eq!(e.kind, EventKind::ValueChanged);
    assert_eq!(
        e.data.get("element_id").and_then(|v| v.as_str()),
        Some("field-7")
    );
    assert_eq!(
        e.data.get("new_value").and_then(|v| v.as_str()),
        Some("hello")
    );

    // Bare variant → mapped kind, empty data.
    let e = ax_event_to_bridge_event(&AccessibilityEvent::SheetCreated);
    assert_eq!(e.kind, EventKind::SheetOpened);
    assert!(e.data.is_empty());

    let e = ax_event_to_bridge_event(&AccessibilityEvent::WindowMinimized);
    assert_eq!(e.kind, EventKind::WindowMinimized);
}

#[test]
fn normalise_url_strips_query_fragment_trailing_slash() {
    // Same page modulo URL noise — should compare equal.
    assert_eq!(
        normalise_url("http://localhost:4567/foo.html?refresh=1"),
        normalise_url("http://localhost:4567/foo.html")
    );
    assert_eq!(
        normalise_url("http://localhost:4567/foo.html#section"),
        normalise_url("http://localhost:4567/foo.html")
    );
    assert_eq!(
        normalise_url("http://localhost:4567/foo/"),
        normalise_url("http://localhost:4567/foo")
    );
    assert_eq!(
        normalise_url("HTTP://Localhost:4567/Foo.HTML"),
        normalise_url("http://localhost:4567/foo.html")
    );
    // Different paths — distinct.
    assert_ne!(
        normalise_url("http://localhost:4567/foo.html"),
        normalise_url("http://localhost:4567/bar.html")
    );
    // Different hosts — distinct.
    assert_ne!(
        normalise_url("http://localhost:4567/foo"),
        normalise_url("http://example.com/foo")
    );
}

#[test]
fn key_to_cdp_event_handles_named_keys() {
    // The exact keys the cellar planner system prompt enumerates.
    let enter = key_to_cdp_event("Return");
    assert_eq!(enter.key, "Enter");
    assert_eq!(enter.code, "Enter");
    assert_eq!(enter.vk, 13);
    let tab = key_to_cdp_event("Tab");
    assert_eq!(tab.key, "Tab");
    assert_eq!(tab.vk, 9);
    let down = key_to_cdp_event("Down");
    assert_eq!(down.key, "ArrowDown");
    assert_eq!(down.code, "ArrowDown");
    assert_eq!(down.vk, 40);
    let esc = key_to_cdp_event("Escape");
    assert_eq!(esc.vk, 27);
    assert!(esc.text.is_none());
}

#[test]
fn key_to_cdp_event_handles_function_keys() {
    let f1 = key_to_cdp_event("F1");
    assert_eq!(f1.key, "F1");
    assert_eq!(f1.vk, 112);
    let f12 = key_to_cdp_event("F12");
    assert_eq!(f12.key, "F12");
    assert_eq!(f12.vk, 123);
    // F13+ falls through to char-text (treats "F13" as 3-char text).
    let f13 = key_to_cdp_event("F13");
    assert!(f13.text.is_some());
}

#[test]
fn key_to_cdp_event_handles_single_chars() {
    let a = key_to_cdp_event("a");
    assert_eq!(a.key, "a");
    assert_eq!(a.code, "KeyA");
    assert_eq!(a.vk, 65);
    assert_eq!(a.text.as_deref(), Some("a"));
    let d5 = key_to_cdp_event("5");
    assert_eq!(d5.code, "Digit5");
    assert_eq!(d5.vk, 53);
}

#[test]
fn key_to_cdp_event_is_case_insensitive_for_named_keys() {
    assert_eq!(key_to_cdp_event("enter").vk, 13);
    assert_eq!(key_to_cdp_event("ENTER").vk, 13);
    assert_eq!(key_to_cdp_event("EnTeR").vk, 13);
}

#[test]
fn platform_matches_handles_known_aliases() {
    // The exact match cases for each OS — these are what most
    // cellar manifests actually emit.
    let current = std::env::consts::OS;
    assert!(platform_matches(current));
    // Whitespace tolerance.
    assert!(platform_matches(&format!("  {current}  ")));
    // Case tolerance.
    assert!(platform_matches(&current.to_uppercase()));

    // Aliases for macOS.
    if current == "macos" {
        assert!(platform_matches("darwin"));
        assert!(platform_matches("mac"));
        assert!(platform_matches("osx"));
    } else {
        assert!(!platform_matches("darwin"));
        assert!(!platform_matches("mac"));
    }

    // Aliases for Windows.
    if current == "windows" {
        assert!(platform_matches("win"));
        assert!(platform_matches("win32"));
        assert!(platform_matches("win64"));
    } else {
        assert!(!platform_matches("win"));
    }
}

#[test]
fn platform_matches_rejects_other_oses() {
    // Each of these is "the wrong OS" on at least one of our
    // build targets, so the test is platform-aware: it asserts
    // every OS string that ISN'T the current one returns false.
    let candidates = ["macos", "linux", "windows", "freebsd"];
    let current = std::env::consts::OS;
    for c in candidates {
        assert_eq!(
            platform_matches(c),
            c == current,
            "platform_matches({c:?}) should be {} on {current}",
            c == current,
        );
    }
}

#[test]
fn build_extract_expression_wraps_bare_css_selector() {
    let expr = build_extract_expression("fin-streamer[data-field='price']");
    // Should wrap with querySelector + textContent + null-guard
    assert!(expr.contains("document.querySelector"));
    assert!(expr.contains("textContent"));
    // Original selector must be present, with the inner ' escaped
    // for JS string embedding.
    assert!(expr.contains(r"fin-streamer[data-field=\'price\']"));
}

#[test]
fn build_extract_expression_passes_raw_js_through() {
    let js = "(function() { return document.title; })()";
    assert_eq!(build_extract_expression(js), js);
    let arrow = "(() => document.title)()";
    assert_eq!(build_extract_expression(arrow), arrow);
}

#[test]
fn build_extract_expression_supports_contains_and_has_selectors() {
    let expr = build_extract_expression("tr:has(td:contains('EMP-0742')) td:nth-child(2)");
    assert!(expr.contains("rowMatch"));
    assert!(expr.contains("adjacentMatch"));
    assert!(expr.contains("siblingNthMatch"));
    assert!(expr.contains("containsOnlyMatch"));
    assert!(expr.contains("EMP-0742"));
}

#[test]
fn parse_extracted_float_strips_currency() {
    let parsed = parse_extracted("$108,432.50", "float").unwrap();
    // Numbers round-trip via serde_json::Number — compare as f64.
    assert_eq!(parsed.as_f64().unwrap(), 108432.50);
}

#[test]
fn parse_extracted_int_handles_negative() {
    let parsed = parse_extracted("-42", "int").unwrap();
    assert_eq!(parsed.as_i64().unwrap(), -42);
}

#[test]
fn parse_extracted_text_trims() {
    let parsed = parse_extracted("  hello  ", "text").unwrap();
    assert_eq!(parsed.as_str().unwrap(), "hello");
}

#[test]
fn parse_extracted_unknown_hint_falls_back_to_text() {
    let parsed = parse_extracted("BTC", "weirdo_format").unwrap();
    assert_eq!(parsed.as_str().unwrap(), "BTC");
}

#[test]
fn cdp_value_to_string_rejects_null_and_empty() {
    assert!(cdp_value_to_string(&serde_json::Value::Null).is_none());
    assert!(cdp_value_to_string(&serde_json::Value::String(String::new())).is_none());
}

#[test]
fn dom_snapshot_js_includes_all_dimensions() {
    // The dom_changed fingerprint catches an SPA reaction when
    // ANY field differs. Run-6 evidence (cookie-consent,
    // re-authenticate, outstanding-balance, approve-deploy)
    // showed buttons whose click dispatched OK but didn't change
    // text length or interactive count — only visibility,
    // disabled state, or aria-state changed. The snapshot needs
    // every dimension below to catch those.
    //
    // This test pins the field set so any future "simplification"
    // that drops a dimension has to consciously update the test.
    for &js in &[DOM_SNAPSHOT_BODY_JS, DOM_SNAPSHOT_JS] {
        assert!(js.contains("t:"), "text length field");
        assert!(js.contains("c:"), "total interactive count field");
        assert!(js.contains("v:"), "visible interactive count field");
        assert!(js.contains("d:"), "disabled count field");
        assert!(js.contains("s:"), "aria/dialog state hash field");
        assert!(js.contains("u:"), "url field");
        assert!(
            js.contains("offsetParent"),
            "must filter visible elements via offsetParent"
        );
        assert!(
            js.contains("aria-hidden"),
            "must include aria-hidden in state hash"
        );
        assert!(
            js.contains("aria-disabled"),
            "must include aria-disabled in disabled count"
        );
    }
}

#[test]
fn dom_snapshot_js_is_well_formed_self_invoking() {
    // The full snapshot expression must be wrapped in an IIFE so
    // CDP's Runtime.evaluate returns its value directly. The body
    // expression (DOM_SNAPSHOT_BODY_JS) is inlined into a larger
    // closure inside wait_for_effect, so it doesn't need its own
    // wrapper — but it must NOT include the wrapper either, or
    // the inlining would produce nested IIFEs.
    let body = DOM_SNAPSHOT_BODY_JS;
    let full = DOM_SNAPSHOT_JS;
    assert!(
        !body.trim_start().starts_with("(()"),
        "BODY_JS must be statement-body only (no IIFE), got start: {:?}",
        &body.trim_start().chars().take(20).collect::<String>()
    );
    assert!(
        full.trim_start().starts_with("(()"),
        "DOM_SNAPSHOT_JS must be a self-invoking IIFE, got start: {:?}",
        &full.trim_start().chars().take(20).collect::<String>()
    );
    assert!(
        full.contains("return after;"),
        "DOM_SNAPSHOT_JS must return the snapshot string"
    );
}

#[test]
fn cdp_value_to_string_returns_str() {
    let v = serde_json::Value::String("hello".into());
    assert_eq!(cdp_value_to_string(&v).unwrap(), "hello");
}

#[cfg(target_os = "macos")]
#[test]
fn numbers_bootstrap_only_triggers_for_numbers_scripting_unavailable() {
    assert!(should_attempt_numbers_document_bootstrap(
        &InputError::ScriptingUnavailable {
            app: "Numbers".into(),
            reason: "no open document".into(),
        }
    ));
    assert!(!should_attempt_numbers_document_bootstrap(
        &InputError::Failed("random".into())
    ));
    assert!(!should_attempt_numbers_document_bootstrap(
        &InputError::ScriptingUnavailable {
            app: "Pages".into(),
            reason: "no open document".into(),
        }
    ));
}

#[test]
fn test_context_fingerprint_stable() {
    let ctx = ScreenContext {
        app: "Test".into(),
        window: "Window".into(),
        elements: vec![],
        network_events: vec![],
        http_events: vec![],
        timestamp_ms: 0,
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
    assert_eq!(context_fingerprint(&ctx), context_fingerprint(&ctx));
}

#[test]
fn test_context_fingerprint_differs() {
    let ctx1 = ScreenContext {
        app: "App1".into(),
        window: "W".into(),
        elements: vec![],
        network_events: vec![],
        http_events: vec![],
        timestamp_ms: 0,
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
    let ctx2 = ScreenContext {
        app: "App2".into(),
        ..ctx1.clone()
    };
    assert_ne!(context_fingerprint(&ctx1), context_fingerprint(&ctx2));
}

#[test]
fn test_is_significant_event() {
    assert!(is_significant_event(&CelEvent::SheetCreated));
    assert!(is_significant_event(&CelEvent::LayoutChanged));
    assert!(!is_significant_event(&CelEvent::NetworkIdle));
    assert!(!is_significant_event(&CelEvent::WindowMoved));
}

#[test]
fn test_cortex_new() {
    let cortex = Cortex::new("test-1".into());
    assert_eq!(cortex.id, "test-1");
    assert!(!cortex.is_running());
}

#[tokio::test]
async fn cdp_screenshot_returns_none_when_no_client_bound() {
    // The runner relies on this short-circuit: when no CDP client is
    // wired (numbers/native-app scenarios, mock harness), the CDP
    // path must yield None so the caller falls back to the macOS
    // display capture instead of hanging on a non-existent client.
    let cortex = Cortex::new("no-cdp".into());
    assert!(!cortex.has_cdp_client());
    assert!(cortex.cdp_screenshot().await.is_none());
}

// ─── build_set_value_js: <select> handling (eval-smoke Fix) ───────

#[test]
fn set_value_js_has_dedicated_select_branch() {
    let js = build_set_value_js("select", "0", "support");
    // The select-specific branch must exist (otherwise `<select>` calls
    // silently no-op when the planner supplies a display text instead
    // of an option value).
    assert!(
        js.contains("target.tagName === 'SELECT'"),
        "expected select branch in set_value JS"
    );
    // Falls back through three lookup tiers: exact value, case-
    // insensitive value, then textContent match. All three should be
    // visible in the emitted script.
    assert!(
        js.contains("o.value === value"),
        "expected exact-value match"
    );
    assert!(
        js.contains("o.textContent"),
        "expected textContent-based fallback"
    );
    // The 'no-option' sentinel lets the runner distinguish "couldn't
    // find the option" from "couldn't find the element".
    assert!(
        js.contains("'no-option:'"),
        "expected distinct no-option error sentinel"
    );
}

#[test]
fn set_value_js_input_path_unchanged_for_non_selects() {
    // Regression guard: the select branch must not swallow the input
    // path. The original `input`-role code (native setter + input +
    // change events) must still be present for textarea / input use.
    let js = build_set_value_js("input", "0", "hello");
    assert!(js.contains("setNativeValue"));
    assert!(js.contains("HTMLTextAreaElement.prototype"));
    assert!(js.contains("new InputEvent(type, init)"));
    assert!(js.contains("dispatchValueEvent(el, 'beforeinput')"));
    assert!(js.contains("dispatchValueEvent(el, 'input')"));
    assert!(js.contains("new Event('change'"));
}

// ─── dispatch_browser_dom_action: navigate adapter selection ─────

mod browser_dom_dispatch {
    use super::*;
    use crate::adapter::{
        ActionResult, AdapterDriver, AdapterError, AdapterManifest, AdapterState,
    };
    use async_trait::async_trait;
    use cel_context::ContextElement;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct RecordingDriver {
        manifest: AdapterManifest,
        executed: Arc<AtomicBool>,
        last_params: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    }

    #[async_trait]
    impl AdapterDriver for RecordingDriver {
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }
        async fn activate(&mut self) -> Result<(), AdapterError> {
            Ok(())
        }
        async fn deactivate(&mut self) -> Result<(), AdapterError> {
            Ok(())
        }
        async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError> {
            Ok(vec![])
        }
        async fn execute(
            &self,
            _action: &str,
            params: serde_json::Value,
        ) -> Result<ActionResult, AdapterError> {
            self.executed.store(true, Ordering::SeqCst);
            *self.last_params.lock().unwrap() = Some(params.clone());
            Ok(ActionResult {
                success: true,
                error: None,
                data: Some(serde_json::json!({"adapter_received": params})),
            })
        }
        async fn probe(&self) -> bool {
            true
        }
    }

    fn browser_dom_manifest(name: &str, with_navigate: bool) -> AdapterManifest {
        let actions_json = if with_navigate {
            r#"{ "navigate": { "params": { "url": "string" }, "mutates_state": true } }"#
        } else {
            "{}"
        };
        let raw = format!(
            r#"{{
                    "name": "{name}",
                    "display_name": "{name}",
                    "app_patterns": ["(?i){name}"],
                    "platform": ["macos", "linux", "windows"],
                    "context": {{ "element_types": [], "truth_surface": "browser_dom" }},
                    "actions": {actions_json}
                }}"#,
        );
        serde_json::from_str(&raw).unwrap()
    }

    async fn activate_all(cortex: &Cortex) {
        let mut guard = cortex.adapters.write().await;
        for entry in guard.iter_mut() {
            entry.state = AdapterState::Active;
        }
    }

    #[tokio::test]
    async fn picks_first_active_browser_dom_adapter_declaring_navigate() {
        // Two browser-dom adapters racing: only one declares
        // navigate. The dispatcher must pick that one — never the
        // perception-only peer (mirrors the real browser-rs vs TS
        // peer setup at runtime).
        let mut cortex = Cortex::new("nav-test".into());
        let perception_executed = Arc::new(AtomicBool::new(false));
        let perception_params: Arc<std::sync::Mutex<Option<serde_json::Value>>> =
            Arc::new(std::sync::Mutex::new(None));
        cortex.register_adapter(Box::new(RecordingDriver {
            manifest: browser_dom_manifest("browser-rs", false),
            executed: Arc::clone(&perception_executed),
            last_params: Arc::clone(&perception_params),
        }));
        let action_executed = Arc::new(AtomicBool::new(false));
        let action_params: Arc<std::sync::Mutex<Option<serde_json::Value>>> =
            Arc::new(std::sync::Mutex::new(None));
        cortex.register_adapter(Box::new(RecordingDriver {
            manifest: browser_dom_manifest("browser", true),
            executed: Arc::clone(&action_executed),
            last_params: Arc::clone(&action_params),
        }));
        activate_all(&cortex).await;

        let result = cortex
            .dispatch_browser_dom_action(
                "navigate",
                serde_json::json!({
                    "url": "https://example.com",
                    "wait_until": "domcontentloaded",
                    "timeout_ms": 30_000,
                    "dismiss_overlays": true,
                }),
            )
            .await
            .expect("adapter should have handled navigate");

        assert!(result.success, "expected success: {result:?}");
        assert!(
            action_executed.load(Ordering::SeqCst),
            "navigate-declaring adapter should have been dispatched"
        );
        assert!(
            !perception_executed.load(Ordering::SeqCst),
            "perception-only adapter must not be invoked"
        );
        // All canonical knobs flow through to the adapter — keeps
        // the contract honest as adapters opt into richer semantics.
        let received = action_params.lock().unwrap().clone().unwrap();
        assert_eq!(received["url"], "https://example.com");
        assert_eq!(received["wait_until"], "domcontentloaded");
        assert_eq!(received["timeout_ms"], 30_000);
        assert_eq!(received["dismiss_overlays"], true);
    }

    #[tokio::test]
    async fn returns_none_when_no_adapter_handles_action() {
        // No browser-dom adapter registered → caller must fall
        // through to the in-cortex CDP fallback path. The contract
        // is "Some only when an adapter actually executed".
        let cortex = Cortex::new("nav-fallback-test".into());
        let result = cortex
            .dispatch_browser_dom_action(
                "navigate",
                serde_json::json!({"url": "https://example.com"}),
            )
            .await;
        assert!(
            result.is_none(),
            "no registered adapter — must return None so caller falls back"
        );
    }

    #[tokio::test]
    async fn skips_inactive_browser_dom_adapter() {
        // An adapter that declares navigate but is Inactive must
        // NOT be picked — otherwise we'd dispatch into a browser
        // we know is offline.
        let mut cortex = Cortex::new("nav-inactive-test".into());
        let executed = Arc::new(AtomicBool::new(false));
        let params: Arc<std::sync::Mutex<Option<serde_json::Value>>> =
            Arc::new(std::sync::Mutex::new(None));
        cortex.register_adapter(Box::new(RecordingDriver {
            manifest: browser_dom_manifest("browser", true),
            executed: Arc::clone(&executed),
            last_params: Arc::clone(&params),
        }));
        // Skip activate_all — leave the adapter in default Inactive
        // state.
        let result = cortex
            .dispatch_browser_dom_action(
                "navigate",
                serde_json::json!({"url": "https://example.com"}),
            )
            .await;
        assert!(result.is_none(), "inactive adapter must be skipped");
        assert!(
            !executed.load(Ordering::SeqCst),
            "inactive adapter must not be executed"
        );
    }
}
