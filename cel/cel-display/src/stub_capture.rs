use crate::capture::{CaptureError, Frame, MonitorInfo, ScreenCapture, WindowInfo};

/// Fallback capture backend for environments where native screen capture
/// dependencies are unavailable.
pub struct StubCapture;

impl StubCapture {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenCapture for StubCapture {
    fn init(&mut self) -> Result<(), CaptureError> {
        Err(CaptureError::Unavailable)
    }

    fn capture_frame(&mut self) -> Result<Frame, CaptureError> {
        Err(CaptureError::Unavailable)
    }

    fn capture_monitor(&mut self, _monitor_id: u32) -> Result<Frame, CaptureError> {
        Err(CaptureError::Unavailable)
    }

    fn capture_window(&mut self, _window_id: u32) -> Result<Frame, CaptureError> {
        Err(CaptureError::Unavailable)
    }

    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        Err(CaptureError::Unavailable)
    }

    fn list_windows(&self) -> Result<Vec<WindowInfo>, CaptureError> {
        Err(CaptureError::Unavailable)
    }

    fn resolution(&self) -> (u32, u32) {
        (1920, 1080)
    }
}
