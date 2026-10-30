//! Customisable system monitor.

use crate::config::{StatItem, StatStyle, StatsConfig};
use crate::device::Frame565;
use crate::render::{blank, bold, regular, text, text_width, Anchor, Screen, Size, Style};
use crate::scale::rgb_to_565;
use image::{Rgb, RgbImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_hollow_rect_mut};
use imageproc::rect::Rect;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use sysinfo::{Components, Disks, Gpus, Networks, ProcessesToUpdate, System};

const HISTORY: usize = 160;

#[derive(Default, Clone, Copy)]
struct GpuInfo {
    usage: Option<f32>,
    temp: Option<f32>,
    vram: Option<(u64, u64)>,
}

/// sysinfo's GPU list holds a raw driver handle, so it is not `Send`. The screen is only
/// ever used by the controller thread, one call at a time.
struct GpuList(Gpus);
unsafe impl Send for GpuList {}

/// One line on the panel.
struct Row {
    item: StatItem,
    value: String,
    /// 0..1 for bars and graphs
    frac: Option<f32>,
    /// several small bars (CPU cores)
    multi: Option<Vec<f32>>,
}

pub struct StatsScreen {
    cfg: StatsConfig,
    size: Size,
    sys: System,
    comps: Components,
    nets: Networks,
    disks: Disks,
    gpus: Option<GpuList>,
    amd_gpu: Option<PathBuf>,
    nvidia: bool,
    last: Instant,
    started: Instant,
    history: HashMap<StatItem, VecDeque<f32>>,
}

impl StatsScreen {
    pub fn new(cfg: StatsConfig, size: Size) -> Self {
        let mut sys = System::new().expect("system information not available");
        sys.refresh_cpu_usage();
        let wants_gpu = cfg.items.iter().any(|i| matches!(i, StatItem::Gpu | StatItem::GpuTemp | StatItem::Vram));
        let amd_gpu = if wants_gpu { amd_gpu_dir() } else { None };
        let gpus =
            (wants_gpu && amd_gpu.is_none()).then(|| Gpus::new_with_refreshed_list().ok().map(GpuList)).flatten();
        let nvidia = wants_gpu
            && amd_gpu.is_none()
            && gpus.as_ref().is_none_or(|g| g.0.iter().all(|g| g.usage().is_none()))
            && nvidia_smi().is_some();
        StatsScreen {
            cfg,
            size,
            sys,
            comps: Components::new_with_refreshed_list().unwrap_or_else(|_| Components::new().unwrap()),
            nets: Networks::new_with_refreshed_list().unwrap_or_else(|_| Networks::new().unwrap()),
            disks: Disks::new_with_refreshed_list().unwrap_or_else(|_| Disks::new().unwrap()),
            gpus,
            amd_gpu,
            nvidia,
            last: Instant::now(),
            started: Instant::now(),
            history: HashMap::new(),
        }
    }

    fn temp(&self, needles: &[&str]) -> Option<f32> {
        for n in needles {
            for c in self.comps.iter() {
                if c.label().to_lowercase().contains(n) {
                    if let Some(t) = c.temperature().filter(|t| *t > 0.0 && *t < 150.0) {
                        return Some(t);
                    }
                }
            }
        }
        None
    }

    fn cpu_temp(&self) -> Option<f32> {
        self.temp(&["tctl", "tdie", "package id", "cpu", "k10temp", "coretemp", "core 0"])
    }

    fn gpu(&mut self) -> GpuInfo {
        let temp = self.temp(&["amdgpu edge", "amdgpu", "edge", "nouveau", "radeon", "gpu"]);
        if let Some(dir) = &self.amd_gpu {
            let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok().and_then(|s| s.trim().parse::<u64>().ok());
            let vram = read("mem_info_vram_used").zip(read("mem_info_vram_total"));
            return GpuInfo { usage: read("gpu_busy_percent").map(|v| v as f32), temp, vram };
        }
        if let Some(GpuList(g)) = self.gpus.as_mut() {
            g.refresh(false);
            if let Some(gpu) = g.iter().find(|g| g.usage().is_some()) {
                let vram = gpu.used_memory().zip(gpu.total_memory());
                return GpuInfo { usage: gpu.usage(), temp, vram };
            }
        }
        if self.nvidia {
            if let Some(n) = nvidia_smi() {
                return n;
            }
        }
        GpuInfo { temp, ..Default::default() }
    }

    fn deg(&self, t: f32) -> String {
        if self.cfg.fahrenheit {
            format!("{:.0}°F", t * 9.0 / 5.0 + 32.0)
        } else {
            format!("{t:.0}°")
        }
    }

    fn collect(&mut self) -> Vec<Row> {
        let items = self.cfg.items.clone();
        let has = |i: StatItem| items.contains(&i);
        let dt = self.last.elapsed().as_secs_f64().max(0.1);
        self.last = Instant::now();

        self.sys.refresh_cpu_usage();
        if has(StatItem::CpuFreq) {
            self.sys.refresh_cpu_frequency();
        }
        if has(StatItem::Ram) || has(StatItem::Swap) {
            self.sys.refresh_memory();
        }
        if items.iter().any(|i| matches!(i, StatItem::Cpu | StatItem::CpuTemp | StatItem::Gpu | StatItem::GpuTemp)) {
            self.comps.refresh(false);
        }
        if has(StatItem::Net) {
            self.nets.refresh(false);
        }
        if has(StatItem::Disk) || has(StatItem::DiskIo) {
            self.disks.refresh(false);
        }
        if has(StatItem::Processes) {
            self.sys.refresh_processes(ProcessesToUpdate::All, true);
        }
        let gpu = if items.iter().any(|i| matches!(i, StatItem::Gpu | StatItem::GpuTemp | StatItem::Vram)) {
            self.gpu()
        } else {
            GpuInfo::default()
        };
        let with_temp = self.cfg.temp_with_usage;

        let mut rows = Vec::new();
        for item in items {
            let mut row = Row { item, value: "n/a".into(), frac: None, multi: None };
            match item {
                StatItem::Cpu => {
                    let u = self.sys.global_cpu_usage();
                    row.value = format!("{u:.0}%");
                    if let Some(t) = self.cpu_temp().filter(|_| with_temp) {
                        row.value += &format!(" {}", self.deg(t));
                    }
                    row.frac = Some(u / 100.0);
                }
                StatItem::CpuTemp => {
                    if let Some(t) = self.cpu_temp() {
                        row.value = self.deg(t);
                        row.frac = Some(t / 100.0);
                    }
                }
                StatItem::CpuFreq => {
                    let cpus = self.sys.cpus();
                    if !cpus.is_empty() {
                        let mhz = cpus.iter().map(|c| c.frequency()).sum::<u64>() as f32 / cpus.len() as f32;
                        row.value = format!("{:.2} GHz", mhz / 1000.0);
                    }
                }
                StatItem::CpuCores => {
                    let v: Vec<f32> = self.sys.cpus().iter().map(|c| c.usage() / 100.0).collect();
                    let max = v.iter().cloned().fold(0.0, f32::max);
                    row.value = format!("{} ↑{:.0}%", v.len(), max * 100.0);
                    row.multi = Some(v);
                }
                StatItem::Gpu => {
                    if let Some(u) = gpu.usage {
                        row.value = format!("{u:.0}%");
                        if let Some(t) = gpu.temp.filter(|_| with_temp) {
                            row.value += &format!(" {}", self.deg(t));
                        }
                        row.frac = Some(u / 100.0);
                    }
                }
                StatItem::GpuTemp => {
                    if let Some(t) = gpu.temp {
                        row.value = self.deg(t);
                        row.frac = Some(t / 100.0);
                    }
                }
                StatItem::Vram => {
                    if let Some((used, total)) = gpu.vram.filter(|v| v.1 > 0) {
                        row.value = fmt_used(used, total);
                        row.frac = Some(used as f32 / total as f32);
                    }
                }
                StatItem::Ram => {
                    let (used, total) = (self.sys.used_memory(), self.sys.total_memory());
                    if total > 0 {
                        row.value = fmt_used(used, total);
                        row.frac = Some(used as f32 / total as f32);
                    }
                }
                StatItem::Swap => {
                    let (used, total) = (self.sys.used_swap(), self.sys.total_swap());
                    if total > 0 {
                        row.value = fmt_used(used, total);
                        row.frac = Some(used as f32 / total as f32);
                    } else {
                        row.value = "none".into();
                    }
                }
                StatItem::Disk => {
                    if let Some(d) = main_disk(&self.disks) {
                        let used = d.total_space() - d.available_space();
                        row.value = fmt_used(used, d.total_space());
                        row.frac = Some(used as f32 / d.total_space().max(1) as f32);
                    }
                }
                StatItem::DiskIo => {
                    let (r, w) = self
                        .disks
                        .iter()
                        .fold((0u64, 0u64), |a, d| (a.0 + d.usage().read_bytes, a.1 + d.usage().written_bytes));
                    row.value = format!("R{} W{}", fmt_rate(r as f64 / dt), fmt_rate(w as f64 / dt));
                }
                StatItem::Net => {
                    let (d, u) =
                        self.nets.iter().fold((0u64, 0u64), |a, (_, n)| (a.0 + n.received(), a.1 + n.transmitted()));
                    row.value = format!("↓{} ↑{}", fmt_rate(d as f64 / dt), fmt_rate(u as f64 / dt));
                }
                StatItem::Battery => {
                    if let Some((pct, charging)) = battery() {
                        row.value = format!("{}{pct:.0}%", if charging { "⚡" } else { "" });
                        row.frac = Some(pct / 100.0);
                    } else {
                        row.value = "none".into();
                    }
                }
                StatItem::Load => {
                    if !cfg!(windows) {
                        if let Ok(l) = System::load_average() {
                            row.value = format!("{:.2}", l.one);
                            row.frac = Some((l.one as f32 / self.sys.cpus().len().max(1) as f32).min(1.0));
                        }
                    }
                }
                StatItem::Processes => row.value = self.sys.processes().len().to_string(),
                StatItem::Uptime => {
                    if let Ok(s) = System::uptime() {
                        let (d, h, m) = (s / 86400, s / 3600 % 24, s / 60 % 60);
                        row.value = if d > 0 { format!("{d}d {h}h") } else { format!("{h}h {m:02}m") };
                    }
                }
                StatItem::Time => row.value = chrono::Local::now().format("%H:%M:%S").to_string(),
            }
            if let Some(f) = row.frac {
                let h = self.history.entry(item).or_default();
                h.push_back(f.clamp(0.0, 1.0));
                while h.len() > HISTORY {
                    h.pop_front();
                }
            }
            rows.push(row);
        }
        rows
    }

    fn color(&self, frac: f32) -> [u8; 3] {
        if !self.cfg.load_colors {
            return self.cfg.accent;
        }
        if frac < 0.6 {
            [60, 200, 120]
        } else if frac < 0.85 {
            [240, 180, 40]
        } else {
            [240, 70, 60]
        }
    }

    /// Bar, graph or core bars inside `area` (x, y, w, h).
    fn draw_viz(&self, img: &mut RgbImage, row: &Row, area: (i32, i32, i32, i32)) {
        let (x, y, w, h) = area;
        if w < 4 || h < 2 {
            return;
        }
        if let Some(cores) = &row.multi {
            let n = cores.len().max(1) as i32;
            let gap = if w / n >= 3 { 1 } else { 0 };
            for (i, c) in cores.iter().enumerate() {
                let x0 = x + i as i32 * w / n;
                let x1 = x + (i as i32 + 1) * w / n - gap;
                let bh = ((h as f32) * c.clamp(0.0, 1.0)).round().max(1.0) as i32;
                if x1 > x0 {
                    draw_filled_rect_mut(
                        img,
                        Rect::at(x0, y + h - bh).of_size((x1 - x0) as u32, bh as u32),
                        Rgb(self.color(*c)),
                    );
                }
            }
            return;
        }
        let Some(frac) = row.frac else { return };
        match self.cfg.style {
            StatStyle::Text => {}
            StatStyle::Bars => {
                draw_hollow_rect_mut(img, Rect::at(x, y).of_size(w as u32, h as u32), Rgb([70, 70, 70]));
                let fill = ((w - 2) as f32 * frac.clamp(0.0, 1.0)) as u32;
                if fill > 0 && h > 2 {
                    draw_filled_rect_mut(
                        img,
                        Rect::at(x + 1, y + 1).of_size(fill, (h - 2) as u32),
                        Rgb(self.color(frac)),
                    );
                }
            }
            StatStyle::Graphs => {
                draw_filled_rect_mut(img, Rect::at(x, y).of_size(w as u32, h as u32), Rgb([25, 25, 30]));
                if let Some(hist) = self.history.get(&row.item) {
                    let skip = hist.len().saturating_sub(w as usize);
                    let start = x + w - (hist.len() - skip) as i32;
                    for (i, v) in hist.iter().skip(skip).enumerate() {
                        let bh = ((h as f32) * v).round().max(1.0) as i32;
                        draw_filled_rect_mut(
                            img,
                            Rect::at(start + i as i32, y + h - bh).of_size(1, bh as u32),
                            Rgb(self.color(*v)),
                        );
                    }
                }
            }
        }
    }
}

impl Screen for StatsScreen {
    fn render(&mut self) -> (Frame565, Duration) {
        let rows = self.collect();
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let narrow = w < h;
        let mut img = blank(self.size);
        let interval = Duration::from_millis(self.cfg.interval_ms.max(250) as u64);
        if rows.is_empty() {
            return (rgb_to_565(&img), interval);
        }
        let auto_rows = if narrow { 8 } else { 4 };
        let per_page = if self.cfg.rows > 0 { self.cfg.rows as usize } else { auto_rows };
        let pages = rows.len().div_ceil(per_page);
        let page = if pages > 1 {
            (self.started.elapsed().as_secs() / self.cfg.page_seconds.max(1) as u64) as usize % pages
        } else {
            0
        };
        let visible = &rows[page * per_page..rows.len().min((page + 1) * per_page)];
        let slots = if pages > 1 { per_page } else { rows.len() } as i32;
        let rh = h / slots;
        let accent = self.cfg.accent;

        for (i, row) in visible.iter().enumerate() {
            let y0 = i as i32 * rh;
            if narrow {
                // label and value on one line, bar/graph underneath
                let fs = (rh as f32 * 0.45).clamp(8.0, 13.0);
                let ty = y0 + (rh as f32 * 0.32) as i32;
                text(&mut img, row.item.short(), 1, ty, Style::new(fs, bold(), accent));
                let vfs = shrink_to_fit(&row.value, fs * 0.95, w - 4 - text_width(row.item.short(), fs, bold()));
                text(
                    &mut img,
                    &row.value,
                    w - 1,
                    ty,
                    Style::new(vfs, regular(), [230, 230, 230]).anchor(Anchor::Right),
                );
                let vy = y0 + (rh as f32 * 0.62) as i32;
                self.draw_viz(&mut img, row, (1, vy, w - 2, (y0 + rh - 2 - vy).max(2)));
            } else {
                let fs = (rh as f32 * 0.62).clamp(8.0, 24.0);
                let cy = y0 + rh / 2;
                let label_w = text_width("VRAM", fs, bold()) + 4;
                text(&mut img, row.item.short(), 2, cy, Style::new(fs, bold(), accent));
                let has_viz = row.multi.is_some() || (row.frac.is_some() && self.cfg.style != StatStyle::Text);
                let max_value_w = if has_viz { w * 2 / 5 } else { w - label_w - 6 };
                let vfs = shrink_to_fit(&row.value, fs * 0.85, max_value_w);
                let vw = text_width(&row.value, vfs, regular());
                text(
                    &mut img,
                    &row.value,
                    w - 2,
                    cy,
                    Style::new(vfs, regular(), [230, 230, 230]).anchor(Anchor::Right),
                );
                let bh = (rh - 8).clamp(4, if self.cfg.style == StatStyle::Graphs { rh - 4 } else { 12 });
                let x0 = label_w + 2;
                self.draw_viz(&mut img, row, (x0, cy - bh / 2, w - 2 - vw - 5 - x0, bh));
            }
        }
        if pages > 1 {
            // page dots, bottom right
            for p in 0..pages as i32 {
                let c = if p as usize == page { accent } else { [70, 70, 70] };
                draw_filled_rect_mut(
                    &mut img,
                    Rect::at(w - 3 - (pages as i32 - 1 - p) * 4, h - 3).of_size(2, 2),
                    Rgb(c),
                );
            }
        }
        (rgb_to_565(&img), interval)
    }
}

/// Reduce the font size until the text fits into `max_w` pixels.
fn shrink_to_fit(s: &str, mut size: f32, max_w: i32) -> f32 {
    while size > 7.0 && text_width(s, size, regular()) > max_w {
        size -= 0.5;
    }
    size
}

fn fmt_used(used: u64, total: u64) -> String {
    let g = |b: u64| b as f64 / (1u64 << 30) as f64;
    let f = |v: f64| if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") };
    if total >= 1 << 40 {
        let t = |b: u64| b as f64 / (1u64 << 40) as f64;
        return format!("{}/{}T", f(t(used)), f(t(total)));
    }
    format!("{}/{}G", f(g(used)), f(g(total)))
}

fn fmt_rate(mut bps: f64) -> String {
    for unit in ["B", "K", "M", "G"] {
        if bps < 1000.0 || unit == "G" {
            return if bps >= 10.0 || unit == "B" { format!("{bps:.0}{unit}") } else { format!("{bps:.1}{unit}") };
        }
        bps /= 1024.0;
    }
    unreachable!()
}

fn main_disk(disks: &Disks) -> Option<&sysinfo::Disk> {
    let root = if cfg!(windows) { "C:\\" } else { "/" };
    disks
        .iter()
        .find(|d| d.mount_point().to_string_lossy() == root)
        .or_else(|| disks.iter().max_by_key(|d| d.total_space()))
}

/// AMD GPUs on Linux report load and VRAM in sysfs.
fn amd_gpu_dir() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("card") && !n.contains('-')
        })
        .map(|e| e.path().join("device"))
        .filter(|p| p.join("gpu_busy_percent").exists())
        .collect();
    dirs.sort();
    dirs.into_iter().next()
}

fn nvidia_smi() -> Option<GpuInfo> {
    let mut cmd = std::process::Command::new("nvidia-smi");
    cmd.args(["--query-gpu=utilization.gpu,temperature.gpu,memory.used,memory.total", "--format=csv,noheader,nounits"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let v: Vec<f64> = s.lines().next()?.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    if v.len() < 4 {
        return None;
    }
    let mib = 1u64 << 20;
    Some(GpuInfo {
        usage: Some(v[0] as f32),
        temp: Some(v[1] as f32),
        vram: Some((v[2] as u64 * mib, v[3] as u64 * mib)),
    })
}

/// Battery charge in percent and whether it is charging.
fn battery() -> Option<(f32, bool)> {
    #[cfg(target_os = "linux")]
    {
        for e in std::fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
            let p = e.path();
            if std::fs::read_to_string(p.join("type")).map(|t| t.trim() == "Battery").unwrap_or(false) {
                let cap: f32 = std::fs::read_to_string(p.join("capacity")).ok()?.trim().parse().ok()?;
                let status = std::fs::read_to_string(p.join("status")).unwrap_or_default();
                return Some((cap, status.trim() == "Charging"));
            }
        }
        None
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut s: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        if unsafe { GetSystemPowerStatus(&mut s) } == 0 || s.BatteryFlag & 128 != 0 || s.BatteryLifePercent > 100 {
            return None;
        }
        Some((s.BatteryLifePercent as f32, s.BatteryFlag & 8 != 0))
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("pmset").args(["-g", "batt"]).output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        let pct = s.split(['\t', ' ', ';']).find_map(|t| t.strip_suffix('%')?.parse::<f32>().ok())?;
        Some((pct, s.contains("charging;") && !s.contains("discharging")))
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StatItem;
    use crate::device::{canvas_size, orient, HEIGHT, WIDTH};
    use crate::render::{frame_to_rgb, ClockScreen};

    /// Renders every value in every style and orientation. Set PREVIEW_DIR to also
    /// save the pictures as PNG files.
    #[test]
    fn render_all_layouts() {
        let dir = std::env::var_os("PREVIEW_DIR").map(PathBuf::from);
        let all = StatsConfig { items: StatItem::ALL.to_vec(), rows: 0, ..Default::default() };
        let few = StatsConfig::default();
        for rotation in [0u16, 90] {
            let size = canvas_size(rotation);
            for (name, cfg) in [("default", few.clone()), ("all", all.clone())] {
                for style in StatStyle::ALL {
                    let mut s = StatsScreen::new(StatsConfig { style, ..cfg.clone() }, size);
                    let mut frame = Vec::new();
                    for _ in 0..3 {
                        frame = s.render().0;
                    }
                    assert_eq!(frame.len(), size.0 * size.1);
                    assert_eq!(orient(&frame, rotation).len(), WIDTH * HEIGHT);
                    if let Some(d) = &dir {
                        let _ = std::fs::create_dir_all(d);
                        let file = d.join(format!("stats-{name}-{style:?}-{rotation}.png"));
                        frame_to_rgb(&frame, size).save(file).unwrap();
                    }
                }
            }
            let clock = ClockScreen::new(None, size).render().0;
            if let Some(d) = &dir {
                frame_to_rgb(&clock, size).save(d.join(format!("clock-{rotation}.png"))).unwrap();
            }
        }
    }
}
