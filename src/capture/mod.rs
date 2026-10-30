//! Continuous screen capture, scaled to the panel in the capture thread.
//!
//! * Linux: xdg-desktop-portal ScreenCast + PipeWire (Wayland; also works on most X11 desktops)
//! * Windows / macOS: xcap (DXGI desktop duplication / AVFoundation)

use crate::device::Frame565;
use crate::scale::{Region, Scaling};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod xcap_backend;

#[derive(Clone, Debug)]
pub struct CaptureOptions {
    pub fit: Scaling,
    pub region: Option<Region>,
    /// Linux: what the share dialog offers (monitor, window or both)
    pub source: Source,
    /// Windows / macOS: monitor index (0 = primary)
    pub monitor: usize,
    /// Upper limit for processed frames per second
    pub max_fps: f32,
    /// Output size (canvas): 160x80, or 80x160 for portrait rotation
    pub size: (usize, usize),
    /// Ask again which screen to share (Linux)
    pub reselect: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    #[default]
    Any,
    Monitor,
    Window,
}

/// Shared between the capture thread and the consumer.
#[derive(Default)]
pub struct Shared {
    pub frame: Mutex<Option<Frame565>>,
    pub seq: AtomicU64,
    pub error: Mutex<Option<String>>,
    /// Bumped by the consumer each time it took a frame; lets the producer skip work.
    pub taken: AtomicU64,
}

impl Shared {
    pub fn publish(&self, f: Frame565) {
        *self.frame.lock().unwrap() = Some(f);
        self.seq.fetch_add(1, Ordering::Release);
    }
    pub fn fail(&self, e: impl ToString) {
        *self.error.lock().unwrap() = Some(e.to_string());
    }
}

pub struct Capture {
    pub shared: Arc<Shared>,
    last_seq: u64,
    stop: Box<dyn FnMut() + Send>,
}

impl Capture {
    pub fn start(opts: CaptureOptions) -> Capture {
        let shared = Arc::new(Shared::default());
        #[cfg(target_os = "linux")]
        let stop = linux::start(opts, shared.clone());
        #[cfg(not(target_os = "linux"))]
        let stop = xcap_backend::start(opts, shared.clone());
        Capture { shared, last_seq: 0, stop }
    }

    /// The newest frame if there is one we have not seen yet.
    pub fn next_frame(&mut self) -> Option<Frame565> {
        let seq = self.shared.seq.load(Ordering::Acquire);
        if seq == self.last_seq {
            return None;
        }
        self.last_seq = seq;
        self.shared.taken.fetch_add(1, Ordering::Release);
        self.shared.frame.lock().unwrap().clone()
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        (self.stop)();
    }
}

#[cfg(target_os = "linux")]
pub fn token_file() -> std::path::PathBuf {
    crate::config::config_dir().join("portal_restore_token")
}

/// Names of the monitors that can be mirrored (Windows / macOS), primary first.
#[cfg(not(target_os = "linux"))]
pub fn monitor_names() -> Vec<String> {
    let mut m = xcap::Monitor::all().unwrap_or_default();
    m.sort_by_key(|m| !m.is_primary().unwrap_or(false));
    m.iter()
        .enumerate()
        .map(|(i, m)| {
            let name = m.friendly_name().or_else(|_| m.name()).unwrap_or_else(|_| format!("Monitor {}", i + 1));
            format!("{} ({}x{})", name, m.width().unwrap_or(0), m.height().unwrap_or(0))
        })
        .collect()
}
