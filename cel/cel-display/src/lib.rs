//! CEL Display Layer
//!
//! Screen capture and virtual framebuffer for the Context Execution Layer.
//! Uses xcap for cross-platform screen and window capture.

mod capture;
#[cfg(not(feature = "xcap"))]
mod stub_capture;
#[cfg(feature = "xcap")]
mod xcap_capture;

pub use capture::{
    crop_frame, diff_regions, encode_for_llm, encode_jpeg, encode_png, frames_differ, pixel_color,
    resize_frame, CaptureError, Frame, LatestFrame, MonitorInfo, ScreenCapture, WindowInfo,
};
#[cfg(not(feature = "xcap"))]
pub use stub_capture::StubCapture;
#[cfg(feature = "xcap")]
pub use xcap_capture::XcapCapture;

/// Create a screen capture instance.
pub fn create_capture() -> Box<dyn ScreenCapture> {
    #[cfg(feature = "xcap")]
    {
        Box::new(XcapCapture::new())
    }
    #[cfg(not(feature = "xcap"))]
    {
        Box::new(StubCapture::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_creation() {
        let frame = Frame {
            data: vec![255, 0, 0, 255],
            width: 1,
            height: 1,
            timestamp_ms: 1234567890,
        };
        assert_eq!(frame.width, 1);
        assert_eq!(frame.height, 1);
        assert_eq!(frame.data.len(), 4);
        assert_eq!(frame.timestamp_ms, 1234567890);
    }

    #[test]
    fn test_frame_serialization_roundtrip() {
        let frame = Frame {
            data: vec![0, 0, 0, 255, 255, 255, 255, 255],
            width: 2,
            height: 1,
            timestamp_ms: 100,
        };
        let json = serde_json::to_string(&frame).unwrap();
        let back: Frame = serde_json::from_str(&json).unwrap();
        assert_eq!(back.width, 2);
        assert_eq!(back.height, 1);
        assert_eq!(back.data, frame.data);
    }

    #[test]
    fn test_encode_png_valid_2x2() {
        let frame = Frame {
            data: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
            ],
            width: 2,
            height: 2,
            timestamp_ms: 0,
        };
        let png = encode_png(&frame).unwrap();
        assert!(png.len() > 8);
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn test_encode_png_invalid_dimensions() {
        let frame = Frame {
            data: vec![0, 0, 0, 255],
            width: 10,
            height: 10,
            timestamp_ms: 0,
        };
        assert!(encode_png(&frame).is_err());
    }

    #[test]
    fn test_encode_png_single_pixel() {
        let frame = Frame {
            data: vec![128, 64, 32, 255],
            width: 1,
            height: 1,
            timestamp_ms: 42,
        };
        let png = encode_png(&frame).unwrap();
        assert!(!png.is_empty());
    }

    #[test]
    fn test_monitor_info_serialization() {
        let info = MonitorInfo {
            id: 1,
            name: "Primary".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            is_primary: true,
            scale_factor: 2.0,
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: MonitorInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 1);
        assert_eq!(back.name, "Primary");
        assert!(back.is_primary);
        assert_eq!(back.width, 1920);
        assert_eq!(back.height, 1080);
    }

    #[test]
    fn test_window_info_serialization() {
        let info = WindowInfo {
            id: 42,
            title: "My App - Document".into(),
            app_name: "MyApp".into(),
            x: 100,
            y: 50,
            width: 800,
            height: 600,
            is_minimized: false,
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: WindowInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 42);
        assert_eq!(back.title, "My App - Document");
        assert!(!back.is_minimized);
    }

    #[test]
    fn test_capture_error_display() {
        assert_eq!(
            CaptureError::Unavailable.to_string(),
            "Screen capture not available on this platform"
        );
        assert_eq!(
            CaptureError::MonitorNotFound(5).to_string(),
            "Monitor not found: 5"
        );
        assert_eq!(
            CaptureError::WindowNotFound(99).to_string(),
            "Window not found: 99"
        );
        assert_eq!(
            CaptureError::NotInitialized.to_string(),
            "Capture not initialized"
        );
        assert_eq!(CaptureError::NoMonitors.to_string(), "No monitors found");
        assert_eq!(
            CaptureError::EncodingError("bad".into()).to_string(),
            "Image encoding error: bad"
        );
        assert_eq!(
            CaptureError::CaptureFailed("oops".into()).to_string(),
            "Failed to capture frame: oops"
        );
    }

    #[test]
    #[cfg(feature = "xcap")]
    fn test_create_capture_returns_instance() {
        let capture = create_capture();
        let _res = capture.resolution();
    }

    #[test]
    fn test_latest_frame_type() {
        let latest: LatestFrame = std::sync::Arc::new(std::sync::RwLock::new(None));
        assert!(latest.read().unwrap().is_none());

        let frame = Frame {
            data: vec![0; 4],
            width: 1,
            height: 1,
            timestamp_ms: 0,
        };
        *latest.write().unwrap() = Some(frame);
        assert!(latest.read().unwrap().is_some());
    }

    /// Helper: create a 4x4 RGBA test frame with known pixel values.
    fn make_test_frame() -> Frame {
        // 4x4 frame: 64 bytes (4 pixels wide × 4 pixels tall × 4 channels)
        let mut data = vec![0u8; 4 * 4 * 4];
        // Top-left pixel: red
        data[0] = 255;
        data[1] = 0;
        data[2] = 0;
        data[3] = 255;
        // (1,0): green
        data[4] = 0;
        data[5] = 255;
        data[6] = 0;
        data[7] = 255;
        // (2,0): blue
        data[8] = 0;
        data[9] = 0;
        data[10] = 255;
        data[11] = 255;
        // (3,0): white
        data[12] = 255;
        data[13] = 255;
        data[14] = 255;
        data[15] = 255;
        Frame {
            data,
            width: 4,
            height: 4,
            timestamp_ms: 1000,
        }
    }

    #[test]
    fn test_crop_frame() {
        let frame = make_test_frame();
        // Crop top-left 2x2
        let cropped = crop_frame(&frame, 0, 0, 2, 2).unwrap();
        assert_eq!(cropped.width, 2);
        assert_eq!(cropped.height, 2);
        assert_eq!(cropped.data.len(), 2 * 2 * 4);
        // First pixel should be red
        assert_eq!(cropped.data[0], 255);
        assert_eq!(cropped.data[1], 0);
    }

    #[test]
    fn test_crop_clamped_to_bounds() {
        let frame = make_test_frame();
        // Request bigger than frame — should clamp
        let cropped = crop_frame(&frame, 2, 2, 100, 100).unwrap();
        assert_eq!(cropped.width, 2);
        assert_eq!(cropped.height, 2);
    }

    #[test]
    fn test_crop_empty_returns_error() {
        let frame = make_test_frame();
        assert!(crop_frame(&frame, 100, 100, 1, 1).is_err());
    }

    #[test]
    fn test_crop_offset_region_extracts_correct_pixels() {
        // WS18 region capture core: cropping an OFFSET sub-rectangle must
        // return exactly those source pixels, not the top-left ones. Row 0 of
        // the test frame is red, green, blue, white; a 2x1 crop at x=1 must
        // yield [green, blue].
        let frame = make_test_frame();
        let cropped = crop_frame(&frame, 1, 0, 2, 1).unwrap();
        assert_eq!(cropped.width, 2);
        assert_eq!(cropped.height, 1);
        assert_eq!(cropped.data.len(), 2 * 4);
        assert_eq!(&cropped.data[0..4], &[0, 255, 0, 255]); // source (1,0) green
        assert_eq!(&cropped.data[4..8], &[0, 0, 255, 255]); // source (2,0) blue
    }

    #[test]
    fn test_pixel_color() {
        let frame = make_test_frame();
        let c = pixel_color(&frame, 0, 0).unwrap();
        assert_eq!(c, [255, 0, 0, 255]); // Red
        let c = pixel_color(&frame, 1, 0).unwrap();
        assert_eq!(c, [0, 255, 0, 255]); // Green
        assert!(pixel_color(&frame, 100, 100).is_none()); // Out of bounds
    }

    #[test]
    fn test_diff_regions_identical() {
        let frame = make_test_frame();
        let regions = diff_regions(&frame, &frame, 2);
        assert!(regions.is_empty());
    }

    #[test]
    fn test_diff_regions_different() {
        let frame_a = make_test_frame();
        let mut frame_b = make_test_frame();
        // Change the entire first row to white
        for i in 0..16 {
            frame_b.data[i] = 255;
        }
        let regions = diff_regions(&frame_a, &frame_b, 2);
        assert!(!regions.is_empty());
    }

    #[test]
    fn test_encode_for_llm() {
        let frame = make_test_frame();
        let b64 = encode_for_llm(&frame, 4, 80).unwrap();
        assert!(!b64.is_empty());
        // Should be valid base64
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .unwrap();
        assert!(!decoded.is_empty());
    }
}
