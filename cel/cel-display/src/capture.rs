use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

/// A captured screen frame.
#[derive(Clone, Serialize, Deserialize)]
pub struct Frame {
    /// Raw RGBA pixel data.
    pub data: Vec<u8>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Capture timestamp (milliseconds since epoch).
    pub timestamp_ms: u64,
}

/// Information about a display monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    /// Display scale factor (e.g. 2.0 for Retina displays).
    #[serde(default = "default_scale_factor")]
    pub scale_factor: f64,
}

fn default_scale_factor() -> f64 {
    1.0
}

/// Information about a window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_minimized: bool,
}

/// Error type for screen capture operations.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("Screen capture not available on this platform")]
    Unavailable,
    #[error("Failed to capture frame: {0}")]
    CaptureFailed(String),
    #[error("Capture not initialized")]
    NotInitialized,
    #[error("No monitors found")]
    NoMonitors,
    #[error("Monitor not found: {0}")]
    MonitorNotFound(u32),
    #[error("Window not found: {0}")]
    WindowNotFound(u32),
    #[error("Image encoding error: {0}")]
    EncodingError(String),
}

/// Platform-agnostic screen capture trait.
pub trait ScreenCapture: Send + Sync {
    /// Initialize the capture session.
    fn init(&mut self) -> Result<(), CaptureError>;

    /// Capture the primary monitor.
    fn capture_frame(&mut self) -> Result<Frame, CaptureError>;

    /// Capture a specific monitor by ID.
    fn capture_monitor(&mut self, monitor_id: u32) -> Result<Frame, CaptureError>;

    /// Capture a specific window by ID.
    fn capture_window(&mut self, window_id: u32) -> Result<Frame, CaptureError>;

    /// List available monitors.
    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError>;

    /// List visible windows.
    fn list_windows(&self) -> Result<Vec<WindowInfo>, CaptureError>;

    /// Get the primary display resolution.
    fn resolution(&self) -> (u32, u32);
}

/// Thread-safe latest frame holder for continuous capture.
pub type LatestFrame = Arc<RwLock<Option<Frame>>>;

/// Encode a frame as PNG bytes.
pub fn encode_png(frame: &Frame) -> Result<Vec<u8>, CaptureError> {
    use image::{ImageBuffer, RgbaImage};
    let img: RgbaImage = ImageBuffer::from_raw(frame.width, frame.height, frame.data.clone())
        .ok_or_else(|| CaptureError::EncodingError("Invalid frame dimensions".into()))?;
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    image::ImageEncoder::write_image(
        encoder,
        &img,
        frame.width,
        frame.height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| CaptureError::EncodingError(e.to_string()))?;
    Ok(buf)
}

/// Resize a frame so it fits within the given maximum dimensions, preserving aspect ratio.
/// Returns the original frame unchanged if it already fits.
pub fn resize_frame(frame: &Frame, max_width: u32, max_height: u32) -> Result<Frame, CaptureError> {
    if frame.width <= max_width && frame.height <= max_height {
        return Ok(frame.clone());
    }
    let img = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(
        frame.width,
        frame.height,
        frame.data.clone(),
    )
    .ok_or(CaptureError::EncodingError("Invalid frame data".into()))?;
    let ratio = (max_width as f64 / frame.width as f64)
        .min(max_height as f64 / frame.height as f64);
    let new_w = (frame.width as f64 * ratio) as u32;
    let new_h = (frame.height as f64 * ratio) as u32;
    let resized =
        image::imageops::resize(&img, new_w, new_h, image::imageops::FilterType::Lanczos3);
    Ok(Frame {
        data: resized.into_raw(),
        width: new_w,
        height: new_h,
        timestamp_ms: frame.timestamp_ms,
    })
}

/// Encode a frame as JPEG bytes at the given quality (0-100).
pub fn encode_jpeg(frame: &Frame, quality: u8) -> Result<Vec<u8>, CaptureError> {
    let img = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(
        frame.width,
        frame.height,
        frame.data.clone(),
    )
    .ok_or(CaptureError::EncodingError("Invalid frame data".into()))?;
    let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
        .encode_image(&rgb)
        .map_err(|e| CaptureError::EncodingError(e.to_string()))?;
    Ok(buf)
}

/// Crop a region from a frame. Returns a new Frame with only the specified rectangle.
/// Coordinates are clamped to frame bounds.
pub fn crop_frame(frame: &Frame, x: u32, y: u32, w: u32, h: u32) -> Result<Frame, CaptureError> {
    let x = x.min(frame.width);
    let y = y.min(frame.height);
    let w = w.min(frame.width.saturating_sub(x));
    let h = h.min(frame.height.saturating_sub(y));
    if w == 0 || h == 0 {
        return Err(CaptureError::EncodingError("Crop region is empty".into()));
    }
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for row in y..(y + h) {
        let start = ((row * frame.width + x) * 4) as usize;
        let end = start + (w * 4) as usize;
        if end <= frame.data.len() {
            data.extend_from_slice(&frame.data[start..end]);
        }
    }
    Ok(Frame {
        data,
        width: w,
        height: h,
        timestamp_ms: frame.timestamp_ms,
    })
}

/// Get the RGBA color at a specific pixel. Returns [R, G, B, A].
pub fn pixel_color(frame: &Frame, x: u32, y: u32) -> Option<[u8; 4]> {
    if x >= frame.width || y >= frame.height {
        return None;
    }
    let offset = ((y * frame.width + x) * 4) as usize;
    if offset + 4 > frame.data.len() {
        return None;
    }
    Some([
        frame.data[offset],
        frame.data[offset + 1],
        frame.data[offset + 2],
        frame.data[offset + 3],
    ])
}

/// Find rectangular regions that differ between two frames.
/// Divides the frames into a grid of cells and reports which cells changed.
/// Returns a list of (x, y, width, height) bounding boxes.
pub fn diff_regions(a: &Frame, b: &Frame, cell_size: u32) -> Vec<(u32, u32, u32, u32)> {
    if a.width != b.width || a.height != b.height || cell_size == 0 {
        return vec![(0, 0, a.width, a.height)]; // Entirely different
    }
    let cols = (a.width + cell_size - 1) / cell_size;
    let rows = (a.height + cell_size - 1) / cell_size;
    let mut changed_cells = Vec::new();

    for row in 0..rows {
        for col in 0..cols {
            let cx = col * cell_size;
            let cy = row * cell_size;
            let cw = cell_size.min(a.width - cx);
            let ch = cell_size.min(a.height - cy);

            let mut diff_count = 0u32;
            let threshold = (cw * ch) / 50; // >2% of cell pixels changed

            'cell: for y in cy..(cy + ch) {
                for x in cx..(cx + cw) {
                    let off = ((y * a.width + x) * 4) as usize;
                    if off + 3 < a.data.len() && off + 3 < b.data.len() {
                        let dr = (a.data[off] as i16 - b.data[off] as i16).unsigned_abs();
                        let dg = (a.data[off + 1] as i16 - b.data[off + 1] as i16).unsigned_abs();
                        let db = (a.data[off + 2] as i16 - b.data[off + 2] as i16).unsigned_abs();
                        if dr > 10 || dg > 10 || db > 10 {
                            diff_count += 1;
                            if diff_count > threshold {
                                changed_cells.push((cx, cy, cw, ch));
                                break 'cell;
                            }
                        }
                    }
                }
            }
        }
    }

    // Merge adjacent cells into larger bounding boxes would be ideal,
    // but returning raw cells is useful and simple
    changed_cells
}

/// One-call optimization for the LLM vision path.
/// Resize to fit within max_dimension (longest side), encode as JPEG, return base64.
/// This is the hot path for screenshot → LLM API calls.
pub fn encode_for_llm(frame: &Frame, max_dimension: u32, jpeg_quality: u8) -> Result<String, CaptureError> {
    use base64::Engine;
    let resized = resize_frame(frame, max_dimension, max_dimension)?;
    let jpeg = encode_jpeg(&resized, jpeg_quality)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&jpeg))
}

/// Returns `true` if two frames differ significantly (more than 1% of pixels
/// change by more than 10 intensity units in any channel).
pub fn frames_differ(a: &Frame, b: &Frame) -> bool {
    if a.width != b.width || a.height != b.height {
        return true;
    }
    let total = a.data.len();
    if total == 0 {
        return false;
    }
    let changed = a
        .data
        .iter()
        .zip(b.data.iter())
        .filter(|(a, b)| (**a as i16 - **b as i16).unsigned_abs() > 10)
        .count();
    changed > total / 100
}
