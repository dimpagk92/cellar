//! Decision — what the gateway will do after evaluating the matcher.

use cellar_types::{ActionType, MatchResult, Rule};
use serde::{Deserialize, Serialize};

/// Snapshot of a fired rule. Carries enough context to write to the
/// fired-log, the memory layer, and (where applicable) construct a
/// [`crate::ConfirmationRequest`] without retaining a borrow into the
/// matcher's result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FiredRuleSnapshot {
    /// The rule's ID.
    pub rule_id: String,
    /// The rule's display name.
    pub rule_name: String,
    /// The rule's NL original.
    pub rule_nl_original: String,
    /// The rule's action.
    pub action_type: ActionType,
    /// The configured `timeout_s` from the action (relevant only for
    /// `require_confirmation`). `None` means use the gateway default.
    pub timeout_s: Option<u64>,
    /// The webhook ID configured on the rule (relevant only for `webhook`).
    pub webhook_id: Option<String>,
}

impl FiredRuleSnapshot {
    /// Capture a fired rule's relevant fields, dropping the borrow.
    pub fn capture(matched: &MatchResult<'_>) -> Self {
        let r: &Rule = matched.rule;
        Self {
            rule_id: r.id.clone(),
            rule_name: r.name.clone(),
            rule_nl_original: r.nl_original.clone(),
            action_type: r.action.action_type,
            timeout_s: r.action.timeout_s,
            webhook_id: r.action.webhook_id.clone(),
        }
    }
}

/// The gateway's decision after running the matcher.
///
/// Constructed by [`Decision::from_matches`]; consumed by
/// [`crate::Gateway::intercept`].
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// No matching rule, or only `log_only` / `webhook` rules matched. The
    /// action proceeds; carried rules drive logging and webhook intents.
    Allow {
        /// Rules that matched without intercepting (audit and webhook).
        passthrough_fires: Vec<FiredRuleSnapshot>,
    },
    /// A `require_confirmation` rule matched. The gateway pauses and asks
    /// the broker.
    RequireConfirmation {
        /// The first `require_confirmation` rule that matched. Ordering is
        /// the order rules were supplied to the matcher.
        rule: FiredRuleSnapshot,
        /// Any other rules (audit/webhook) that also matched and should be
        /// recorded alongside.
        passthrough_fires: Vec<FiredRuleSnapshot>,
    },
    /// A `veto` or `soft_block` rule matched. The action is rejected.
    /// `soft_block` differs from `veto` only by a flag the daemon's
    /// countermeasure layer reads.
    Veto {
        /// The vetoing rule.
        rule: FiredRuleSnapshot,
        /// True iff the matched rule was `soft_block` rather than `veto`.
        soft_block: bool,
        /// Any other audit/webhook rules that also matched.
        passthrough_fires: Vec<FiredRuleSnapshot>,
    },
}

impl Decision {
    /// Reduce a list of matcher results into one [`Decision`].
    ///
    /// Precedence (highest wins): `allow` > `veto` > `soft_block` >
    /// `require_confirmation` > `webhook` / `log_only`.
    ///
    /// `allow` is the explicit exception variant — an exception rule created
    /// by the "Always allow this kind" confirmation path. It short-circuits
    /// all blocking rules: if any exception rule fires, the action is allowed
    /// immediately and the other matched rules ride along as passthrough audit.
    ///
    /// When two rules of the same precedence match, the first one wins
    /// (matcher order). Lower-precedence rules of the same call still get
    /// logged via `passthrough_fires` so audit records them.
    pub fn from_matches(matches: &[MatchResult<'_>]) -> Self {
        // Bucket fired rules by action type. Precedence:
        // allow > veto > soft_block > require_confirmation > others.
        let mut allow: Option<FiredRuleSnapshot> = None;
        let mut veto: Option<FiredRuleSnapshot> = None;
        let mut soft_block: Option<FiredRuleSnapshot> = None;
        let mut confirm: Option<FiredRuleSnapshot> = None;
        let mut others: Vec<FiredRuleSnapshot> = Vec::new();

        for m in matches {
            let snap = FiredRuleSnapshot::capture(m);
            match snap.action_type {
                ActionType::Allow if allow.is_none() => allow = Some(snap),
                ActionType::Veto if veto.is_none() => veto = Some(snap),
                ActionType::SoftBlock if soft_block.is_none() => soft_block = Some(snap),
                ActionType::RequireConfirmation if confirm.is_none() => confirm = Some(snap),
                _ => others.push(snap),
            }
        }

        // Explicit exception overrides all blocking rules. All other matched
        // rules ride along as passthrough fires for audit.
        if let Some(allow_rule) = allow {
            let mut passthrough = others;
            if let Some(v) = veto {
                passthrough.push(v);
            }
            if let Some(sb) = soft_block {
                passthrough.push(sb);
            }
            if let Some(c) = confirm {
                passthrough.push(c);
            }
            passthrough.insert(0, allow_rule);
            return Decision::Allow {
                passthrough_fires: passthrough,
            };
        }

        if let Some(rule) = veto {
            // soft_block + confirm + others ride along as passthrough audit.
            let mut passthrough = others;
            if let Some(sb) = soft_block {
                passthrough.push(sb);
            }
            if let Some(c) = confirm {
                passthrough.push(c);
            }
            return Decision::Veto {
                rule,
                soft_block: false,
                passthrough_fires: passthrough,
            };
        }
        if let Some(rule) = soft_block {
            let mut passthrough = others;
            if let Some(c) = confirm {
                passthrough.push(c);
            }
            return Decision::Veto {
                rule,
                soft_block: true,
                passthrough_fires: passthrough,
            };
        }
        if let Some(rule) = confirm {
            return Decision::RequireConfirmation {
                rule,
                passthrough_fires: others,
            };
        }

        Decision::Allow {
            passthrough_fires: others,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cellar_types::{Action, Expression, Operator, Rule, RuleKind};
    use chrono::Utc;
    use serde_json::json;

    fn rule_with(id: &str, action_type: ActionType) -> Rule {
        Rule {
            id: id.into(),
            name: id.into(),
            nl_original: id.into(),
            kind: RuleKind::Guard,
            enabled: true,
            match_expr: Expression::leaf("kind", Operator::Eq, json!("agent_action_attempted")),
            action: Action {
                action_type,
                webhook_id: None,
                timeout_s: None,
            },
            cooldown_seconds: 0,
            created_at: Utc::now(),
        }
    }

    fn make_match(rule: &Rule) -> MatchResult<'_> {
        MatchResult { rule }
    }

    #[test]
    fn no_matches_allows() {
        let d = Decision::from_matches(&[]);
        assert!(matches!(d, Decision::Allow { passthrough_fires } if passthrough_fires.is_empty()));
    }

    #[test]
    fn allow_rule_overrides_require_confirmation() {
        let exc = rule_with("exc", ActionType::Allow);
        let guard = rule_with("guard", ActionType::RequireConfirmation);
        let d = Decision::from_matches(&[make_match(&exc), make_match(&guard)]);
        // Even though a RequireConfirmation rule matched, the Allow rule wins.
        assert!(
            matches!(d, Decision::Allow { ref passthrough_fires }
                if passthrough_fires.iter().any(|f| f.rule_id == "exc")
                && passthrough_fires.iter().any(|f| f.rule_id == "guard")
            ),
            "expected Allow with both rules in passthrough, got {:?}",
            d
        );
    }

    #[test]
    fn allow_rule_overrides_veto() {
        let exc = rule_with("exc", ActionType::Allow);
        let veto = rule_with("v", ActionType::Veto);
        let d = Decision::from_matches(&[make_match(&exc), make_match(&veto)]);
        assert!(
            matches!(d, Decision::Allow { .. }),
            "Allow rule should override Veto, got {:?}",
            d
        );
    }

    #[test]
    fn veto_wins_over_confirm_without_allow() {
        let veto = rule_with("v", ActionType::Veto);
        let guard = rule_with("g", ActionType::RequireConfirmation);
        let d = Decision::from_matches(&[make_match(&veto), make_match(&guard)]);
        assert!(
            matches!(
                d,
                Decision::Veto {
                    soft_block: false,
                    ..
                }
            ),
            "expected Veto, got {:?}",
            d
        );
    }

    #[test]
    fn require_confirmation_with_passthrough_log() {
        let guard = rule_with("g", ActionType::RequireConfirmation);
        let log = rule_with("l", ActionType::LogOnly);
        let d = Decision::from_matches(&[make_match(&guard), make_match(&log)]);
        assert!(
            matches!(d, Decision::RequireConfirmation { ref passthrough_fires, .. }
                if passthrough_fires.iter().any(|f| f.rule_id == "l")
            ),
            "expected RequireConfirmation with log passthrough, got {:?}",
            d
        );
    }
}
