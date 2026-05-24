//! Post-deserialization validation.
//!
//! Serde gets us structural validation for free (any rule that deserializes
//! into `Rule` has well-formed expression / operator / action / kind). This
//! module catches semantic issues serde can't:
//! - Watchlist names referenced by `in_watchlist` / `not_in_watchlist` that
//!   don't exist in the user's daemon (a *warning*, not an error — the user
//!   may intend to create the watchlist next).
//! - Empty `name` or `nl_original`.

use cellar_types::{
    expression::{Expression, Operator},
    rule::Rule,
};

/// Validate a parsed rule, returning a list of human-readable warnings.
///
/// Returns warnings rather than errors because the user gets a confirmation
/// step in the UI — the daemon can save with warnings and surface them, but
/// they don't block compilation.
pub fn validate_rule(rule: &Rule, known_watchlists: &[String]) -> Vec<String> {
    let mut warnings = Vec::new();

    if rule.name.trim().is_empty() {
        warnings.push("name is empty — UI may show 'Untitled rule'".into());
    }
    if rule.nl_original.trim().is_empty() {
        warnings.push("nl_original is empty — fire log will be hard to audit".into());
    }

    // Walk the expression tree collecting referenced watchlist names.
    let mut referenced: Vec<String> = Vec::new();
    collect_watchlist_refs(&rule.match_expr, &mut referenced);

    for name in referenced {
        if !known_watchlists.iter().any(|w| w == &name) {
            warnings.push(format!(
                "rule references watchlist '{name}' which doesn't exist in the daemon yet — create it before the rule will fire"
            ));
        }
    }

    warnings
}

fn collect_watchlist_refs(expr: &Expression, out: &mut Vec<String>) {
    match expr {
        Expression::All(subs) | Expression::Any(subs) => {
            for s in subs {
                collect_watchlist_refs(s, out);
            }
        }
        Expression::Not(inner) => collect_watchlist_refs(inner, out),
        Expression::Leaf(leaf) => {
            if matches!(leaf.op, Operator::InWatchlist | Operator::NotInWatchlist) {
                if let Some(name) = leaf.value.as_str() {
                    out.push(name.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cellar_types::{
        expression::Expression,
        rule::{Action, ActionType, RuleKind},
    };
    use chrono::Utc;
    use serde_json::json;

    fn rule(name: &str, expr: Expression) -> Rule {
        Rule {
            id: "draft".into(),
            name: name.into(),
            nl_original: name.into(),
            kind: RuleKind::Watcher,
            enabled: true,
            match_expr: expr,
            action: Action {
                action_type: ActionType::Webhook,
                webhook_id: Some("default".into()),
                timeout_s: None,
            },
            cooldown_seconds: 60,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn no_warnings_for_well_formed_rule() {
        let r = rule(
            "x",
            Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
        );
        let warnings = validate_rule(&r, &[]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn warns_on_empty_name() {
        let r = rule(
            "",
            Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
        );
        let warnings = validate_rule(&r, &[]);
        assert!(warnings.iter().any(|w| w.contains("name is empty")));
    }

    #[test]
    fn warns_on_unknown_watchlist() {
        let r = rule(
            "x",
            Expression::leaf(
                "data.bundle_id",
                Operator::NotInWatchlist,
                json!("missing_list"),
            ),
        );
        let warnings = validate_rule(&r, &["other_list".into()]);
        assert!(warnings.iter().any(|w| w.contains("missing_list")));
    }

    #[test]
    fn no_warning_for_known_watchlist() {
        let r = rule(
            "x",
            Expression::leaf(
                "data.bundle_id",
                Operator::NotInWatchlist,
                json!("approved_apps"),
            ),
        );
        let warnings = validate_rule(&r, &["approved_apps".into()]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn finds_watchlist_refs_inside_all() {
        let r = rule(
            "x",
            Expression::all(vec![
                Expression::leaf("kind", Operator::Eq, json!("process_started")),
                Expression::leaf(
                    "data.bundle_id",
                    Operator::NotInWatchlist,
                    json!("approved_apps"),
                ),
            ]),
        );
        let warnings = validate_rule(&r, &[]);
        assert!(warnings.iter().any(|w| w.contains("approved_apps")));
    }
}
