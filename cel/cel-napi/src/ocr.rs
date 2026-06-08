//! OCR napi bindings — on-device text recognition via `cel-ocr` (macOS Vision).
//!
//! This is the local, no-LLM perception fallback exposed to the agent / MCP
//! layer. Callers that already hold a screenshot (e.g. `capture_screen` /
//! `capture_region`) pass the PNG bytes straight in; the MCP `cel_see ocr`
//! mode composes capture + ocr so it reuses the well-tested capture path.

use napi_derive::napi;

/// Recognize text in an encoded image (PNG/JPEG/…) using the on-device Vision
/// framework.
///
/// Returns a JSON string of the shape
/// `{ "count": N, "lines": [ { "text", "confidence", "bounds": { "x", "y",
/// "width", "height" } }, … ] }`. Bounds are pixel coordinates with a top-left
/// origin (matching CEL screenshots / AX). macOS only — errors on other
/// platforms.
///
/// - `fast`: use the faster, lower-accuracy recognition pass (default: false →
///   accurate).
/// - `min_confidence`: drop lines below this confidence, 0.0..=1.0 (default 0).
#[napi]
pub fn ocr(
    image_bytes: napi::bindgen_prelude::Buffer,
    fast: Option<bool>,
    min_confidence: Option<f64>,
) -> napi::Result<String> {
    let opts = cel_ocr::OcrOptions {
        level: if fast.unwrap_or(false) {
            cel_ocr::RecognitionLevel::Fast
        } else {
            cel_ocr::RecognitionLevel::Accurate
        },
        min_confidence: min_confidence.unwrap_or(0.0) as f32,
        ..Default::default()
    };
    let lines = cel_ocr::recognize_text_with(image_bytes.as_ref(), &opts)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let out = serde_json::json!({ "count": lines.len(), "lines": lines });
    serde_json::to_string(&out).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Whether on-device OCR is compiled in and available on this platform (true on
/// macOS, false elsewhere).
#[napi]
pub fn ocr_available() -> bool {
    cel_ocr::ocr_available()
}
