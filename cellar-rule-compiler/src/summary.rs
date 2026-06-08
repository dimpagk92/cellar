//! Human-readable rule summarizer.
//!
//! Renders a compiled `Rule` as multi-line text the UI shows to the user
//! during the "I understood this as…" confirmation step. Deterministic;
//! no LLM call.

use cellar_types::{
    expression::{Expression, Leaf, Operator},
    rule::{Action, ActionType, Rule},
};
use serde_json::Value;

/// Render a rule as a human-readable summary block.
pub fn summarize_rule(rule: &Rule) -> String {
    let mut out = String::new();
    out.push_str("WHEN\n");
    render_expression(&rule.match_expr, &mut out, 1);
    out.push_str("THEN\n  ");
    out.push_str(&render_action(&rule.action));
    if rule.cooldown_seconds > 0 {
        out.push_str(&format!("\n  (cooldown {}s)", rule.cooldown_seconds));
    }
    out
}

fn render_expression(expr: &Expression, out: &mut String, indent: usize) {
    let pad = "  ".repeat(indent);
    match expr {
        Expression::Leaf(leaf) => {
            out.push_str(&pad);
            out.push_str(&render_leaf(leaf));
            out.push('\n');
        }
        Expression::All(subs) => match subs.len() {
            0 => {
                out.push_str(&pad);
                out.push_str("(always)\n");
            }
            1 => render_expression(&subs[0], out, indent),
            _ => {
                for (i, s) in subs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&pad);
                        out.push_str("AND\n");
                    }
                    render_expression(s, out, indent);
                }
            }
        },
        Expression::Any(subs) => match subs.len() {
            0 => {
                out.push_str(&pad);
                out.push_str("(never)\n");
            }
            1 => render_expression(&subs[0], out, indent),
            _ => {
                out.push_str(&pad);
                out.push_str("ANY OF\n");
                for s in subs {
                    render_expression(s, out, indent + 1);
                }
            }
        },
        Expression::Not(inner) => {
            out.push_str(&pad);
            out.push_str("NOT (\n");
            render_expression(inner, out, indent + 1);
            out.push_str(&pad);
            out.push_str(")\n");
        }
    }
}

fn render_leaf(leaf: &Leaf) -> String {
    let op = render_operator(leaf.op);
    let value = render_value(leaf.op, &leaf.value);
    format!("{} {} {}", leaf.field, op, value)
}

fn render_operator(op: Operator) -> &'static str {
    match op {
        Operator::Eq => "=",
        Operator::Neq => "≠",
        Operator::Gt => ">",
        Operator::Gte => "≥",
        Operator::Lt => "<",
        Operator::Lte => "≤",
        Operator::StartsWith => "starts with",
        Operator::NotStartsWith => "NOT starts with",
        Operator::EndsWith => "ends with",
        Operator::NotEndsWith => "NOT ends with",
        Operator::Contains => "contains",
        Operator::NotContains => "NOT contains",
        Operator::Regex => "matches regex",
        Operator::In => "in",
        Operator::NotIn => "NOT in",
        Operator::InWatchlist => "in watchlist",
        Operator::NotInWatchlist => "NOT in watchlist",
    }
}

fn render_value(op: Operator, v: &Value) -> String {
    // For numeric ops, render with thousands separators when the number is large
    if matches!(
        op,
        Operator::Gt | Operator::Gte | Operator::Lt | Operator::Lte
    ) {
        if let Some(n) = v.as_u64() {
            return format_thousands(n);
        }
        if let Some(n) = v.as_i64() {
            if n >= 0 {
                return format_thousands(n as u64);
            } else {
                return format!("-{}", format_thousands(n.unsigned_abs()));
            }
        }
        if let Some(f) = v.as_f64() {
            return format!("{f}");
        }
    }

    match v {
        Value::String(s) => {
            if matches!(op, Operator::InWatchlist | Operator::NotInWatchlist) {
                format!("`{s}`")
            } else {
                s.clone()
            }
        }
        Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|i| match i {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
            format!("[{}]", parts.join(", "))
        }
        other => other.to_string(),
    }
}

fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

fn render_action(action: &Action) -> String {
    match action.action_type {
        ActionType::Webhook => format!(
            "fire webhook `{}`",
            action.webhook_id.as_deref().unwrap_or("default")
        ),
        ActionType::RequireConfirmation => {
            let t = action.timeout_s.unwrap_or(300);
            format!("require confirmation (timeout {})", format_duration_s(t))
        }
        ActionType::Allow => "always allow (exception rule)".into(),
        ActionType::Veto => "veto".into(),
        ActionType::SoftBlock => "soft-block via cel_act".into(),
        ActionType::LogOnly => "log only".into(),
        ActionType::RedactMemory => "redact memory (suppress persistence)".into(),
    }
}

fn format_duration_s(s: u64) -> String {
    if s >= 60 && s.is_multiple_of(60) {
        format!("{} min", s / 60)
    } else {
        format!("{}s", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cellar_types::rule::RuleKind;
    use chrono::Utc;
    use serde_json::json;

    fn r(expr: Expression, action: Action) -> Rule {
        Rule {
            id: "draft".into(),
            name: "test".into(),
            nl_original: "x".into(),
            kind: RuleKind::Watcher,
            enabled: true,
            match_expr: expr,
            action,
            cooldown_seconds: 60,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn renders_simple_eq() {
        let rule = r(
            Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            Action {
                action_type: ActionType::Webhook,
                webhook_id: Some("default".into()),
                timeout_s: None,
            },
        );
        let s = summarize_rule(&rule);
        assert!(s.contains("kind = file_deleted"));
        assert!(s.contains("fire webhook `default`"));
        assert!(s.contains("cooldown 60s"));
    }

    #[test]
    fn renders_thousands_in_numeric_ops() {
        let rule = r(
            Expression::leaf("data.size_bytes", Operator::Gte, json!(1_073_741_824u64)),
            Action {
                action_type: ActionType::Webhook,
                webhook_id: Some("default".into()),
                timeout_s: None,
            },
        );
        let s = summarize_rule(&rule);
        assert!(s.contains("1,073,741,824"));
        assert!(s.contains("data.size_bytes ≥"));
    }

    #[test]
    fn renders_all_with_and_separators() {
        let rule = r(
            Expression::all(vec![
                Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
                Expression::leaf("data.path", Operator::StartsWith, json!("~/Documents")),
            ]),
            Action {
                action_type: ActionType::Webhook,
                webhook_id: Some("default".into()),
                timeout_s: None,
            },
        );
        let s = summarize_rule(&rule);
        assert!(s.contains("AND"));
        assert!(s.contains("data.path starts with ~/Documents"));
    }

    #[test]
    fn renders_require_confirmation_with_minutes() {
        let rule = r(
            Expression::leaf("kind", Operator::Eq, json!("url_changed")),
            Action {
                action_type: ActionType::RequireConfirmation,
                webhook_id: None,
                timeout_s: Some(300),
            },
        );
        let s = summarize_rule(&rule);
        assert!(s.contains("require confirmation (timeout 5 min)"));
    }

    #[test]
    fn renders_in_watchlist_with_backticks() {
        let rule = r(
            Expression::leaf(
                "data.bundle_id",
                Operator::NotInWatchlist,
                json!("approved_apps"),
            ),
            Action {
                action_type: ActionType::Webhook,
                webhook_id: Some("default".into()),
                timeout_s: None,
            },
        );
        let s = summarize_rule(&rule);
        assert!(s.contains("NOT in watchlist `approved_apps`"));
    }

    #[test]
    fn renders_in_array() {
        let rule = r(
            Expression::leaf(
                "data.action_type",
                Operator::In,
                json!(["fs.move", "fs.copy"]),
            ),
            Action {
                action_type: ActionType::RequireConfirmation,
                webhook_id: None,
                timeout_s: Some(45),
            },
        );
        let s = summarize_rule(&rule);
        assert!(s.contains("data.action_type in [fs.move, fs.copy]"));
        // 45 is not a clean minute multiple → renders in seconds.
        assert!(s.contains("(timeout 45s)"));
    }
}
