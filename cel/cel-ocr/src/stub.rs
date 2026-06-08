//! Non-macOS stub backend.
//!
//! Vision is macOS-only, so on every other target OCR compiles to this: decode
//! still works (callers can validate input), but recognition reports
//! [`OcrError::NotSupported`] and never references any Apple framework.

use crate::{OcrError, OcrLine, OcrOptions, Result};

/// Always `false` off macOS.
pub fn available() -> bool {
    false
}

/// Always [`OcrError::NotSupported`] off macOS.
pub fn recognize(
    _image_bytes: &[u8],
    _opts: &OcrOptions,
    _width: u32,
    _height: u32,
) -> Result<Vec<OcrLine>> {
    Err(OcrError::NotSupported)
}
