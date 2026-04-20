//! Incognito / private-browsing window detection.
//!
//! Localized title matching across 20+ locales (Chrome, Firefox, Edge, Brave,
//! Safari). Keyword tables adapted from screenpipe-a11y (MIT), which draws
//! from Chromium `generated_resources.grd` / `.xtb` and Firefox Fluent
//! `.ftl` translation files.
//!
//! Strategy bias: a false positive (skipping a normal window) is much less
//! harmful than a false negative (recording an incognito window). The
//! keyword list is intentionally broad.
//!
//! Pure function, no I/O — safe to call per-frame.

/// Return true if the window title matches a known incognito / private
/// browsing indicator in any supported locale.
pub fn is_title_private(window_title: &str) -> bool {
    if window_title.is_empty() {
        return false;
    }
    let lower = window_title.to_lowercase();

    for kw in ENGLISH_KEYWORDS {
        if lower.contains(kw) {
            return true;
        }
    }
    for kw in LOCALIZED_KEYWORDS {
        if lower.contains(kw) {
            return true;
        }
    }
    for kw in CJK_KEYWORDS {
        if window_title.contains(kw) {
            return true;
        }
    }
    false
}

/// Common English incognito / private phrases across Chrome, Firefox, Edge,
/// Brave and Safari. Specific phrases are used instead of bare "private" to
/// avoid false positives on normal windows ("Private API docs", etc.).
const ENGLISH_KEYWORDS: &[&str] = &[
    "incognito",
    "inprivate",
    "private browsing",
    "private window",
    "private mode",
    "- private",
    "(private)",
    "brave private",
];

/// Localized strings from Chromium and Firefox translations. All lowercase.
const LOCALIZED_KEYWORDS: &[&str] = &[
    // German
    "inkognito",
    "privater modus",
    "privates fenster",
    // French
    "navigation privée",
    "navigation privee",
    // Spanish
    "incógnito",
    "navegación privada",
    "navegacion privada",
    // Portuguese
    "navegação privada",
    "navegacao privada",
    "anônima",
    "anonima",
    // Italian
    "navigazione anonima",
    // Dutch
    "incognitovenster",
    "privévenster",
    "privevenster",
    // Polish
    "przeglądanie prywatne",
    "przegladanie prywatne",
    // Turkish
    "gizli sekme",
    "gizli gezinme",
    // Russian
    "инкогнито",
    "приватное окно",
    // Ukrainian
    "інкогніто",
    "приватне вікно",
    // Arabic
    "تصفح متخفي",
    "تصفح خاص",
    // Hindi
    "गुप्त",
    // Thai
    "ไม่ระบุตัวตน",
    // Vietnamese
    "ẩn danh",
    // Czech
    "anonymní",
    "soukromé prohlížení",
    // Romanian
    "navigare privată",
    // Hungarian
    "inkognitó",
    "privát böngészés",
    // Swedish
    "inkognitofönster",
    "privat surfning",
    // Norwegian
    "inkognitovindu",
    "privat nettlesing",
    // Danish
    "inkognitovindue",
    "privat browsing",
    // Finnish
    "incognito-ikkuna",
    "yksityinen selaus",
    // Greek
    "ανώνυμη περιήγηση",
    "ιδιωτική περιήγηση",
    // Hebrew
    "גלישה בסתר",
    "גלישה פרטית",
];

/// CJK / non-Latin strings where lowercasing is a no-op — checked against
/// the original title.
const CJK_KEYWORDS: &[&str] = &[
    // Japanese
    "シークレット",
    "プライベートブラウジング",
    // Chinese Simplified
    "无痕",
    "隐身",
    "隐私浏览",
    // Chinese Traditional
    "無痕",
    "隱私瀏覽",
    // Korean
    "시크릿",
    "사생활 보호",
];

/// Detector trait — platforms can extend with native APIs (e.g. AppleScript
/// `get mode of window` on macOS Chromium). Default impl uses title matching.
pub trait IncognitoDetector: Send + Sync {
    fn is_incognito(&self, _app_name: &str, _process_id: i32, window_title: &str) -> bool {
        is_title_private(window_title)
    }
}

/// Title-only detector — all platforms. No native calls, pure function.
pub struct TitleOnlyDetector;

impl IncognitoDetector for TitleOnlyDetector {}

/// Create the default detector. Platform-specific native detectors can be
/// plugged in later.
pub fn create_detector() -> Box<dyn IncognitoDetector> {
    Box::new(TitleOnlyDetector)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_chrome_incognito_english() {
        assert!(is_title_private("New Tab - Google Chrome (Incognito)"));
    }

    #[test]
    fn detects_firefox_private_english() {
        assert!(is_title_private("Mozilla Firefox (Private Browsing)"));
    }

    #[test]
    fn detects_edge_inprivate() {
        assert!(is_title_private("Bing - InPrivate - Microsoft Edge"));
    }

    #[test]
    fn detects_safari_private_window() {
        assert!(is_title_private("Safari — Private Window"));
    }

    #[test]
    fn detects_german_inkognito() {
        assert!(is_title_private("Neuer Tab - Google Chrome (Inkognito)"));
        assert!(is_title_private("Startseite — Firefox (Privater Modus)"));
    }

    #[test]
    fn detects_french_navigation_privee() {
        assert!(is_title_private("Accueil — Firefox (Navigation privée)"));
    }

    #[test]
    fn detects_japanese_secret() {
        assert!(is_title_private(
            "新しいタブ - Google Chrome (シークレット)"
        ));
    }

    #[test]
    fn detects_chinese_wuhen() {
        assert!(is_title_private("新标签页 - Google Chrome (无痕模式)"));
    }

    #[test]
    fn detects_korean_secret() {
        assert!(is_title_private("새 탭 - Chrome (시크릿 모드)"));
    }

    #[test]
    fn detects_russian_incognito() {
        assert!(is_title_private(
            "Новая вкладка — Google Chrome (Инкогнито)"
        ));
    }

    #[test]
    fn normal_titles_not_flagged() {
        assert!(!is_title_private("GitHub - Google Chrome"));
        assert!(!is_title_private("Reddit - Mozilla Firefox"));
        assert!(!is_title_private("Untitled - TextEdit"));
    }

    #[test]
    fn avoids_false_positives_on_private_word() {
        assert!(!is_title_private("Private API docs - Chrome"));
        assert!(!is_title_private("Secret Santa Planning - Firefox"));
        assert!(!is_title_private("Enter Password - Chrome"));
        assert!(!is_title_private("My Private Repository - GitHub"));
    }

    #[test]
    fn empty_and_whitespace_are_not_private() {
        assert!(!is_title_private(""));
        assert!(!is_title_private("   "));
    }

    #[test]
    fn case_insensitive_english() {
        assert!(is_title_private("INCOGNITO - Chrome"));
        assert!(is_title_private("PRIVATE BROWSING - Firefox"));
        assert!(is_title_private("INPRIVATE - Edge"));
    }

    #[test]
    fn default_trait_uses_title_detection() {
        let d = create_detector();
        assert!(d.is_incognito("Chrome", 1234, "Something (Incognito)"));
        assert!(!d.is_incognito("Chrome", 1234, "GitHub - Chrome"));
    }
}
