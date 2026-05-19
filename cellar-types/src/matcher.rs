//! The rule matcher.
//!
//! Pure function `(event, rules[]) → fired[]`. No async, no I/O, no LLM.
//! Watchlist lookups go through the `WatchlistLookup` trait so this crate
//! has no dependency on SQLite or any specific storage.

use regex::Regex;
use serde_json::Value;

use crate::event::Event;
use crate::expression::{Expression, Leaf, Operator};
use crate::rule::Rule;

/// Trait implemented by the daemon (against SQLite) and by tests
/// (against an in-memory map). Keeps the matcher pure.
pub trait WatchlistLookup {
    /// Returns true if `watchlist_name` exists and contains `item`.
    /// Unknown watchlist names should return false (not panic).
    fn contains(&self, watchlist_name: &str, item: &str) -> bool;
}

/// One rule that matched the event, plus a snapshot of why.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchResult<'a> {
    /// The rule that fired.
    pub rule: &'a Rule,
}

/// The matcher.
///
/// Stateless. Cooldown enforcement happens *outside* this layer (in the daemon,
/// against SQLite). The matcher returns every rule whose expression evaluates
/// to true; the caller decides whether to actually fire.
pub struct Matcher;

impl Matcher {
    /// Evaluate all enabled rules against an event.
    /// Returns the rules that matched, in the input order.
    pub fn evaluate<'a, W: WatchlistLookup>(
        event: &Event,
        rules: &'a [Rule],
        watchlists: &W,
    ) -> Vec<MatchResult<'a>> {
        rules
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| {
                if evaluate_expression(&r.match_expr, event, watchlists) {
                    Some(MatchResult { rule: r })
                } else {
                    None
                }
            })
            .collect()
    }
}

fn evaluate_expression<W: WatchlistLookup>(
    expr: &Expression,
    event: &Event,
    watchlists: &W,
) -> bool {
    match expr {
        Expression::All(subs) => subs
            .iter()
            .all(|e| evaluate_expression(e, event, watchlists)),
        Expression::Any(subs) => subs
            .iter()
            .any(|e| evaluate_expression(e, event, watchlists)),
        Expression::Not(inner) => !evaluate_expression(inner, event, watchlists),
        Expression::Leaf(leaf) => evaluate_leaf(leaf, event, watchlists),
    }
}

fn evaluate_leaf<W: WatchlistLookup>(leaf: &Leaf, event: &Event, watchlists: &W) -> bool {
    let lhs = match event.resolve_field(&leaf.field) {
        Some(v) => v,
        None => return false, // missing field never matches (except via NotIn / NotContains / etc — but conservatively, no)
    };

    match leaf.op {
        Operator::Eq => lhs == leaf.value,
        Operator::Neq => lhs != leaf.value,
        Operator::Gt => cmp_numeric(&lhs, &leaf.value).is_some_and(|o| o.is_gt()),
        Operator::Gte => cmp_numeric(&lhs, &leaf.value).is_some_and(|o| o.is_ge()),
        Operator::Lt => cmp_numeric(&lhs, &leaf.value).is_some_and(|o| o.is_lt()),
        Operator::Lte => cmp_numeric(&lhs, &leaf.value).is_some_and(|o| o.is_le()),
        Operator::StartsWith => {
            string_pair(&lhs, &leaf.value).is_some_and(|(a, b)| a.starts_with(b))
        }
        Operator::NotStartsWith => {
            string_pair(&lhs, &leaf.value).is_none_or(|(a, b)| !a.starts_with(b))
        }
        Operator::EndsWith => string_pair(&lhs, &leaf.value).is_some_and(|(a, b)| a.ends_with(b)),
        Operator::NotEndsWith => {
            string_pair(&lhs, &leaf.value).is_none_or(|(a, b)| !a.ends_with(b))
        }
        Operator::Contains => string_pair(&lhs, &leaf.value).is_some_and(|(a, b)| a.contains(b)),
        Operator::NotContains => string_pair(&lhs, &leaf.value).is_none_or(|(a, b)| !a.contains(b)),
        Operator::Regex => {
            let (text, pattern) = match string_pair(&lhs, &leaf.value) {
                Some(p) => p,
                None => return false,
            };
            // Compile-per-evaluation is fine for v1 — N is small.
            // The daemon may pre-compile and cache later (Phase 5 optimization).
            Regex::new(pattern).is_ok_and(|re| re.is_match(text))
        }
        Operator::In => match leaf.value.as_array() {
            Some(items) => items.iter().any(|item| item == &lhs),
            None => false,
        },
        Operator::NotIn => match leaf.value.as_array() {
            Some(items) => !items.iter().any(|item| item == &lhs),
            None => true,
        },
        Operator::InWatchlist => {
            let list_name = match leaf.value.as_str() {
                Some(s) => s,
                None => return false,
            };
            let item = match lhs.as_str() {
                Some(s) => s,
                None => return false,
            };
            watchlists.contains(list_name, item)
        }
        Operator::NotInWatchlist => {
            let list_name = match leaf.value.as_str() {
                Some(s) => s,
                None => return true,
            };
            let item = match lhs.as_str() {
                Some(s) => s,
                None => return true,
            };
            !watchlists.contains(list_name, item)
        }
    }
}

/// Coerce two JSON values to f64 and compare. Returns `None` if either side
/// can't be coerced.
fn cmp_numeric(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    let af = a
        .as_f64()
        .or_else(|| a.as_i64().map(|n| n as f64))
        .or_else(|| a.as_u64().map(|n| n as f64))?;
    let bf = b
        .as_f64()
        .or_else(|| b.as_i64().map(|n| n as f64))
        .or_else(|| b.as_u64().map(|n| n as f64))?;
    af.partial_cmp(&bf)
}

/// Extract `(&str, &str)` from two JSON values if both are strings.
fn string_pair<'a>(a: &'a Value, b: &'a Value) -> Option<(&'a str, &'a str)> {
    Some((a.as_str()?, b.as_str()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventKind, EventSource};
    use crate::rule::{Action, ActionType, RuleKind};
    use crate::watchlist::InMemoryWatchlists;
    use serde_json::json;

    fn rule(name: &str, expr: Expression) -> Rule {
        Rule {
            id: format!("rule_{}", name),
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
            cooldown_seconds: 0,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn empty_all_matches() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        let r = rule("empty_all", Expression::all(vec![]));
        let ws = InMemoryWatchlists::default();
        // Bind to a slice so the borrow checker can prove [r] outlives `fired`.
        let rs = [r];
        let fired = Matcher::evaluate(&e, &rs, &ws);
        assert_eq!(fired.len(), 1);
    }

    #[test]
    fn empty_any_does_not_match() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        let r = rule("empty_any", Expression::any(vec![]));
        let ws = InMemoryWatchlists::default();
        assert_eq!(Matcher::evaluate(&e, &[r], &ws).len(), 0);
    }

    #[test]
    fn disabled_rule_skipped() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        let mut r = rule("disabled", Expression::all(vec![]));
        r.enabled = false;
        let ws = InMemoryWatchlists::default();
        assert_eq!(Matcher::evaluate(&e, &[r], &ws).len(), 0);
    }

    #[test]
    fn numeric_gte() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
            .with_data("size_bytes", 2_000_000_000u64);
        let r = rule(
            "big",
            Expression::leaf("data.size_bytes", Operator::Gte, json!(1_073_741_824u64)),
        );
        let ws = InMemoryWatchlists::default();
        assert_eq!(Matcher::evaluate(&e, &[r], &ws).len(), 1);
    }

    #[test]
    fn starts_with() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
            .with_data("path", "~/Documents/foo.pdf");
        let r = rule(
            "in_docs",
            Expression::leaf("data.path", Operator::StartsWith, json!("~/Documents")),
        );
        let ws = InMemoryWatchlists::default();
        assert_eq!(Matcher::evaluate(&e, &[r], &ws).len(), 1);
    }

    #[test]
    fn regex_op() {
        let e = Event::now(EventSource::CortexCdp, EventKind::UrlChanged)
            .with_data("url", "https://twitter.com/somebody");
        let r = rule(
            "twitter",
            Expression::leaf(
                "data.url",
                Operator::Regex,
                json!(r"^https://(www\.)?twitter\.com/"),
            ),
        );
        let ws = InMemoryWatchlists::default();
        assert_eq!(Matcher::evaluate(&e, &[r], &ws).len(), 1);
    }

    #[test]
    fn in_watchlist_negation() {
        let e = Event::now(EventSource::Process, EventKind::ProcessStarted)
            .with_data("bundle_id", "com.example.unknown");
        let r = rule(
            "not_approved",
            Expression::leaf(
                "data.bundle_id",
                Operator::NotInWatchlist,
                json!("approved_apps"),
            ),
        );
        let mut ws = InMemoryWatchlists::default();
        ws.set(
            "approved_apps",
            ["com.apple.Safari", "com.anthropic.claude"],
        );
        // Borrow `r` via slice::from_ref so the watchlist mutation between
        // the two evaluations doesn't require cloning.
        assert_eq!(
            Matcher::evaluate(&e, std::slice::from_ref(&r), &ws).len(),
            1
        );

        // Now mark it approved → no match.
        ws.set("approved_apps", ["com.apple.Safari", "com.example.unknown"]);
        assert_eq!(
            Matcher::evaluate(&e, std::slice::from_ref(&r), &ws).len(),
            0
        );
    }

    #[test]
    fn missing_field_does_not_match() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        let r = rule(
            "needs_path",
            Expression::leaf("data.path", Operator::StartsWith, json!("/x")),
        );
        let ws = InMemoryWatchlists::default();
        assert_eq!(Matcher::evaluate(&e, &[r], &ws).len(), 0);
    }

    #[test]
    fn composed_all_and_any() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
            .with_data("path", "/etc/passwd")
            .with_data("size_bytes", 1024u64);
        let r = rule(
            "sensitive_or_big",
            Expression::all(vec![
                Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
                Expression::any(vec![
                    Expression::leaf("data.path", Operator::StartsWith, json!("/etc")),
                    Expression::leaf("data.size_bytes", Operator::Gte, json!(1_073_741_824u64)),
                ]),
            ]),
        );
        let ws = InMemoryWatchlists::default();
        assert_eq!(Matcher::evaluate(&e, &[r], &ws).len(), 1);
    }
}
