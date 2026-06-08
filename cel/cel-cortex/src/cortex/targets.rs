//! Target validation and element / URL resolution.
//!
//! `validate_targets` checks a plan's action targets against the live model and
//! returns a `TargetValidation`. Plus helpers to resolve elements by id, compute
//! click centers, normalise URLs, and match the declared platform.

use super::*;

pub(crate) fn find_element<'a>(
    context: &'a ScreenContext,
    target_id: &str,
) -> Option<&'a cel_context::ContextElement> {
    context.elements.iter().find(|el| el.id == target_id)
}

pub(crate) fn bounds_center(element: &cel_context::ContextElement) -> Option<(i32, i32)> {
    let bounds = element.bounds.as_ref()?;
    Some((
        bounds.x + (bounds.width as i32 / 2),
        bounds.y + (bounds.height as i32 / 2),
    ))
}

/// Recognize navigation-style `cdp_eval` and extract the destination URL.
///
/// Matches patterns the planner is known to emit, including:
///   * `window.location.href = '<url>'`
///   * `window.location.href='<url>'`
///   * `location.href = "<url>"`
///   * `(function() { window.location.href = '<url>'; return 'navigating'; })()`
///
/// Returns `None` for non-navigation evals. A returned URL has had surrounding
/// quotes stripped and is safe to hand to `reset_preferred_target`.
pub(crate) fn extract_navigation_url(expression: &str) -> Option<String> {
    let normalized = expression.trim();
    let needle = "location.href";
    let idx = normalized.find(needle)?;
    let after = &normalized[idx + needle.len()..];
    let eq = after.find('=')?;
    let rest = after[eq + 1..].trim_start();
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let quote = bytes[0];
    if quote != b'"' && quote != b'\'' && quote != b'`' {
        return None;
    }
    let tail = &rest[1..];
    let end = tail.find(quote as char)?;
    let url = &tail[..end];
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
}

/// Result of `Cortex::validate_targets`. Empty `missing` ⇒ all targets
/// were found in the provided context.
#[derive(Debug, Clone)]
pub struct TargetValidation {
    pub missing: Vec<String>,
}

impl TargetValidation {
    pub fn is_ok(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Normalise a URL for equivalence comparison: strip query, fragment,
/// trailing slash; lowercase. Used by [`Cortex::wait_for_url`] so the
/// planner can't pass `?refresh=1` or `#section` and bypass the URL
/// match. We compare as strings because pulling in the `url` crate
/// just to normalise two known-shape URLs would be overkill.
pub(crate) fn normalise_url(s: &str) -> String {
    let s = s.split('#').next().unwrap_or(s);
    let s = s.split('?').next().unwrap_or(s);
    s.trim_end_matches('/').to_lowercase()
}

/// Whether the given platform string in an adapter manifest matches
/// the current operating system. Used by [`Cortex::register_adapter`]
/// to skip adapters that can't possibly work on this OS.
///
/// Cellar manifests use the Rust target-OS spellings (`macos`,
/// `linux`, `windows`). Matched case-insensitively and tolerantly —
/// "darwin" maps to `macos` (some manifests use the unix name), and
/// whitespace gets trimmed.
pub(crate) fn platform_matches(declared: &str) -> bool {
    let d = declared.trim().to_lowercase();
    let current = std::env::consts::OS;
    match d.as_str() {
        "darwin" | "macos" | "mac" | "osx" => current == "macos",
        "linux" => current == "linux",
        "windows" | "win" | "win32" | "win64" => current == "windows",
        other => other == current,
    }
}

impl Cortex {
    /// Check that all `target_ids` exist in the given context. Returns a
    /// `TargetValidation` reporting any that are missing, so the runner can
    /// replan instead of silently misfiring against stale element IDs.
    ///
    /// Does NOT consult the `MentalModel` directly — callers pass the
    /// context they intend to execute against (typically a post-refresh
    /// snapshot), so validation and dispatch agree on the same element set.
    pub fn validate_targets(
        &self,
        context: &ScreenContext,
        target_ids: &[&str],
    ) -> TargetValidation {
        let missing = target_ids
            .iter()
            .filter(|id| find_element(context, id).is_none())
            .map(|id| (*id).to_string())
            .collect();
        TargetValidation { missing }
    }

    // ─── Liveness API (Phase 1) ─────────────────────────────────────────
}
