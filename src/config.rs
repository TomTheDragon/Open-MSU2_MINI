use crate::capture::Source;
use crate::scale::Scaling;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Mirror,
    Stats,
    Clock,
    Image,
    Video,
    Album,
    Animation,
}

impl Mode {
    pub const ALL: [Mode; 7] =
        [Mode::Mirror, Mode::Stats, Mode::Clock, Mode::Image, Mode::Video, Mode::Album, Mode::Animation];
    pub fn label(self) -> &'static str {
        match self {
            Mode::Mirror => "Screen mirror",
            Mode::Stats => "System monitor",
            Mode::Clock => "Clock",
            Mode::Image => "Image / GIF",
            Mode::Video => "Video",
            Mode::Album => "Album (stored on device)",
            Mode::Animation => "Animation (stored on device)",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mode: Mode,
    pub button_modes: Vec<Mode>,
    /// Picture rotation: 0, 90, 180 or 270 degrees (clockwise)
    pub rotation: u16,
    /// Old setting (before rotation existed); read once and converted
    #[serde(skip_serializing)]
    pub flipped: bool,
    pub fit: Scaling,
    /// Mirror only this area: [x, y, w, h] in pixels of the shared screen
    pub region: Option<[u32; 4]>,
    pub source: Source,
    /// Windows / macOS: which monitor to mirror (0 = primary)
    pub monitor: usize,
    pub max_fps: f32,
    pub image: Option<PathBuf>,
    /// Video for the video mode
    pub video: Option<PathBuf>,
    /// Play the video's sound
    pub video_sound: bool,
    /// Start the video again at the end (otherwise its last picture stays)
    pub video_loop: bool,
    pub clock_background: Option<PathBuf>,
    pub port: Option<String>,
    pub stats: StatsConfig,
    /// Icon of the control window: taskbar, notification area or both
    pub tray_icon: TrayIcon,
    /// Open the control window minimized (or only in the notification area)
    pub start_minimized: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: Mode::Mirror,
            button_modes: vec![Mode::Mirror, Mode::Stats, Mode::Clock, Mode::Album, Mode::Animation],
            rotation: 0,
            flipped: false,
            fit: Scaling::Crop,
            region: None,
            source: Source::Any,
            monitor: 0,
            max_fps: 60.0,
            image: None,
            video: None,
            video_sound: false,
            video_loop: true,
            clock_background: None,
            port: None,
            stats: StatsConfig::default(),
            tray_icon: TrayIcon::Taskbar,
            start_minimized: false,
        }
    }
}

pub fn normalize_rotation(r: u16) -> u16 {
    ((r as u32 % 360) / 90 * 90) as u16
}

// -- frame-rate choices ---------------------------------------------------------------

pub const FPS_CHOICES: [f32; 13] = [0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 20.0, 24.0, 30.0, 45.0, 60.0, 90.0, 120.0];

/// How well a frame-rate limit fits the device (USB ~890 KB/s, LCD ~61 full frames/s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FpsClass {
    /// Below what the device can do; saves CPU and USB bandwidth
    Saver,
    /// Reached even when the whole screen changes (e.g. full-screen video)
    Full,
    /// Reached only when parts of the screen change; full-screen changes stay at ~25 fps
    Partial,
    /// Above the panel's full-frame rate; only small changes reach it, more CPU
    SmallOnly,
}

pub fn fps_class(fps: f32) -> FpsClass {
    if fps < 10.0 {
        FpsClass::Saver
    } else if fps <= 24.0 {
        FpsClass::Full
    } else if fps <= 60.0 {
        FpsClass::Partial
    } else {
        FpsClass::SmallOnly
    }
}

impl FpsClass {
    pub fn mark(self) -> &'static str {
        match self {
            FpsClass::Saver => "eco",
            FpsClass::Full => "✓ full screen",
            FpsClass::Partial => "◐ partial changes",
            FpsClass::SmallOnly => "⚠ small changes only",
        }
    }
    pub fn explanation(self) -> &'static str {
        match self {
            FpsClass::Saver => "Saves CPU and USB bandwidth. Good for slow content; motion looks choppy.",
            FpsClass::Full => "Reached for any content, even when the whole screen changes (full-screen video).",
            FpsClass::Partial => {
                "Reached when only parts of the screen change. Full-screen changes stay at about 25 fps (USB limit)."
            }
            FpsClass::SmallOnly => {
                "Above the panel's full-frame limit (~61 fps): only small changes like the mouse reach it. Uses more CPU."
            }
        }
    }
}

pub fn fps_label(fps: f32) -> String {
    let n = if fps.fract() == 0.0 { format!("{fps:.0}") } else { format!("{fps}") };
    format!("{n} fps   {}", fps_class(fps).mark())
}

// -- system monitor ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatItem {
    Cpu,
    CpuTemp,
    CpuFreq,
    CpuCores,
    Gpu,
    GpuTemp,
    Vram,
    Ram,
    Swap,
    Disk,
    DiskIo,
    Net,
    Battery,
    Load,
    Processes,
    Uptime,
    Time,
}

impl StatItem {
    pub const ALL: [StatItem; 17] = [
        StatItem::Cpu,
        StatItem::CpuTemp,
        StatItem::CpuFreq,
        StatItem::CpuCores,
        StatItem::Gpu,
        StatItem::GpuTemp,
        StatItem::Vram,
        StatItem::Ram,
        StatItem::Swap,
        StatItem::Disk,
        StatItem::DiskIo,
        StatItem::Net,
        StatItem::Battery,
        StatItem::Load,
        StatItem::Processes,
        StatItem::Uptime,
        StatItem::Time,
    ];
    /// Name in the settings dialog
    pub fn name(self) -> &'static str {
        match self {
            StatItem::Cpu => "CPU usage",
            StatItem::CpuTemp => "CPU temperature",
            StatItem::CpuFreq => "CPU clock speed",
            StatItem::CpuCores => "CPU usage per core",
            StatItem::Gpu => "GPU usage",
            StatItem::GpuTemp => "GPU temperature",
            StatItem::Vram => "GPU memory (VRAM)",
            StatItem::Ram => "Memory (RAM)",
            StatItem::Swap => "Swap",
            StatItem::Disk => "Disk space",
            StatItem::DiskIo => "Disk read/write speed",
            StatItem::Net => "Network down/up",
            StatItem::Battery => "Battery",
            StatItem::Load => "System load (Linux/macOS)",
            StatItem::Processes => "Number of processes",
            StatItem::Uptime => "Uptime",
            StatItem::Time => "Time",
        }
    }
    /// Short label on the panel
    pub fn short(self) -> &'static str {
        match self {
            StatItem::Cpu => "CPU",
            StatItem::CpuTemp => "CPU°",
            StatItem::CpuFreq => "CLK",
            StatItem::CpuCores => "CORE",
            StatItem::Gpu => "GPU",
            StatItem::GpuTemp => "GPU°",
            StatItem::Vram => "VRAM",
            StatItem::Ram => "RAM",
            StatItem::Swap => "SWAP",
            StatItem::Disk => "DISK",
            StatItem::DiskIo => "I/O",
            StatItem::Net => "NET",
            StatItem::Battery => "BAT",
            StatItem::Load => "LOAD",
            StatItem::Processes => "PROC",
            StatItem::Uptime => "UP",
            StatItem::Time => "TIME",
        }
    }
}

/// Old setting (before graphic and value position were separate); read once and converted
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatStyle {
    Bars,
    Graphs,
    Text,
}

/// Graphic next to each value
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StatGraphic {
    None,
    /// Bar showing the current value
    #[default]
    Bars,
    /// Column graph of the last minutes
    Histogram,
}

impl StatGraphic {
    pub const ALL: [StatGraphic; 3] = [StatGraphic::None, StatGraphic::Bars, StatGraphic::Histogram];
    pub fn name(self) -> &'static str {
        match self {
            StatGraphic::None => "None",
            StatGraphic::Bars => "Bars",
            StatGraphic::Histogram => "Histogram (history)",
        }
    }
}

/// Where the value (e.g. "45%") goes relative to the graphic
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ValuePosition {
    None,
    Before,
    #[default]
    Behind,
    OnTop,
}

impl ValuePosition {
    pub const ALL: [ValuePosition; 4] =
        [ValuePosition::None, ValuePosition::Before, ValuePosition::Behind, ValuePosition::OnTop];
    pub fn name(self) -> &'static str {
        match self {
            ValuePosition::None => "None",
            ValuePosition::Before => "Before the graphic",
            ValuePosition::Behind => "Behind the graphic",
            ValuePosition::OnTop => "On top of the graphic",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatsConfig {
    /// Shown items, in this order
    pub items: Vec<StatItem>,
    pub graphic: StatGraphic,
    pub value_position: ValuePosition,
    /// Grey frame around bars and histograms
    pub outline: bool,
    /// Space in pixels between the labels (CPU, RAM, ...) and what follows them
    pub label_gap: u32,
    /// Old setting; converted by `migrate`
    #[serde(skip_serializing)]
    pub style: Option<StatStyle>,
    /// Rows per page; 0 = as many as fit
    pub rows: u32,
    /// Seconds per page when there are more items than rows
    pub page_seconds: u32,
    /// Update interval in milliseconds
    pub interval_ms: u32,
    /// Colour of the labels
    pub accent: [u8; 3],
    /// Colour bars and histograms green / yellow / red by value (otherwise the label colour)
    pub load_colors: bool,
    /// Show the temperature next to CPU and GPU usage
    pub temp_with_usage: bool,
    pub fahrenheit: bool,
}

impl Default for StatsConfig {
    fn default() -> Self {
        StatsConfig {
            items: vec![StatItem::Cpu, StatItem::Gpu, StatItem::Ram, StatItem::Net],
            graphic: StatGraphic::Bars,
            value_position: ValuePosition::Behind,
            outline: true,
            label_gap: 6,
            style: None,
            rows: 0,
            page_seconds: 5,
            interval_ms: 1000,
            accent: [80, 160, 255],
            load_colors: true,
            temp_with_usage: true,
            fahrenheit: false,
        }
    }
}

impl StatsConfig {
    /// Convert the old combined `style` setting.
    pub(crate) fn migrate(&mut self) {
        match self.style.take() {
            Some(StatStyle::Bars) => self.graphic = StatGraphic::Bars,
            Some(StatStyle::Graphs) => {
                self.graphic = StatGraphic::Histogram;
                self.outline = false;
            }
            Some(StatStyle::Text) => {
                self.graphic = StatGraphic::None;
                self.value_position = ValuePosition::Behind;
            }
            None => {}
        }
    }
}

// -- window --------------------------------------------------------------------------

/// Where the control window shows its icon
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrayIcon {
    /// Taskbar only (normal window)
    #[default]
    Taskbar,
    /// Notification area only; the minimized window disappears from the taskbar
    Tray,
    Both,
}

impl TrayIcon {
    pub const ALL: [TrayIcon; 3] = [TrayIcon::Taskbar, TrayIcon::Tray, TrayIcon::Both];
    pub fn name(self) -> &'static str {
        match self {
            TrayIcon::Taskbar => "Taskbar",
            TrayIcon::Tray => "Notification area",
            TrayIcon::Both => "Taskbar and notification area",
        }
    }
    pub fn in_tray(self) -> bool {
        self != TrayIcon::Taskbar
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("open-msu2-mini")
}

fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

impl Config {
    pub fn load() -> Config {
        let mut c: Config =
            std::fs::read_to_string(config_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if c.flipped {
            c.rotation = 180;
            c.flipped = false;
        }
        c.rotation = normalize_rotation(c.rotation);
        c.max_fps = c.max_fps.clamp(0.1, 1000.0);
        c.stats.migrate();
        c
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(config_dir());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(config_file(), s);
        }
    }
}

// -- background instance bookkeeping ----------------------------------------------

fn pid_file() -> PathBuf {
    config_dir().join("background.pid")
}

/// PID of a running `open-msu2-mini run` instance, if any.
pub fn background_pid() -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_file()).ok()?.trim().parse().ok()?;
    let mut sys = sysinfo::System::new().ok()?;
    let p = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[p]), true);
    let proc_ = sys.process(p)?;
    proc_.name().to_string_lossy().contains("open-msu2-mini").then_some(pid)
}

pub fn write_pid() {
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(pid_file(), std::process::id().to_string());
}

pub fn remove_pid() {
    if background_pid() == Some(std::process::id()) {
        let _ = std::fs::remove_file(pid_file());
    }
}

pub fn kill_background(pid: u32) {
    let Ok(mut sys) = sysinfo::System::new() else { return };
    let p = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[p]), true);
    if let Some(proc_) = sys.process(p) {
        // SIGTERM on Unix lets it clean up; Windows has no signals, so it is ended directly.
        if proc_.kill_with(sysinfo::Signal::Term).is_none() {
            proc_.kill();
        }
    }
    for _ in 0..30 {
        if background_pid().is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
