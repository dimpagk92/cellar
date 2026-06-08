//! Paste-with-restore — insert text via the clipboard, then restore it.
//!
//! Sets the clipboard to `text`, sends the platform paste shortcut, then puts
//! the user's previous clipboard contents back. This lets an agent insert text
//! reliably (no per-character typing, no autocorrect, handles emoji/newlines)
//! WITHOUT clobbering whatever the user had copied. macOS uses Cmd+V.

use crate::inject::{InputController, InputError};

/// Paste `text` into the focused field via the clipboard, restoring the
/// previous clipboard contents afterward.
///
/// Best-effort restore: a non-text or empty original clipboard is left cleared
/// (we can only round-trip text). A short delay between paste and restore gives
/// the target app time to read the pasteboard first.
pub fn paste_with_restore(
    controller: &mut dyn InputController,
    text: &str,
) -> Result<(), InputError> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| InputError::Failed(format!("clipboard open: {e}")))?;

    let original = clipboard.get_text().ok();

    clipboard
        .set_text(text.to_string())
        .map_err(|e| InputError::Failed(format!("clipboard set: {e}")))?;

    // Cmd+V (macOS). The modifier names map through key_combo → enigo::Key::Meta.
    controller.key_combo(&["cmd", "v"])?;

    // Let the target read the pasteboard before we restore it.
    std::thread::sleep(std::time::Duration::from_millis(80));

    if let Some(orig) = original {
        let _ = clipboard.set_text(orig);
    }
    Ok(())
}
