//! CEL OCR — local, on-device text recognition.
//!
//! This wraps Apple's **Vision** framework (`VNRecognizeTextRequest` +
//! `VNImageRequestHandler`). It is the fast/free/deterministic perception
//! fallback for surfaces with **no accessibility tree** — `<canvas>`, games,
//! image-only documents, some Electron apps — where CEL would otherwise have to
//! pay a slow, non-deterministic vision-LLM round-trip just to read text.
//!
//! Key property: **this is not an LLM.** Vision runs entirely on-device. No
//! network, no tokens, millisecond latency, and the same `(text, bounds,
//! confidence)` output for the same pixels every time. It complements — does
//! not replace — CEL's AX tree (structured truth) and its VLM path (semantic
//! question-answering). When an element is in the AX tree, prefer that; reach
//! for OCR only when the pixels are all you have.
//!
//! ## Coordinate system
//!
//! Vision reports bounding boxes **normalized** (0..1) with a **bottom-left**
//! origin. [`OcrLine::bounds`] is converted to **pixel** coordinates with a
//! **top-left** origin (matching CEL's screenshot / AX convention) so the boxes
//! line up with what [`cel_act`] click targets expect. The conversion needs the
//! source image's pixel dimensions, which are read from the encoded bytes.
//!
//! ## Platforms
//!
//! macOS only. On any other target this crate compiles to a stub that returns
//! [`OcrError::NotSupported`], so it is safe to depend on from cross-platform
//! crates.

use serde::Serialize;

/// Errors from an OCR request.
#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    /// OCR is not available on this platform (non-macOS stub build).
    #[error("OCR not supported on this platform — Vision is macOS-only")]
    NotSupported,
    /// The supplied bytes could not be decoded as an image.
    #[error("could not decode image: {0}")]
    DecodeFailed(String),
    /// The Vision request handler could not be constructed or run.
    #[error("Vision request failed: {0}")]
    RequestFailed(String),
}

/// Result alias for OCR operations.
pub type Result<T> = std::result::Result<T, OcrError>;

/// How thorough the recognition pass should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecognitionLevel {
    /// Slower, neural, far more accurate. The default — OCR is already cheap.
    #[default]
    Accurate,
    /// Faster, lower accuracy. Use when latency matters more than fidelity.
    Fast,
}

/// Options controlling a recognition pass.
#[derive(Debug, Clone, Serialize)]
pub struct OcrOptions {
    /// Accuracy/speed trade-off. Defaults to [`RecognitionLevel::Accurate`].
    pub level: RecognitionLevel,
    /// Apply language-based correction to candidates. Defaults to `true`.
    pub language_correction: bool,
    /// Drop lines whose confidence is below this (0.0..=1.0). Defaults to `0.0`
    /// (keep everything); Vision's own confidences are usually >0.3 for real
    /// text.
    pub min_confidence: f32,
    /// Preferred BCP-47 language hints (e.g. `["en-US"]`). Empty = Vision's
    /// automatic language detection.
    pub languages: Vec<String>,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            level: RecognitionLevel::Accurate,
            language_correction: true,
            min_confidence: 0.0,
            languages: Vec::new(),
        }
    }
}

/// A pixel-space bounding box, top-left origin (matches CEL screenshots / AX).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct OcrBounds {
    /// Left edge, pixels from the image's left.
    pub x: f64,
    /// Top edge, pixels from the image's top.
    pub y: f64,
    /// Width in pixels.
    pub width: f64,
    /// Height in pixels.
    pub height: f64,
}

impl OcrBounds {
    /// Centre point — the natural click target for this line.
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// One recognized line of text with its location and confidence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OcrLine {
    /// The recognized text (top candidate).
    pub text: String,
    /// Vision's confidence in the top candidate, 0.0..=1.0.
    pub confidence: f32,
    /// Pixel bounding box, top-left origin.
    pub bounds: OcrBounds,
}

/// Recognize text in an encoded image (PNG, JPEG, …) using default options.
///
/// `image_bytes` is the encoded image — exactly what `cel-display`'s
/// `encode_png` produces, or any screenshot file's contents.
pub fn recognize_text(image_bytes: &[u8]) -> Result<Vec<OcrLine>> {
    recognize_text_with(image_bytes, &OcrOptions::default())
}

/// Recognize text with explicit [`OcrOptions`].
pub fn recognize_text_with(image_bytes: &[u8], opts: &OcrOptions) -> Result<Vec<OcrLine>> {
    // Pixel dimensions are needed to de-normalize Vision's bounding boxes.
    let (width, height) = image_dimensions(image_bytes)?;
    let mut lines = backend::recognize(image_bytes, opts, width, height)?;
    if opts.min_confidence > 0.0 {
        lines.retain(|l| l.confidence >= opts.min_confidence);
    }
    Ok(lines)
}

/// Whether real (non-stub) OCR is compiled in and available on this platform.
pub fn ocr_available() -> bool {
    backend::available()
}

/// Read just the pixel dimensions of an encoded image, cheaply.
fn image_dimensions(image_bytes: &[u8]) -> Result<(u32, u32)> {
    image::load_from_memory(image_bytes)
        .map(|img| {
            use image::GenericImageView;
            img.dimensions()
        })
        .map_err(|e| OcrError::DecodeFailed(e.to_string()))
}

#[cfg(target_os = "macos")]
#[path = "imp.rs"]
mod backend;

#[cfg(not(target_os = "macos"))]
#[path = "stub.rs"]
mod backend;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_line_serializes_with_pixel_bounds() {
        let line = OcrLine {
            text: "Hello".into(),
            confidence: 0.97,
            bounds: OcrBounds {
                x: 10.0,
                y: 20.0,
                width: 40.0,
                height: 12.0,
            },
        };
        let json = serde_json::to_string(&line).unwrap();
        assert!(json.contains("\"text\":\"Hello\""));
        assert!(json.contains("\"confidence\":0.97"));
        assert!(json.contains("\"x\":10.0"));
    }

    #[test]
    fn bounds_center_is_midpoint() {
        let b = OcrBounds {
            x: 10.0,
            y: 20.0,
            width: 40.0,
            height: 12.0,
        };
        assert_eq!(b.center(), (30.0, 26.0));
    }

    #[test]
    fn default_options_are_accurate_and_corrected() {
        let o = OcrOptions::default();
        assert_eq!(o.level, RecognitionLevel::Accurate);
        assert!(o.language_correction);
        assert_eq!(o.min_confidence, 0.0);
    }

    #[test]
    fn garbage_bytes_fail_to_decode() {
        let err = recognize_text(&[0, 1, 2, 3]).unwrap_err();
        assert!(matches!(err, OcrError::DecodeFailed(_)));
    }

    // On non-macOS the backend is the stub and reports unavailable.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn stub_is_unavailable_off_macos() {
        assert!(!ocr_available());
    }
}
