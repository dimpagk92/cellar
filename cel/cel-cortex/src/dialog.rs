//! Dialog Dismisser — detects common blocking dialogs (cookies, notifications, etc.)
//!
//! Cellar's version is observation-only: it flags dismissable elements but does
//! NOT auto-click. The MCP caller (Claude) decides.
//!
//! Safety principle: cortex observes, agent acts.

use crate::model::{DialogType, DismissableDialog};
use cel_context::ScreenContext;

/// A pattern for detecting dismissable dialogs.
struct DismissPattern {
    /// Lowercase label must match this exactly (after trimming).
    label_lower: &'static str,
    dialog_type: DialogType,
    /// Higher = prefer this over other matches on the same dialog.
    priority: i32,
}

/// Ordered by priority — prefer privacy-preserving actions.
/// E.g., "Reject all" over "Accept all" for cookies.
static DISMISS_PATTERNS: &[DismissPattern] = &[
    // Cookie consent — prefer reject
    DismissPattern {
        label_lower: "reject all",
        dialog_type: DialogType::CookieConsent,
        priority: 10,
    },
    DismissPattern {
        label_lower: "reject",
        dialog_type: DialogType::CookieConsent,
        priority: 10,
    },
    DismissPattern {
        label_lower: "decline all",
        dialog_type: DialogType::CookieConsent,
        priority: 10,
    },
    DismissPattern {
        label_lower: "decline",
        dialog_type: DialogType::CookieConsent,
        priority: 10,
    },
    DismissPattern {
        label_lower: "deny all",
        dialog_type: DialogType::CookieConsent,
        priority: 9,
    },
    DismissPattern {
        label_lower: "deny",
        dialog_type: DialogType::CookieConsent,
        priority: 9,
    },
    DismissPattern {
        label_lower: "only essential",
        dialog_type: DialogType::CookieConsent,
        priority: 9,
    },
    DismissPattern {
        label_lower: "accept all cookies",
        dialog_type: DialogType::CookieConsent,
        priority: 5,
    },
    DismissPattern {
        label_lower: "accept cookies",
        dialog_type: DialogType::CookieConsent,
        priority: 5,
    },
    DismissPattern {
        label_lower: "accept all",
        dialog_type: DialogType::CookieConsent,
        priority: 5,
    },
    DismissPattern {
        label_lower: "i agree",
        dialog_type: DialogType::CookieConsent,
        priority: 4,
    },
    DismissPattern {
        label_lower: "agree",
        dialog_type: DialogType::CookieConsent,
        priority: 4,
    },
    // Notification prompts — prefer deny
    DismissPattern {
        label_lower: "no thanks",
        dialog_type: DialogType::NotificationPrompt,
        priority: 8,
    },
    DismissPattern {
        label_lower: "not now",
        dialog_type: DialogType::NotificationPrompt,
        priority: 8,
    },
    DismissPattern {
        label_lower: "later",
        dialog_type: DialogType::NotificationPrompt,
        priority: 8,
    },
    DismissPattern {
        label_lower: "maybe later",
        dialog_type: DialogType::NotificationPrompt,
        priority: 8,
    },
    DismissPattern {
        label_lower: "skip",
        dialog_type: DialogType::NotificationPrompt,
        priority: 8,
    },
    DismissPattern {
        label_lower: "deny",
        dialog_type: DialogType::NotificationPrompt,
        priority: 8,
    },
    DismissPattern {
        label_lower: "block",
        dialog_type: DialogType::NotificationPrompt,
        priority: 7,
    },
    // Generic dismiss
    DismissPattern {
        label_lower: "dismiss",
        dialog_type: DialogType::GenericDismiss,
        priority: 6,
    },
    DismissPattern {
        label_lower: "close",
        dialog_type: DialogType::GenericDismiss,
        priority: 6,
    },
    DismissPattern {
        label_lower: "got it",
        dialog_type: DialogType::GenericDismiss,
        priority: 6,
    },
    DismissPattern {
        label_lower: "ok",
        dialog_type: DialogType::GenericDismiss,
        priority: 6,
    },
    DismissPattern {
        label_lower: "okay",
        dialog_type: DialogType::GenericDismiss,
        priority: 6,
    },
    DismissPattern {
        label_lower: "\u{00d7}",
        dialog_type: DialogType::GenericDismiss,
        priority: 3,
    }, // × close button
];

/// Scan the current context for dismissable blocking dialogs.
/// Returns the best dismiss target, or None if none found.
pub fn find_dismissable_dialog(ctx: &ScreenContext) -> Option<DismissableDialog> {
    let mut best_match: Option<DismissableDialog> = None;
    let mut best_priority = -1;

    for el in &ctx.elements {
        // Only consider visible, enabled buttons/links
        if !el.state.visible || !el.state.enabled {
            continue;
        }
        if el.element_type != "button" && el.element_type != "link" {
            continue;
        }

        let label = el.label.as_deref().unwrap_or("").trim();
        if label.is_empty() {
            continue;
        }

        let label_lower = label.to_lowercase();

        for pattern in DISMISS_PATTERNS {
            if label_lower == pattern.label_lower && pattern.priority > best_priority {
                best_match = Some(DismissableDialog {
                    element_id: el.id.clone(),
                    label: label.to_string(),
                    dialog_type: pattern.dialog_type.clone(),
                    action: "click".to_string(),
                });
                best_priority = pattern.priority;
            }
        }
    }

    best_match
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_accessibility::{Bounds, ElementState};
    use cel_context::{ContentRole, ContextElement, ContextSource};

    fn make_button(id: &str, label: &str) -> ContextElement {
        ContextElement {
            id: id.to_string(),
            label: Some(label.to_string()),
            description: None,
            element_type: "button".to_string(),
            value: None,
            bounds: Some(Bounds {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            }),
            state: ElementState {
                focused: false,
                enabled: true,
                visible: true,
                selected: false,
                expanded: None,
                checked: None,
            },
            parent_id: None,
            actions: vec!["click".to_string()],
            confidence: 0.85,
            source: ContextSource::AccessibilityTree,
            content_role: ContentRole::Interactive,
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
    fn test_reject_preferred_over_accept() {
        let ctx = make_context(vec![
            make_button("1", "Accept all"),
            make_button("2", "Reject all"),
        ]);
        let dialog = find_dismissable_dialog(&ctx).unwrap();
        assert_eq!(dialog.element_id, "2");
        assert_eq!(dialog.dialog_type, DialogType::CookieConsent);
    }

    #[test]
    fn test_notification_dismiss() {
        let ctx = make_context(vec![
            make_button("1", "Allow"),
            make_button("2", "No thanks"),
        ]);
        let dialog = find_dismissable_dialog(&ctx).unwrap();
        assert_eq!(dialog.element_id, "2");
        assert_eq!(dialog.dialog_type, DialogType::NotificationPrompt);
    }

    #[test]
    fn test_generic_close() {
        let ctx = make_context(vec![make_button("1", "Close")]);
        let dialog = find_dismissable_dialog(&ctx).unwrap();
        assert_eq!(dialog.dialog_type, DialogType::GenericDismiss);
    }

    #[test]
    fn test_invisible_button_ignored() {
        let mut btn = make_button("1", "Reject all");
        btn.state.visible = false;
        let ctx = make_context(vec![btn]);
        assert!(find_dismissable_dialog(&ctx).is_none());
    }

    #[test]
    fn test_no_dialog_on_normal_page() {
        let ctx = make_context(vec![make_button("1", "Submit"), make_button("2", "Cancel")]);
        assert!(find_dismissable_dialog(&ctx).is_none());
    }

    #[test]
    fn test_x_close_button() {
        let ctx = make_context(vec![make_button("1", "\u{00d7}")]); // ×
        let dialog = find_dismissable_dialog(&ctx).unwrap();
        assert_eq!(dialog.dialog_type, DialogType::GenericDismiss);
    }
}
