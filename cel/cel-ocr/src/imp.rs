//! macOS Vision-framework OCR backend.
//!
//! Drives `VNImageRequestHandler` + `VNRecognizeTextRequest` synchronously and
//! converts Vision's normalized, bottom-left-origin bounding boxes into
//! pixel-space, top-left-origin [`OcrBounds`].

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedTextObservation, VNRequest,
    VNRequestTextRecognitionLevel,
};

use crate::{OcrBounds, OcrError, OcrLine, OcrOptions, RecognitionLevel, Result};

/// Vision is always available on macOS.
pub fn available() -> bool {
    true
}

/// Run a synchronous text-recognition pass over `image_bytes`.
pub fn recognize(
    image_bytes: &[u8],
    opts: &OcrOptions,
    width: u32,
    height: u32,
) -> Result<Vec<OcrLine>> {
    let data = NSData::with_bytes(image_bytes);

    // Build the recognition request and apply options. objc2-vision marks these
    // accessors safe, so no `unsafe` is needed around them.
    let request: Retained<VNRecognizeTextRequest> = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(match opts.level {
        RecognitionLevel::Accurate => VNRequestTextRecognitionLevel::Accurate,
        RecognitionLevel::Fast => VNRequestTextRecognitionLevel::Fast,
    });
    request.setUsesLanguageCorrection(opts.language_correction);
    if !opts.languages.is_empty() {
        let langs: Vec<Retained<NSString>> = opts
            .languages
            .iter()
            .map(|s| NSString::from_str(s))
            .collect();
        let arr = NSArray::from_retained_slice(&langs);
        request.setRecognitionLanguages(&arr);
    }

    // The handler owns the image; options dict is empty (no orientation hints).
    // The key type is `VNImageOption` (an `NSString` typedef); an empty,
    // explicitly-typed dictionary avoids any transmute.
    let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
    let handler = VNImageRequestHandler::initWithData_options(
        VNImageRequestHandler::alloc(),
        &data,
        &options,
    );

    // Perform synchronously. VNRecognizeTextRequest is-a VNRequest.
    let req_ref: &VNRequest = &request;
    let requests = NSArray::from_slice(&[req_ref]);
    handler
        .performRequests_error(&requests)
        .map_err(|e| OcrError::RequestFailed(e.localizedDescription().to_string()))?;

    // Collect observations → lines.
    let mut lines = Vec::new();
    let Some(results) = request.results() else {
        return Ok(lines);
    };
    let count = results.count();
    for i in 0..count {
        let obs: Retained<VNRecognizedTextObservation> = results.objectAtIndex(i);
        let candidates = obs.topCandidates(1);
        if candidates.count() == 0 {
            continue;
        }
        let top = candidates.objectAtIndex(0);
        let text = top.string().to_string();
        if text.is_empty() {
            continue;
        }
        let confidence = top.confidence();

        // Vision: normalized (0..1), bottom-left origin. Convert to pixels,
        // top-left origin.
        // SAFETY: reading the observation's normalized bounding box.
        let bbox = unsafe { obs.boundingBox() };
        let w = width as f64;
        let h = height as f64;
        let bounds = OcrBounds {
            x: bbox.origin.x * w,
            y: (1.0 - bbox.origin.y - bbox.size.height) * h,
            width: bbox.size.width * w,
            height: bbox.size.height * h,
        };

        lines.push(OcrLine {
            text,
            confidence,
            bounds,
        });
    }

    Ok(lines)
}
