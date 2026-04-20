//! Anomaly detection from events and context.
//!
//! Detects unexpected UI states: dialogs, errors, app switches, auth prompts.
//! Anomalies are deduplicated and TTL-managed in the Cortex's anomaly queue.

use crate::model::{Anomaly, AnomalyType};
use cel_context::{CelEvent, ScreenContext};

/// Detect anomalies from watchdog events.
///
/// - `SheetCreated` → dialog anomaly
/// - `AppActivated` with mismatched app_name → app_switch anomaly
pub fn detect_anomalies_from_events(events: &[CelEvent], expected_app: &str) -> Vec<Anomaly> {
    let mut anomalies = Vec::new();
    let now = now_ms();

    for event in events {
        match event {
            CelEvent::SheetCreated => {
                anomalies.push(Anomaly {
                    anomaly_type: AnomalyType::Dialog,
                    title: None,
                    description: "A dialog or sheet appeared unexpectedly".into(),
                    timestamp: now,
                    element_ids: vec![],
                });
            }
            CelEvent::AppActivated { app_name } => {
                if let Some(name) = app_name {
                    if name != expected_app {
                        anomalies.push(Anomaly {
                            anomaly_type: AnomalyType::AppSwitch,
                            title: Some(name.clone()),
                            description: format!(
                                "App switched to \"{}\" (expected \"{}\")",
                                name, expected_app
                            ),
                            timestamp: now,
                            element_ids: vec![],
                        });
                    }
                }
            }
            _ => {}
        }
    }

    anomalies
}

/// Detect anomalies from context elements.
///
/// Scans for: alert/dialog types, error/failed/exception labels, auth prompts.
pub fn detect_anomalies_from_context(context: &ScreenContext) -> Vec<Anomaly> {
    let mut anomalies = Vec::new();
    let now = now_ms();

    for el in &context.elements {
        let label = el.label.as_deref().unwrap_or("").to_lowercase();
        let el_type = el.element_type.to_lowercase();

        // Dialog/alert elements
        if el_type == "alert" || el_type == "dialog" {
            anomalies.push(Anomaly {
                anomaly_type: AnomalyType::Dialog,
                title: el.label.clone(),
                description: format!("Dialog detected: \"{}\"", el.label.as_deref().unwrap_or("unknown")),
                timestamp: now,
                element_ids: vec![el.id.clone()],
            });
        }

        // Error elements
        if label.contains("error") || label.contains("failed") || label.contains("exception") {
            anomalies.push(Anomaly {
                anomaly_type: AnomalyType::Error,
                title: el.label.clone(),
                description: format!("Error element: \"{}\"", el.label.as_deref().unwrap_or("")),
                timestamp: now,
                element_ids: vec![el.id.clone()],
            });
        }

        // Auth prompts (in dialog/sheet/window containers)
        if (label.contains("sign in") || label.contains("log in") || label.contains("authenticate"))
            && (el_type == "dialog" || el_type == "sheet" || el_type == "window")
        {
            anomalies.push(Anomaly {
                anomaly_type: AnomalyType::AuthPrompt,
                title: el.label.clone(),
                description: format!("Auth prompt: \"{}\"", el.label.as_deref().unwrap_or("")),
                timestamp: now,
                element_ids: vec![el.id.clone()],
            });
        }
    }

    anomalies
}

/// Get current time in milliseconds since epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_accessibility::{Bounds, ElementState};
    use cel_context::{ContentRole, ContextElement, ContextSource};

    fn make_element(id: &str, el_type: &str, label: Option<&str>) -> ContextElement {
        ContextElement {
            id: id.to_string(),
            label: label.map(String::from),
            description: None,
            element_type: el_type.to_string(),
            value: None,
            bounds: Some(Bounds { x: 0, y: 0, width: 100, height: 30 }),
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
            confidence: 0.85,
            source: ContextSource::AccessibilityTree,
            content_role: ContentRole::default(),
            properties: std::collections::HashMap::new(),
        }
    }

    fn make_context(elements: Vec<ContextElement>) -> ScreenContext {
        ScreenContext {
            app: "Test".into(),
            window: "Test Window".into(),
            elements,
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
        }
    }

    #[test]
    fn test_sheet_created_event() {
        let events = vec![CelEvent::SheetCreated];
        let anomalies = detect_anomalies_from_events(&events, "Finder");
        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].anomaly_type, AnomalyType::Dialog);
    }

    #[test]
    fn test_app_switch_event() {
        let events = vec![CelEvent::AppActivated {
            app_name: Some("Chrome".into()),
        }];
        let anomalies = detect_anomalies_from_events(&events, "Finder");
        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].anomaly_type, AnomalyType::AppSwitch);
        assert!(anomalies[0].description.contains("Chrome"));
    }

    #[test]
    fn test_same_app_no_anomaly() {
        let events = vec![CelEvent::AppActivated {
            app_name: Some("Finder".into()),
        }];
        let anomalies = detect_anomalies_from_events(&events, "Finder");
        assert!(anomalies.is_empty());
    }

    #[test]
    fn test_dialog_element_detected() {
        let ctx = make_context(vec![make_element("1", "dialog", Some("Confirm"))]);
        let anomalies = detect_anomalies_from_context(&ctx);
        assert!(anomalies.iter().any(|a| a.anomaly_type == AnomalyType::Dialog));
    }

    #[test]
    fn test_error_element_detected() {
        let ctx = make_context(vec![make_element("1", "text", Some("Connection error"))]);
        let anomalies = detect_anomalies_from_context(&ctx);
        assert!(anomalies.iter().any(|a| a.anomaly_type == AnomalyType::Error));
    }

    #[test]
    fn test_auth_prompt_detected() {
        let ctx = make_context(vec![make_element("1", "dialog", Some("Sign in to continue"))]);
        let anomalies = detect_anomalies_from_context(&ctx);
        assert!(anomalies.iter().any(|a| a.anomaly_type == AnomalyType::AuthPrompt));
    }

    #[test]
    fn test_normal_elements_no_anomalies() {
        let ctx = make_context(vec![
            make_element("1", "button", Some("Submit")),
            make_element("2", "text", Some("Hello world")),
        ]);
        let anomalies = detect_anomalies_from_context(&ctx);
        assert!(anomalies.is_empty());
    }

    #[test]
    fn test_non_significant_events_no_anomalies() {
        let events = vec![
            CelEvent::FocusChanged { old: None, new: Some("a".into()) },
            CelEvent::NetworkIdle,
        ];
        let anomalies = detect_anomalies_from_events(&events, "Finder");
        assert!(anomalies.is_empty());
    }
}
