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
    Gif,
    Video,
    /// A YouTube live stream or video
    #[serde(rename = "youtube")]
    YouTube,
    /// The still picture stored on the device
    Album,
    /// The 36-frame animation stored on the device
    Animation,
}

impl Mode {
    /// The modes this build offers (Video and YouTube are optional features)
    pub const ALL: &'static [Mode] = &[
        Mode::Mirror,
        Mode::Stats,
        Mode::Clock,
        Mode::Image,
        Mode::Gif,
        #[cfg(feature = "video")]
        Mode::Video,
        #[cfg(feature = "youtube")]
        Mode::YouTube,
        Mode::Album,
        Mode::Animation,
    ];
    pub fn available(self) -> bool {
        Mode::ALL.contains(&self)
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Mirror => "Screen mirror",
            Mode::Stats => "System monitor",
            Mode::Clock => "Clock",
            Mode::Image => "Image",
            Mode::Gif => "GIF",
            Mode::Video => "Video",
            Mode::YouTube => "YouTube",
            Mode::Album => "Slot1 on device (image)",
            Mode::Animation => "Slot2 on device (image or GIF)",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mode: Mode,
    /// Modes holding the device's touch button cycles through; empty = all
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
    /// Animated GIF (or WebP / APNG) for the GIF mode
    pub gif: Option<PathBuf>,
    /// Screen brightness in percent (10..=100)
    pub brightness: u8,
    /// Play the video's sound
    pub video_sound: bool,
    /// Start the video again at the end (otherwise its last picture stays)
    pub video_loop: bool,
    pub clock_background: Option<PathBuf>,
    /// YouTube mode: address of the live stream or video
    pub youtube_url: Option<String>,
    /// YouTube mode: playing (false = stopped)
    pub youtube_playing: bool,
    /// YouTube mode: play the sound
    pub youtube_sound: bool,
    /// YouTube mode: saved streams and videos
    pub youtube_favorites: Vec<Favorite>,
    /// The last address played that is not a favourite (the touch button steps through it
    /// and the favourites)
    pub youtube_typed: Option<String>,
    /// Serial port of the chosen device; None = the first one found
    pub port: Option<String>,
    /// USB serial number of the chosen device, if it has one (the port name can change)
    pub device_serial: Option<String>,
    pub stats: StatsConfig,
    /// Icon of the control window: taskbar, notification area or both
    pub tray_icon: TrayIcon,
    /// Open the control window minimized (or only in the notification area)
    pub start_minimized: bool,
    /// Which display these settings belong to (0 = the first, saved in config.json)
    #[serde(skip)]
    pub display: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: Mode::Mirror,
            button_modes: Vec::new(),
            rotation: 0,
            flipped: false,
            fit: Scaling::Crop,
            region: None,
            source: Source::Any,
            monitor: 0,
            max_fps: 60.0,
            image: None,
            video: None,
            gif: None,
            brightness: 100,
            video_sound: false,
            video_loop: true,
            clock_background: None,
            youtube_url: None,
            youtube_playing: true,
            youtube_sound: false,
            youtube_favorites: Vec::new(),
            youtube_typed: None,
            port: None,
            device_serial: None,
            stats: StatsConfig::default(),
            tray_icon: TrayIcon::Taskbar,
            start_minimized: false,
            display: 0,
        }
    }
}

/// A saved YouTube address
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    pub name: String,
    pub url: String,
}

/// Lowest brightness setting (percent), so the screen never goes completely dark
pub const MIN_BRIGHTNESS: u8 = 10;

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
    /// Disk values can be shown for a chosen drive
    pub fn has_drive(self) -> bool {
        matches!(self, StatItem::Disk | StatItem::DiskIo)
    }
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
    /// Shown rows, in this order
    pub items: Vec<StatEntry>,
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

/// Which drive the disk values show
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DiskSelect {
    /// The drive the system runs from (`/` or `C:\`)
    #[default]
    System,
    /// All drives together
    All,
    /// The drive mounted here (e.g. `/home` or `D:\`)
    Mount(String),
}

impl DiskSelect {
    /// Short name on the panel, e.g. "HOME" for `/home` or "D:" for `D:\`; None for the
    /// system drive and all drives, which keep the normal label
    pub fn short(&self) -> Option<String> {
        let DiskSelect::Mount(m) = self else { return None };
        let m = m.trim_end_matches(['/', '\\']);
        let name = m.rsplit(['/', '\\']).next().unwrap_or(m);
        (!name.is_empty()).then(|| name.to_uppercase().chars().take(5).collect())
    }
}

/// One row of the system monitor. Disk rows can each show a different drive, so the same
/// value can be shown more than once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "EntryRepr", into = "EntryRepr")]
pub struct StatEntry {
    pub item: StatItem,
    /// Only used by `Disk` and `DiskIo`
    pub drive: DiskSelect,
}

/// Saved as just the value ("cpu") unless a drive is chosen, so older settings still load.
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum EntryRepr {
    Plain(StatItem),
    WithDrive { item: StatItem, drive: DiskSelect },
}

impl From<EntryRepr> for StatEntry {
    fn from(r: EntryRepr) -> Self {
        match r {
            EntryRepr::Plain(item) => StatEntry::new(item),
            EntryRepr::WithDrive { item, drive } => StatEntry { item, drive },
        }
    }
}

impl From<StatEntry> for EntryRepr {
    fn from(e: StatEntry) -> Self {
        if e.drive == DiskSelect::System {
            EntryRepr::Plain(e.item)
        } else {
            EntryRepr::WithDrive { item: e.item, drive: e.drive }
        }
    }
}

impl StatEntry {
    pub fn new(item: StatItem) -> Self {
        StatEntry { item, drive: DiskSelect::System }
    }
    pub fn has_drive(&self) -> bool {
        self.item.has_drive()
    }
    /// Name in the settings dialog
    pub fn name(&self) -> String {
        match (&self.drive, self.has_drive()) {
            (DiskSelect::Mount(m), true) => format!("{}: {m}", self.item.name()),
            (DiskSelect::All, true) => format!("{}: all drives", self.item.name()),
            _ => self.item.name().to_string(),
        }
    }
    /// Label on the panel
    pub fn short(&self) -> String {
        self.has_drive().then(|| self.drive.short()).flatten().unwrap_or_else(|| self.item.short().to_string())
    }
}

impl Default for StatsConfig {
    fn default() -> Self {
        StatsConfig {
            items: [StatItem::Cpu, StatItem::Gpu, StatItem::Ram, StatItem::Net].map(StatEntry::new).to_vec(),
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

/// Most displays (screens) driven at the same time
pub const MAX_DISPLAYS: usize = 16;

/// Settings of a display: config.json for the first, display2.json, display3.json, ... for the others
fn config_file(display: usize) -> PathBuf {
    match display {
        0 => config_dir().join("config.json"),
        n => config_dir().join(format!("display{}.json", n + 1)),
    }
}

impl Config {
    /// Settings of the first display
    pub fn load() -> Config {
        Self::load_display(0)
    }

    /// Settings of every display: the saved ones, plus new ones up to `at_least` (e.g. the
    /// number of screens connected). New ones are saved once something is changed.
    pub fn load_displays(at_least: usize) -> Vec<Config> {
        let saved = (1..MAX_DISPLAYS).filter(|i| config_file(*i).exists()).max().map_or(1, |i| i + 1);
        let mut list = vec![Self::load_display(0)];
        for i in 1..saved.max(at_least).min(MAX_DISPLAYS) {
            // one in between may never have been changed (and saved)
            let c = if config_file(i).exists() { Self::load_display(i) } else { Self::new_display(&list[0], i) };
            list.push(c);
        }
        list
    }

    /// Settings for another display. The window settings are shared by all displays; the
    /// mode is the clock (screen mirroring would ask again which screen to share).
    pub fn new_display(first: &Config, display: usize) -> Config {
        Config {
            display,
            mode: Mode::Clock,
            tray_icon: first.tray_icon,
            start_minimized: first.start_minimized,
            youtube_favorites: first.youtube_favorites.clone(),
            stats: first.stats.clone(),
            ..Default::default()
        }
    }

    fn load_display(display: usize) -> Config {
        let mut c: Config = std::fs::read_to_string(config_file(display))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        c.display = display;
        if c.flipped {
            c.rotation = 180;
            c.flipped = false;
        }
        c.rotation = normalize_rotation(c.rotation);
        c.max_fps = c.max_fps.clamp(0.1, 1000.0);
        c.stats.migrate();
        c.brightness = c.brightness.clamp(MIN_BRIGHTNESS, 100);
        // older settings kept only the address playing
        if c.youtube_typed.is_none() {
            if let Some(u) = c.youtube_url.clone() {
                c.set_youtube_url(u);
            }
        }
        // a mode this build does not have (settings from a build with more features)
        if !c.mode.available() {
            c.mode = Mode::default();
        }
        // the old default list left out Image and Video: use all modes instead
        if c.button_modes == [Mode::Mirror, Mode::Stats, Mode::Clock, Mode::Album, Mode::Animation] {
            c.button_modes.clear();
        }
        // before 1.1.2 one mode showed pictures and GIFs: move a GIF to the new GIF mode
        let is_gif = |p: &PathBuf| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif"));
        if c.gif.is_none() && c.image.as_ref().is_some_and(is_gif) {
            c.gif = c.image.take();
            if c.mode == Mode::Image {
                c.mode = Mode::Gif;
            }
        }
        c
    }

    /// Play this YouTube address next; one that is not a favourite is kept as the typed one.
    pub fn set_youtube_url(&mut self, url: String) {
        if !self.youtube_favorites.iter().any(|f| f.url == url) {
            self.youtube_typed = Some(url.clone());
        }
        self.youtube_url = Some(url);
    }

    /// The YouTube addresses the touch button steps through: the typed one, then the favourites
    pub fn youtube_entries(&self) -> Vec<String> {
        let mut list: Vec<String> = self.youtube_typed.iter().cloned().collect();
        for f in &self.youtube_favorites {
            if !list.contains(&f.url) {
                list.push(f.url.clone());
            }
        }
        list
    }

    /// Name of a favourite address
    #[cfg_attr(not(feature = "youtube"), allow(dead_code))]
    pub fn youtube_name(&self, url: &str) -> Option<String> {
        self.youtube_favorites.iter().find(|f| f.url == url).map(|f| f.name.clone())
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(config_dir());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(config_file(self.display), s);
        }
    }
}

// -- background instance bookkeeping ----------------------------------------------

fn pid_file() -> PathBuf {
    config_dir().join("background.pid")
}

/// PID of a running `open-msu2-mini run` instance, if any.
pub fn background_pid() -> Option<u32> {
    running_pid(&pid_file())
}

fn gui_pid_file() -> PathBuf {
    config_dir().join("window.pid")
}

/// PID of an open control window, if any.
pub fn gui_pid() -> Option<u32> {
    running_pid(&gui_pid_file()).filter(|p| *p != std::process::id())
}

/// Note that the control window is open (or closed again).
pub fn set_gui_pid(open: bool) {
    let _ = std::fs::create_dir_all(config_dir());
    if open {
        let _ = std::fs::write(gui_pid_file(), std::process::id().to_string());
    } else if running_pid(&gui_pid_file()) == Some(std::process::id()) {
        let _ = std::fs::remove_file(gui_pid_file());
    }
}

/// The program's PID in this file, if it still runs.
fn running_pid(file: &std::path::Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(file).ok()?.trim().parse().ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_display_has_its_own_settings_file() {
        assert!(config_file(0).ends_with("config.json"));
        assert!(config_file(1).ends_with("display2.json"));
        let first = Config {
            tray_icon: TrayIcon::Tray,
            mode: Mode::Mirror,
            port: Some("/dev/ttyACM0".into()),
            ..Default::default()
        };
        let second = Config::new_display(&first, 1);
        assert_eq!(second.display, 1);
        assert_eq!(second.tray_icon, TrayIcon::Tray, "window settings are shared");
        assert_eq!(second.port, None, "a new display takes any free screen");
        assert_eq!(second.mode, Mode::Clock);
        // the display number is not stored in the file
        assert!(!serde_json::to_string(&second).unwrap().contains("\"display\""));
    }

    #[test]
    fn stat_rows_with_drives_load_and_save() {
        // settings from before drives could be chosen
        let old: StatsConfig = serde_json::from_str(r#"{"items":["cpu","disk","disk_io"]}"#).unwrap();
        assert_eq!(old.items, [StatItem::Cpu, StatItem::Disk, StatItem::DiskIo].map(StatEntry::new));

        let cfg = StatsConfig {
            items: vec![
                StatEntry { item: StatItem::DiskIo, drive: DiskSelect::Mount("/".into()) },
                StatEntry { item: StatItem::Disk, drive: DiskSelect::Mount("/mnt/backup".into()) },
                StatEntry { item: StatItem::Disk, drive: DiskSelect::All },
                StatEntry::new(StatItem::Ram),
            ],
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""ram""#), "plain rows stay plain: {json}");
        let back: StatsConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items, cfg.items);

        assert_eq!(cfg.items[1].short(), "BACKU");
        assert_eq!(cfg.items[0].short(), "I/O");
        assert_eq!(DiskSelect::Mount("D:\\".into()).short().as_deref(), Some("D:"));
    }
}
