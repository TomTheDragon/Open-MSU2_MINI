//! Customisable system monitor.

use crate::config::{StatGraphic, StatItem, StatsConfig, ValuePosition};
use crate::device::Frame565;
use crate::render::{blank, bold, regular, text, text_inverted, text_width, Anchor, Screen, Size, Style};
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
    /// Windows: CPU load from the kernel (sysinfo's performance counters can be missing)
    #[cfg(windows)]
    win_cpu: wincpu::WinCpu,
    /// Last search for temperature sensors (Windows: slow, and usually finds none)
    comps_searched: Option<Instant>,
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
            #[cfg(windows)]
            win_cpu: wincpu::WinCpu::new(),
            comps_searched: None,
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

    /// Total CPU load in percent
    fn cpu_usage(&self) -> f32 {
        #[cfg(windows)]
        if let Some(u) = self.win_cpu.total {
            return u;
        }
        self.sys.global_cpu_usage()
    }

    /// Load of each CPU core in percent
    fn core_usage(&self) -> Vec<f32> {
        #[cfg(windows)]
        if !self.win_cpu.cores.is_empty() {
            return self.win_cpu.cores.clone();
        }
        self.sys.cpus().iter().map(|c| c.usage()).collect()
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
            // with no sensors found, sysinfo searches again on every refresh (on Windows a
            // WMI query that needs admin rights); do that at most once a minute
            let search_due = self.comps_searched.is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
            if !self.comps.is_empty() || search_due {
                if self.comps.is_empty() {
                    self.comps_searched = Some(Instant::now());
                }
                self.comps.refresh(false);
            }
        }
        #[cfg(windows)]
        self.win_cpu.refresh();
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
                    let u = self.cpu_usage();
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
                    let v: Vec<f32> = self.core_usage().iter().map(|u| u / 100.0).collect();
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

    /// Whether this row gets a graphic
    fn has_viz(&self, row: &Row) -> bool {
        self.cfg.graphic != StatGraphic::None && (row.frac.is_some() || row.multi.is_some())
    }

    /// Bar, histogram or core bars inside `area` (x, y, w, h).
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
        // inner area: inside the outline, if there is one
        let (ix, iy, iw, ih) = if self.cfg.outline && h > 2 { (x + 1, y + 1, w - 2, h - 2) } else { (x, y, w, h) };
        match self.cfg.graphic {
            StatGraphic::None => {}
            StatGraphic::Bars => {
                let fill = (iw as f32 * frac.clamp(0.0, 1.0)) as u32;
                if fill > 0 {
                    draw_filled_rect_mut(img, Rect::at(ix, iy).of_size(fill, ih as u32), Rgb(self.color(frac)));
                }
            }
            StatGraphic::Histogram => {
                draw_filled_rect_mut(img, Rect::at(x, y).of_size(w as u32, h as u32), Rgb([25, 25, 30]));
                if let Some(hist) = self.history.get(&row.item) {
                    let skip = hist.len().saturating_sub(iw as usize);
                    let start = ix + iw - (hist.len() - skip) as i32;
                    for (i, v) in hist.iter().skip(skip).enumerate() {
                        let bh = ((ih as f32) * v).round().max(1.0) as i32;
                        draw_filled_rect_mut(
                            img,
                            Rect::at(start + i as i32, iy + ih - bh).of_size(1, bh as u32),
                            Rgb(self.color(*v)),
                        );
                    }
                }
            }
        }
        if self.cfg.outline {
            draw_hollow_rect_mut(img, Rect::at(x, y).of_size(w as u32, h as u32), Rgb([70, 70, 70]));
        }
    }

    /// Graphic and value of one row, between `x0` and `x1`, vertically centred on `cy`.
    /// `tall` is the height for histograms and for values drawn on top, `bar` the height
    /// of a plain bar.
    fn draw_body(
        &self,
        img: &mut RgbImage,
        row: &Row,
        (x0, x1): (i32, i32),
        cy: i32,
        (tall, bar): (i32, i32),
        fs: f32,
    ) {
        let pos = self.cfg.value_position;
        let white = |size: f32| Style::new(size, regular(), [230, 230, 230]);
        if !self.has_viz(row) {
            if pos != ValuePosition::None {
                let vfs = shrink_to_fit(&row.value, fs, x1 - x0);
                text(img, &row.value, x1, cy, white(vfs).anchor(Anchor::Right));
            }
            return;
        }
        let gh = if self.cfg.graphic == StatGraphic::Histogram || pos == ValuePosition::OnTop { tall } else { bar };
        let gy = cy - gh / 2;
        let gap = 4;
        match pos {
            ValuePosition::None => self.draw_viz(img, row, (x0, gy, x1 - x0, gh)),
            ValuePosition::Before | ValuePosition::Behind => {
                let vfs = shrink_to_fit(&row.value, fs, (x1 - x0) * 2 / 5);
                let vw = text_width(&row.value, vfs, regular());
                if pos == ValuePosition::Behind {
                    text(img, &row.value, x1, cy, white(vfs).anchor(Anchor::Right));
                    self.draw_viz(img, row, (x0, gy, x1 - vw - gap - x0, gh));
                } else {
                    text(img, &row.value, x0, cy, white(vfs));
                    self.draw_viz(img, row, (x0 + vw + gap, gy, x1 - x0 - vw - gap, gh));
                }
            }
            ValuePosition::OnTop => {
                self.draw_viz(img, row, (x0, gy, x1 - x0, gh));
                // digits are about 0.7 × the font size tall; keep them inside the graphic
                let vfs = shrink_to_fit(&row.value, fs.min((gh - 2) as f32 / 0.7), x1 - x0 - 4);
                text_inverted(img, &row.value, (x0 + x1) / 2, cy, white(vfs).anchor(Anchor::Center));
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
        let gap = self.cfg.label_gap.min(40) as i32;
        // landscape label column: as wide as the widest label of all pages, so rows line up
        let label_fs = (rh as f32 * 0.62).clamp(8.0, 24.0);
        let label_col = rows.iter().map(|r| text_width(r.item.short(), label_fs, bold())).max().unwrap_or(0);

        for (i, row) in visible.iter().enumerate() {
            let y0 = i as i32 * rh;
            if narrow {
                // label on top; graphic and value on the line underneath
                let fs = (rh as f32 * 0.45).clamp(8.0, 13.0);
                let ty = y0 + (rh as f32 * 0.32) as i32;
                text(&mut img, row.item.short(), 1, ty, Style::new(fs, bold(), accent));
                if !self.has_viz(row) {
                    // no graphic: the value goes on the label's line
                    let x0 = 1 + text_width(row.item.short(), fs, bold()) + gap;
                    self.draw_body(&mut img, row, (x0, w - 1), ty, (0, 0), fs * 0.95);
                    continue;
                }
                let value_below = self.cfg.value_position != ValuePosition::None;
                let vy = y0 + (rh as f32 * if value_below { 0.52 } else { 0.62 }) as i32;
                let gh = (y0 + rh - 2 - vy).max(2);
                let vfs = (fs * 0.95).min(gh as f32 / 0.7);
                // keep the last row clear of the page dots in the bottom-right corner
                let x1 = if pages > 1 && i as i32 == slots - 1 { w - 2 - pages as i32 * 4 } else { w - 1 };
                self.draw_body(&mut img, row, (1, x1), vy + gh / 2, (gh, gh), vfs);
            } else {
                let fs = label_fs;
                let cy = y0 + rh / 2;
                text(&mut img, row.item.short(), 2, cy, Style::new(fs, bold(), accent));
                let tall = (rh - 4).max(4);
                let bar = (rh - 8).clamp(4, 12);
                self.draw_body(&mut img, row, (2 + label_col + gap, w - 2), cy, (tall, bar), fs * 0.85);
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

/// CPU load on Windows from NtQuerySystemInformation, like Task Manager. sysinfo uses
/// performance counters (PDH), which are missing or broken on some systems.
#[cfg(windows)]
mod wincpu {
    /// SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION (times in 100 ns; kernel includes idle)
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Times {
        idle: i64,
        kernel: i64,
        user: i64,
        dpc: i64,
        interrupt: i64,
        interrupt_count: u32,
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQuerySystemInformation(class: u32, info: *mut core::ffi::c_void, len: u32, ret_len: *mut u32) -> i32;
    }

    const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;

    #[derive(Default)]
    pub struct WinCpu {
        prev: Vec<Times>,
        /// Percent; None until two readings exist (or when the call fails)
        pub total: Option<f32>,
        pub cores: Vec<f32>,
    }

    impl WinCpu {
        pub fn new() -> Self {
            let mut c = WinCpu::default();
            c.refresh();
            c
        }

        pub fn refresh(&mut self) {
            let mut now = vec![Times::default(); 256];
            let size = std::mem::size_of::<Times>();
            let mut ret = 0u32;
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
                    now.as_mut_ptr().cast(),
                    (now.len() * size) as u32,
                    &mut ret,
                )
            };
            if status < 0 {
                self.total = None;
                self.cores.clear();
                return;
            }
            now.truncate(ret as usize / size);
            if self.prev.len() == now.len() && !now.is_empty() {
                let busy = |a: &Times, b: &Times| {
                    let idle = a.idle - b.idle;
                    let all = (a.kernel - b.kernel) + (a.user - b.user);
                    (all - idle, all)
                };
                let (mut sum_busy, mut sum_all) = (0i64, 0i64);
                self.cores = now
                    .iter()
                    .zip(&self.prev)
                    .map(|(a, b)| {
                        let (busy, all) = busy(a, b);
                        sum_busy += busy;
                        sum_all += all;
                        percent(busy, all)
                    })
                    .collect();
                self.total = Some(percent(sum_busy, sum_all));
            }
            self.prev = now;
        }
    }

    fn percent(busy: i64, all: i64) -> f32 {
        if all > 0 {
            (busy as f32 / all as f32 * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        }
    }
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

/// `open-msu2-mini sysinfo`: every value, read three times, with the time each read takes.
pub fn print_values() {
    let cfg = StatsConfig { items: StatItem::ALL.to_vec(), ..Default::default() };
    let t = Instant::now();
    let mut s = StatsScreen::new(cfg, crate::device::canvas_size(0));
    println!("start: {} ms", t.elapsed().as_millis());
    for round in 1..=3 {
        std::thread::sleep(Duration::from_secs(1));
        let t = Instant::now();
        let rows = s.collect();
        println!("-- read {round}: {} ms", t.elapsed().as_millis());
        for r in rows {
            println!("{:<12} {}", format!("{:?}", r.item), r.value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StatItem;
    use crate::device::{canvas_size, orient, HEIGHT, WIDTH};
    use crate::render::{frame_to_rgb, ClockScreen};

    /// Renders every value with every graphic, value position and orientation. Set
    /// PREVIEW_DIR to also save the pictures as PNG files.
    #[test]
    fn old_style_setting_is_converted() {
        use crate::config::Config;
        let c: Config = serde_json::from_str(r#"{"stats": {"style": "graphs"}}"#).unwrap();
        let mut stats = c.stats;
        stats.migrate();
        assert_eq!(stats.graphic, StatGraphic::Histogram);
        assert!(!stats.outline);
        let mut text: StatsConfig = serde_json::from_str(r#"{"style": "text"}"#).unwrap();
        text.migrate();
        assert_eq!((text.graphic, text.value_position), (StatGraphic::None, ValuePosition::Behind));
        assert!(serde_json::to_string(&text).unwrap().find("\"style\"").is_none());
    }

    #[test]
    fn render_all_layouts() {
        let dir = std::env::var_os("PREVIEW_DIR").map(PathBuf::from);
        let all = StatsConfig { items: StatItem::ALL.to_vec(), rows: 0, ..Default::default() };
        let few = StatsConfig::default();
        for rotation in [0u16, 90] {
            let size = canvas_size(rotation);
            for (name, cfg) in [("default", few.clone()), ("all", all.clone())] {
                for graphic in StatGraphic::ALL {
                    for value_position in ValuePosition::ALL {
                        for outline in [true, false] {
                            let cfg = StatsConfig { graphic, value_position, outline, ..cfg.clone() };
                            let mut s = StatsScreen::new(cfg, size);
                            let mut frame = Vec::new();
                            for _ in 0..3 {
                                frame = s.render().0;
                            }
                            assert_eq!(frame.len(), size.0 * size.1);
                            assert_eq!(orient(&frame, rotation).len(), WIDTH * HEIGHT);
                            if let Some(d) = &dir {
                                let _ = std::fs::create_dir_all(d);
                                let o = if outline { "outline" } else { "plain" };
                                let file =
                                    d.join(format!("stats-{name}-{graphic:?}-{value_position:?}-{o}-{rotation}.png"));
                                frame_to_rgb(&frame, size).save(file).unwrap();
                            }
                        }
                    }
                }
            }
            let clock = ClockScreen::new(None, size).render().0;
            if let Some(d) = &dir {
                frame_to_rgb(&clock, size).save(d.join(format!("clock-{rotation}.png"))).unwrap();
            }
        }
    }

    /// Makes the README pictures docs/screens-landscape.png and docs/screens-portrait.png
    /// from live values: `README_DIR=docs cargo test --release readme_pictures -- --ignored`.
    /// Takes about 40 s, to fill the histograms.
    #[test]
    #[ignore]
    fn readme_pictures() {
        use image::{imageops, Rgb, RgbImage};
        let dir = PathBuf::from(std::env::var_os("README_DIR").expect("set README_DIR"));
        let mut screens: Vec<(u16, StatsScreen)> = Vec::new();
        for rotation in [0u16, 90] {
            let size = canvas_size(rotation);
            let all = StatsConfig { items: StatItem::ALL.to_vec(), page_seconds: 100_000, ..Default::default() };
            let middle = if rotation == 0 {
                StatsConfig { graphic: StatGraphic::Histogram, outline: false, ..all }
            } else {
                all
            };
            screens.push((rotation, StatsScreen::new(StatsConfig::default(), size)));
            screens.push((rotation, StatsScreen::new(middle, size)));
        }
        let mut frames = vec![Vec::new(); screens.len()];
        for i in 0..HISTORY {
            // the last sample over a full second, like the app (rates such as NET)
            std::thread::sleep(Duration::from_millis(if i + 1 == HISTORY { 1000 } else { 250 }));
            for (f, (_, s)) in frames.iter_mut().zip(screens.iter_mut()) {
                *f = s.render().0;
            }
        }
        let (scale, gap) = (3u32, 12u32);
        for (rotation, name) in [(0u16, "landscape"), (90, "portrait")] {
            let size = canvas_size(rotation);
            let clock = ClockScreen::new(None, size).render().0;
            let mut panels: Vec<&Frame565> =
                screens.iter().zip(&frames).filter(|((r, _), _)| *r == rotation).map(|(_, f)| f).collect();
            panels.push(&clock);
            let (w, h) = (size.0 as u32 * scale, size.1 as u32 * scale);
            let n = panels.len() as u32;
            let mut sheet = RgbImage::from_pixel(n * (w + gap) + gap, h + 2 * gap, Rgb([32, 34, 40]));
            for (i, f) in panels.into_iter().enumerate() {
                let big = imageops::resize(&frame_to_rgb(f, size), w, h, imageops::FilterType::Nearest);
                imageops::replace(&mut sheet, &big, (gap + i as u32 * (w + gap)) as i64, gap as i64);
            }
            sheet.save(dir.join(format!("screens-{name}.png"))).unwrap();
        }
    }
}
