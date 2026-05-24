//! Expression language used in rule `match` clauses.
//!
//! Intentionally tiny. Evaluation is deterministic and side-effect-free.
//! Adding new operators here requires updating the NL-compiler few-shot
//! examples and the matcher's expression dispatcher in `matcher.rs`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The full expression tree.
///
/// `All` and `Any` are short-circuiting. `Not` inverts a single sub-expression.
/// `Leaf` is the atomic comparison: pick a field, an operator, and a value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Expression {
    /// Logical AND over a list of sub-expressions. Empty list matches.
    All(Vec<Expression>),
    /// Logical OR over a list of sub-expressions. Empty list does not match.
    Any(Vec<Expression>),
    /// Negate a sub-expression.
    Not(Box<Expression>),
    /// Atomic comparison.
    Leaf(Leaf),
}

/// A single atomic comparison.
///
/// Fields are addressed by dotted path (see `Event::resolve_field`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Leaf {
    /// Dotted field path. Examples: `kind`, `source`, `data.path`,
    /// `data.action_args.target_path`.
    pub field: String,
    /// Operator to apply.
    pub op: Operator,
    /// Right-hand-side value. Type depends on operator.
    pub value: Value,
}

/// The operator set.
///
/// Numeric ops coerce both sides via `serde_json::Number` semantics.
/// String ops require both sides to be strings. `in_watchlist` / `not_in_watchlist`
/// require `value` to be the watchlist name as a JSON string; the matcher
/// resolves it via `WatchlistLookup`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    /// `field == value`
    Eq,
    /// `field != value`
    Neq,
    /// `field > value` (numeric)
    Gt,
    /// `field >= value` (numeric)
    Gte,
    /// `field < value` (numeric)
    Lt,
    /// `field <= value` (numeric)
    Lte,
    /// `field` starts with the string `value`
    StartsWith,
    /// `field` does not start with the string `value`
    NotStartsWith,
    /// `field` ends with the string `value`
    EndsWith,
    /// `field` does not end with the string `value`
    NotEndsWith,
    /// `field` contains the substring `value`
    Contains,
    /// `field` does not contain the substring `value`
    NotContains,
    /// `field` matches the regex `value`
    Regex,
    /// `field` is one of the items in `value` (an array)
    In,
    /// `field` is not one of the items in `value` (an array)
    NotIn,
    /// `field` is contained in the named watchlist `value`
    InWatchlist,
    /// `field` is not contained in the named watchlist `value`
    NotInWatchlist,
}

impl Expression {
    /// Convenience: build an `All` from a vec.
    pub fn all(exprs: Vec<Expression>) -> Self {
        Expression::All(exprs)
    }

    /// Convenience: build an `Any` from a vec.
    pub fn any(exprs: Vec<Expression>) -> Self {
        Expression::Any(exprs)
    }

    /// Convenience: build a leaf.
    pub fn leaf(field: impl Into<String>, op: Operator, value: impl Into<Value>) -> Self {
        Expression::Leaf(Leaf {
            field: field.into(),
            op,
            value: value.into(),
        })
    }
}
