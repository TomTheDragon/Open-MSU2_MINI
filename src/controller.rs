//! Background thread that keeps the device connected and drives the selected mode.

use crate::capture::{Capture, CaptureOptions};
use crate::config::{Config, Mode};
use crate::device::{canvas_size, hw_flip, orient};
use crate::device::{Device, Frame565, FLASH_ANIMATION, FLASH_IMAGE, FRAME_PAGES};
use crate::render::{self, ClockScreen, ImageScreen, Screen};
use crate::stats::StatsScreen;
use crate::video::{Playback, VideoScreen};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Job = Box<dyn FnOnce(&mut Device) -> anyhow::Result<()> + Send>;
pub type JobDone = Box<dyn FnOnce(anyhow::Result<()>) + Send>;

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub connected: bool,
    pub text: String,
    /// Frames per second actually sent to the device (mirror / animations)
    pub fps: f32,
    /// USB data rate in KB/s
    pub kbps: f32,
    pub mode: Mode,
}

pub struct Handle {
    pub cfg: Arc<Mutex<Config>>,
    pub status: Arc<Mutex<Status>>,
    changed: Arc<AtomicBool>,
    reselect: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    jobs: mpsc::Sender<(Job, JobDone)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Handle {
    /// Apply a change to the settings (saved to disk) and let the controller pick it up.
    pub fn update(&self, f: impl FnOnce(&mut Config)) {
        let mut c = self.cfg.lock().unwrap();
        f(&mut c);
        c.save();
        self.changed.store(true, Ordering::SeqCst);
    }
    pub fn reselect_screen(&self) {
        self.reselect.store(true, Ordering::SeqCst);
        self.update(|c| c.mode = Mode::Mirror);
    }
    pub fn run_job(&self, job: Job, done: JobDone) {
        let _ = self.jobs.send((job, done));
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().map(|t| !t.is_finished()).unwrap_or(false)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn spawn(cfg: Config, use_button: bool, on_status: impl Fn(&Status) + Send + 'static) -> Handle {
    let cfg = Arc::new(Mutex::new(cfg));
    let status = Arc::new(Mutex::new(Status::default()));
    let changed = Arc::new(AtomicBool::new(true));
    let reselect = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let mut c = Controller {
        cfg: cfg.clone(),
        status: status.clone(),
        changed: changed.clone(),
        reselect: reselect.clone(),
        stop: stop.clone(),
        jobs: rx,
        on_status: Box::new(on_status),
        use_button,
        dev: None,
        mode: Mode::Mirror,
        screen: None,
        capture: None,
        last_capture: None,
        shown: None,
        flipped: None,
        rotation: 0,
        baseline: None,
        key_down: false,
        next_poll: Instant::now(),
        anim_frame: 0,
        meter: Meter::default(),
        dimmer: None,
    };
    let thread = std::thread::Builder::new().name("controller".into()).spawn(move || c.run()).unwrap();
    Handle { cfg, status, changed, reselect, stop, jobs: tx, thread: Some(thread) }
}

#[derive(Default)]
struct Meter {
    since: Option<Instant>,
    frames: u64,
    bytes: u64,
}

struct Controller {
    cfg: Arc<Mutex<Config>>,
    status: Arc<Mutex<Status>>,
    changed: Arc<AtomicBool>,
    reselect: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    jobs: mpsc::Receiver<(Job, JobDone)>,
    on_status: Box<dyn Fn(&Status) + Send>,
    use_button: bool,
    dev: Option<Device>,
    mode: Mode,
    screen: Option<Box<dyn Screen>>,
    capture: Option<(Capture, CaptureOptions)>,
    /// Last frame from the screen capture, to redraw after a settings change
    last_capture: Option<Frame565>,
    /// What the panel shows right now (None = unknown / drawn by the device itself)
    shown: Option<Frame565>,
    flipped: Option<bool>,
    /// Current picture rotation (canvas is portrait for 90/270)
    rotation: u16,
    baseline: Option<u16>,
    key_down: bool,
    next_poll: Instant,
    anim_frame: u32,
    meter: Meter,
    /// Software brightness (None = full brightness)
    dimmer: Option<Dimmer>,
}

/// Darkens pictures before they are sent: the panel has no brightness control the
/// firmware exposes, so dimming means darker colours (black stays black).
struct Dimmer {
    percent: u8,
    /// RGB565 channel lookup: 5-bit red/blue and 6-bit green values scaled down
    lut5: [u16; 32],
    lut6: [u16; 64],
}

impl Dimmer {
    fn new(percent: u8) -> Self {
        let f = percent as f32 / 100.0;
        Dimmer {
            percent,
            lut5: std::array::from_fn(|v| (v as f32 * f).round() as u16),
            lut6: std::array::from_fn(|v| (v as f32 * f).round() as u16),
        }
    }

    fn apply(&self, frame: &mut [u16]) {
        for p in frame {
            let (r, g, b) = (*p >> 11, (*p >> 5) & 63, *p & 31);
            *p = (self.lut5[r as usize] << 11) | (self.lut6[g as usize] << 5) | self.lut5[b as usize];
        }
    }
}

/// The mode after the current one in the button's cycle: all modes (or the configured
/// list), without Image, GIF and Video while no file is chosen for them.
fn next_button_mode(cfg: &Config) -> Mode {
    let seq: Vec<Mode> = if cfg.button_modes.is_empty() { Mode::ALL.to_vec() } else { cfg.button_modes.clone() };
    let usable = |m: &Mode| match m {
        Mode::Image => cfg.image.is_some(),
        Mode::Gif => cfg.gif.is_some(),
        Mode::Video => cfg.video.is_some(),
        _ => true,
    };
    let start = seq.iter().position(|m| *m == cfg.mode).map_or(0, |i| i + 1);
    (0..seq.len()).map(|k| seq[(start + k) % seq.len()]).find(usable).unwrap_or(cfg.mode)
}

const BUTTON_POLL: Duration = Duration::from_millis(200);

impl Controller {
    fn set_status(&self, f: impl FnOnce(&mut Status)) {
        let mut s = self.status.lock().unwrap();
        f(&mut s);
        (self.on_status)(&s);
    }

    fn run(&mut self) {
        while !self.stop.load(Ordering::SeqCst) {
            if let Err(e) = self.tick() {
                self.dev = None;
                self.shown = None;
                let msg = format!("waiting for device ({e:#})");
                self.set_status(|s| {
                    s.connected = false;
                    s.text = msg;
                    s.fps = 0.0;
                });
                for _ in 0..20 {
                    if self.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        self.capture = None;
        if let Some(d) = self.dev.as_mut() {
            let _ = d.flush();
        }
    }

    fn tick(&mut self) -> anyhow::Result<()> {
        if self.dev.is_none() {
            let port = self.cfg.lock().unwrap().port.clone();
            let d = Device::open(port.as_deref())?;
            let text = format!(
                "connected ({}, firmware v{})",
                d.port_name,
                d.version.map(|v| v.to_string()).unwrap_or("?".into())
            );
            self.dev = Some(d);
            self.flipped = None;
            self.baseline = None;
            self.shown = None;
            self.changed.store(true, Ordering::SeqCst);
            self.set_status(|s| {
                s.connected = true;
                s.text = text;
            });
        }
        while let Ok((job, done)) = self.jobs.try_recv() {
            let r = job(self.dev.as_mut().unwrap());
            let failed_io = r.as_ref().err().map(|e| format!("{e:#}"));
            done(r);
            self.shown = None;
            self.changed.store(true, Ordering::SeqCst);
            if let Some(e) = failed_io {
                if e.contains("did not answer") || e.contains("I/O") || e.contains("dropped") {
                    anyhow::bail!(e);
                }
            }
        }
        if self.changed.swap(false, Ordering::SeqCst) {
            self.enter_mode()?;
        }
        self.poll_button()?;
        let wait = self.step()?;
        let until = Instant::now() + wait;
        loop {
            self.poll_button()?;
            let now = Instant::now();
            if now >= until || self.changed.load(Ordering::SeqCst) || self.stop.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep((until - now).min(Duration::from_millis(10)));
            if self.mode == Mode::Mirror {
                break; // mirror polls for new frames itself
            }
        }
        Ok(())
    }

    fn enter_mode(&mut self) -> anyhow::Result<()> {
        let cfg = self.cfg.lock().unwrap().clone();
        let flip = hw_flip(cfg.rotation);
        if self.rotation != cfg.rotation {
            self.shown = None;
            self.rotation = cfg.rotation;
        }
        let size = canvas_size(cfg.rotation);
        if self.flipped != Some(flip) {
            self.dev.as_mut().unwrap().set_flipped(flip)?;
            self.flipped = Some(flip);
            self.shown = None;
        }
        if self.dimmer.as_ref().map_or(100, |d| d.percent) != cfg.brightness {
            self.dimmer = (cfg.brightness < 100).then(|| Dimmer::new(cfg.brightness));
            self.shown = None;
        }
        self.mode = cfg.mode;
        self.screen = None;
        self.meter = Meter::default();
        let mode = cfg.mode;
        self.set_status(|s| {
            s.mode = mode;
            s.fps = 0.0;
            s.kbps = 0.0;
        });
        if self.mode != Mode::Mirror {
            self.capture = None;
        }
        match self.mode {
            Mode::Stats => self.screen = Some(Box::new(StatsScreen::new(cfg.stats.clone(), size))),
            Mode::Clock => self.screen = Some(Box::new(ClockScreen::new(cfg.clock_background.as_deref(), size))),
            Mode::Image | Mode::Gif => {
                let (file, what) = if self.mode == Mode::Image { (&cfg.image, "image") } else { (&cfg.gif, "GIF") };
                match file.as_deref().map(|p| ImageScreen::new(p, cfg.fit, size)) {
                    Some(Ok(s)) => self.screen = Some(Box::new(s)),
                    Some(Err(e)) => {
                        self.set_status(|s| s.text = format!("{e:#}"));
                        self.show(render::message(&["Cannot open", &format!("the {what}")], [255, 120, 120], size))?;
                    }
                    None => self.show(render::message(&[&format!("No {what} selected")], [255, 255, 255], size))?,
                }
            }
            Mode::Video => match cfg.video.as_deref().map(|p| {
                VideoScreen::new(p, cfg.fit, size, Playback { sound: cfg.video_sound, looping: cfg.video_loop })
            }) {
                Some(Ok(s)) => self.screen = Some(Box::new(s)),
                Some(Err(e)) => {
                    self.set_status(|s| s.text = format!("{e:#}"));
                    self.show(render::message(&["Cannot play", "the video"], [255, 120, 120], size))?;
                }
                None => self.show(render::message(&["No video selected"], [255, 255, 255], size))?,
            },
            Mode::Album => {
                self.dev.as_mut().unwrap().show_flash_image(FLASH_IMAGE)?;
                self.shown = None;
            }
            Mode::Animation => self.anim_frame = 0,
            Mode::Mirror => {
                let opts = CaptureOptions {
                    fit: cfg.fit,
                    region: cfg.region.map(|r| (r[0], r[1], r[2], r[3])),
                    source: cfg.source,
                    monitor: cfg.monitor,
                    max_fps: cfg.max_fps.max(0.1),
                    size,
                    reselect: self.reselect.swap(false, Ordering::SeqCst),
                };
                let same = self.capture.as_ref().map(|(_, o)| {
                    o.fit == opts.fit
                        && o.region == opts.region
                        && o.source == opts.source
                        && o.monitor == opts.monitor
                        && o.max_fps == opts.max_fps
                        && o.size == opts.size
                });
                if same == Some(true) && !opts.reselect {
                    if let Some(f) = self.last_capture.clone() {
                        self.show(f)?; // e.g. after rotating: redraw even if the screen is static
                    }
                } else {
                    self.capture = None; // stop the old one first
                    self.last_capture = None;
                    self.show(render::message(
                        &["Waiting for screen", "sharing permission..."],
                        [255, 255, 255],
                        size,
                    ))?;
                    self.capture = Some((Capture::start(opts.clone()), opts));
                }
            }
        }
        Ok(())
    }

    /// Draw a canvas (see `device::canvas_size`), sending only what changed.
    fn show(&mut self, mut canvas: Frame565) -> anyhow::Result<()> {
        if let Some(dim) = &self.dimmer {
            dim.apply(&mut canvas);
        }
        let frame = orient(&canvas, self.rotation);
        let dev = self.dev.as_mut().unwrap();
        let before = dev.stats.bytes;
        if dev.draw_diff(&frame, self.shown.as_deref())? {
            self.meter.frames += 1;
            self.meter.bytes += dev.stats.bytes - before;
        }
        self.shown = Some(frame);
        Ok(())
    }

    fn update_meter(&mut self) {
        let since = *self.meter.since.get_or_insert_with(Instant::now);
        let dt = since.elapsed().as_secs_f32();
        if dt >= 1.0 {
            let (fps, kbps) = (self.meter.frames as f32 / dt, self.meter.bytes as f32 / dt / 1024.0);
            self.meter = Meter { since: Some(Instant::now()), ..Default::default() };
            self.set_status(|s| {
                s.fps = fps;
                s.kbps = kbps;
            });
        }
    }

    fn step(&mut self) -> anyhow::Result<Duration> {
        self.update_meter();
        match self.mode {
            Mode::Mirror => {
                let Some((cap, _)) = self.capture.as_mut() else { return Ok(Duration::from_millis(50)) };
                if let Some(err) = cap.error() {
                    self.capture = None;
                    self.set_status(|s| s.text = format!("screen capture failed: {err}"));
                    self.show(render::message(
                        &["Screen capture", "not available"],
                        [255, 120, 120],
                        canvas_size(self.rotation),
                    ))?;
                    return Ok(Duration::from_millis(200));
                }
                match cap.next_frame() {
                    Some(f) => {
                        self.last_capture = Some(f.clone());
                        self.show(f)?;
                        Ok(Duration::ZERO)
                    }
                    None => {
                        std::thread::sleep(Duration::from_millis(2));
                        Ok(Duration::ZERO)
                    }
                }
            }
            Mode::Animation => {
                let dev = self.dev.as_mut().unwrap();
                dev.show_flash_image(FLASH_ANIMATION + self.anim_frame * FRAME_PAGES)?;
                self.shown = None;
                self.meter.frames += 1;
                self.anim_frame = (self.anim_frame + 1) % 36;
                Ok(Duration::from_millis(100))
            }
            Mode::Album => Ok(Duration::from_millis(200)),
            _ => match self.screen.as_mut() {
                Some(s) => {
                    let (f, d) = s.render();
                    self.show(f)?;
                    Ok(d)
                }
                None => Ok(Duration::from_millis(200)),
            },
        }
    }

    /// Read the push button. Also the keep-alive: the device returns to offline mode
    /// after ~5 s without traffic.
    fn poll_button(&mut self) -> anyhow::Result<()> {
        if Instant::now() < self.next_poll {
            return Ok(());
        }
        self.next_poll = Instant::now() + BUTTON_POLL;
        let v = self.dev.as_mut().unwrap().read_adc(9)?;
        let Some(base) = self.baseline else {
            self.baseline = Some(v.saturating_sub(125));
            return Ok(());
        };
        if !self.use_button {
            return Ok(());
        }
        if v < base {
            self.key_down = true;
        } else if self.key_down {
            self.key_down = false;
            let mut cfg = self.cfg.lock().unwrap();
            let next = next_button_mode(&cfg);
            cfg.mode = next;
            cfg.save();
            drop(cfg);
            self.changed.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_cycles_all_modes_with_a_file() {
        let mut cfg = Config { mode: Mode::Clock, ..Default::default() };
        // no files: Image, GIF and Video are skipped
        assert_eq!(next_button_mode(&cfg), Mode::Album);
        cfg.image = Some("a.png".into());
        cfg.video = Some("v.mp4".into());
        assert_eq!(next_button_mode(&cfg), Mode::Image);
        cfg.mode = Mode::Image;
        assert_eq!(next_button_mode(&cfg), Mode::Video);
        cfg.mode = Mode::Animation;
        assert_eq!(next_button_mode(&cfg), Mode::Mirror);
    }
}
