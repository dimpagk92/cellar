use crate::capture::{CaptureError, Frame, MonitorInfo, ScreenCapture, WindowInfo};
use std::time::{SystemTime, UNIX_EPOCH};

/// Cross-platform screen capture using xcap.
pub struct XcapCapture {
    initialized: bool,
    primary_width: u32,
    primary_height: u32,
}

impl Default for XcapCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl XcapCapture {
    pub fn new() -> Self {
        Self {
            initialized: false,
            primary_width: 0,
            primary_height: 0,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn image_to_frame(img: image::RgbaImage) -> Frame {
    let width = img.width();
    let height = img.height();
    Frame {
        data: img.into_raw(),
        width,
        height,
        timestamp_ms: now_ms(),
    }
}

fn map_err(e: xcap::XCapError) -> CaptureError {
    CaptureError::CaptureFailed(e.to_string())
}

impl ScreenCapture for XcapCapture {
    fn init(&mut self) -> Result<(), CaptureError> {
        let monitors = xcap::Monitor::all().map_err(map_err)?;
        if monitors.is_empty() {
            return Err(CaptureError::NoMonitors);
        }
        for m in &monitors {
            if m.is_primary().unwrap_or(false) {
                self.primary_width = m.width().unwrap_or(1920);
                self.primary_height = m.height().unwrap_or(1080);
                break;
            }
        }
        if self.primary_width == 0 {
            self.primary_width = monitors[0].width().unwrap_or(1920);
            self.primary_height = monitors[0].height().unwrap_or(1080);
        }
        self.initialized = true;
        tracing::info!(
            "Display capture initialized: {}x{}",
            self.primary_width,
            self.primary_height
        );
        Ok(())
    }

    fn capture_frame(&mut self) -> Result<Frame, CaptureError> {
        if !self.initialized {
            self.init()?;
        }
        let monitors = xcap::Monitor::all().map_err(map_err)?;
        let primary = monitors
            .into_iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .or_else(|| xcap::Monitor::all().ok().and_then(|m| m.into_iter().next()))
            .ok_or(CaptureError::NoMonitors)?;
        let img = primary.capture_image().map_err(map_err)?;
        Ok(image_to_frame(img))
    }

    fn capture_monitor(&mut self, monitor_id: u32) -> Result<Frame, CaptureError> {
        if !self.initialized {
            self.init()?;
        }
        let monitors = xcap::Monitor::all().map_err(map_err)?;
        let monitor = monitors
            .into_iter()
            .find(|m| m.id().unwrap_or(0) == monitor_id)
            .ok_or(CaptureError::MonitorNotFound(monitor_id))?;
        let img = monitor.capture_image().map_err(map_err)?;
        Ok(image_to_frame(img))
    }

    fn capture_window(&mut self, window_id: u32) -> Result<Frame, CaptureError> {
        if !self.initialized {
            self.init()?;
        }
        let windows = xcap::Window::all().map_err(map_err)?;
        let window = windows
            .into_iter()
            .find(|w| w.id().unwrap_or(0) == window_id)
            .ok_or(CaptureError::WindowNotFound(window_id))?;
        let img = window.capture_image().map_err(map_err)?;
        Ok(image_to_frame(img))
    }

    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        let monitors = xcap::Monitor::all().map_err(map_err)?;
        Ok(monitors
            .into_iter()
            .filter_map(|m| {
                Some(MonitorInfo {
                    id: m.id().ok()?,
                    name: m.name().unwrap_or_else(|_| "Unknown".into()),
                    x: m.x().unwrap_or(0),
                    y: m.y().unwrap_or(0),
                    width: m.width().ok()?,
                    height: m.height().ok()?,
                    is_primary: m.is_primary().unwrap_or(false),
                    scale_factor: m.scale_factor().unwrap_or(1.0) as f64,
                })
            })
            .collect())
    }

    fn list_windows(&self) -> Result<Vec<WindowInfo>, CaptureError> {
        let windows = xcap::Window::all().map_err(map_err)?;
        Ok(windows
            .into_iter()
            .filter_map(|w| {
                Some(WindowInfo {
                    id: w.id().ok()?,
                    title: w.title().unwrap_or_else(|_| String::new()),
                    app_name: w.app_name().unwrap_or_else(|_| String::new()),
                    x: w.x().unwrap_or(0),
                    y: w.y().unwrap_or(0),
                    width: w.width().ok()?,
                    height: w.height().ok()?,
                    is_minimized: w.is_minimized().unwrap_or(false),
                })
            })
            .collect())
    }

    fn resolution(&self) -> (u32, u32) {
        if self.primary_width > 0 {
            (self.primary_width, self.primary_height)
        } else {
            (1920, 1080)
        }
    }
}
