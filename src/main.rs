// No console window for the GUI on Windows; CLI commands attach to the parent console.
#![cfg_attr(windows, windows_subsystem = "windows")]
// Some helpers are only used by the window; keep the window-less build warning-free.
#![cfg_attr(not(feature = "gui"), allow(dead_code))]

mod audio;
mod capture;
mod config;
mod controller;
mod device;
mod flash;
#[cfg(feature = "gui")]
mod gui;
#[cfg(feature = "gui")]
mod gui_stats;
#[cfg(feature = "gui")]
mod gui_tray;
mod render;
mod scale;
mod stats;
mod video;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use config::{Config, Mode};
use device::{Device, Frame565, HEIGHT, WIDTH};
use scale::Scaling;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "open-msu2-mini",
    version,
    about = "Open MSU2_MINI: driver and control app for the MSU2 / MSU2_MINI USB screen"
)]
struct Cli {
    /// Serial port of the device (default: auto-detect)
    #[arg(long, global = true)]
    port: Option<String>,
    /// Rotate the picture clockwise: 0, 90, 180 or 270 degrees (default: saved setting)
    #[arg(long, global = true, value_parser = ["0", "90", "180", "270"])]
    rotate: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Mirror,
    Stats,
    Clock,
    Image,
    Video,
    Album,
    Animation,
}

#[derive(Clone, Copy, ValueEnum)]
enum FitArg {
    Crop,
    Fit,
    Stretch,
}

impl From<FitArg> for Scaling {
    fn from(f: FitArg) -> Scaling {
        match f {
            FitArg::Crop => Scaling::Crop,
            FitArg::Fit => Scaling::Fit,
            FitArg::Stretch => Scaling::Stretch,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum FlashWhat {
    Album,
    ClockBackground,
    Animation,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the control window (default)
    Gui,
    /// Run without a window, using the saved settings (options override them)
    Run {
        mode: Option<ModeArg>,
        #[arg(long)]
        fit: Option<FitArg>,
        /// Mirror only part of the screen: X,Y,W,H
        #[arg(long, value_parser = parse_region)]
        region: Option<[u32; 4]>,
        /// Mirror the whole screen (clears a saved region)
        #[arg(long)]
        full: bool,
        /// Ask again which screen/window to share (Linux)
        #[arg(long)]
        reselect: bool,
        /// Monitor number to mirror on Windows/macOS (0 = primary)
        #[arg(long)]
        monitor: Option<usize>,
        /// Upper limit for frames per second, e.g. 0.5, 20 or 60
        #[arg(long)]
        fps: Option<f32>,
        /// Picture or GIF for the image mode
        #[arg(long)]
        image: Option<PathBuf>,
        /// Video for the video mode
        #[arg(long)]
        video: Option<PathBuf>,
        /// Play the video's sound (--sound=false to turn it off)
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        sound: Option<bool>,
        /// Start the video again at the end (--loop=false: play once, keep the last picture)
        #[arg(long = "loop", num_args = 0..=1, default_missing_value = "true")]
        looping: Option<bool>,
        #[arg(long)]
        clock_background: Option<PathBuf>,
        /// Ignore the push button on the device
        #[arg(long)]
        no_button: bool,
        /// Store the given options as new defaults
        #[arg(long)]
        save: bool,
    },
    /// Show a test pattern for 10 seconds
    Test,
    /// Show device information
    Info,
    /// Show a picture or GIF until Ctrl+C
    Show {
        file: PathBuf,
        #[arg(long, default_value = "crop")]
        fit: FitArg,
    },
    /// Store pictures permanently on the device
    Flash {
        what: FlashWhat,
        /// Picture (album, clock-background) or GIF / folder of pictures (animation)
        source: PathBuf,
        #[arg(long, default_value = "crop")]
        fit: FitArg,
    },
    /// Measure the frame rate with different kinds of content
    Bench {
        /// Seconds per test
        #[arg(long, default_value_t = 4.0)]
        seconds: f64,
        /// Also measure real screen mirroring
        #[arg(long)]
        mirror: bool,
    },
}

fn parse_region(s: &str) -> Result<[u32; 4], String> {
    let v: Vec<u32> = s
        .replace('x', ",")
        .split(',')
        .map(|p| p.trim().parse())
        .collect::<Result<_, _>>()
        .map_err(|_| "expected X,Y,W,H")?;
    v.try_into().map_err(|_| "expected X,Y,W,H".to_string())
}

fn main() {
    #[cfg(windows)]
    if std::env::args().len() > 1 {
        attach_console();
    }
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

#[cfg(windows)]
fn attach_console() {
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
}

fn run(cli: Cli) -> Result<()> {
    let rotation: Option<u16> = cli.rotate.as_deref().map(|r| r.parse().unwrap());
    let saved_rotation = || rotation.unwrap_or_else(|| Config::load().rotation);
    match cli.cmd.unwrap_or(Cmd::Gui) {
        Cmd::Gui => {
            #[cfg(feature = "gui")]
            return gui::run(cli.port);
            #[cfg(not(feature = "gui"))]
            anyhow::bail!("this build has no window; use `open-msu2-mini run`");
        }
        Cmd::Run {
            mode,
            fit,
            region,
            full,
            reselect,
            monitor,
            fps,
            image,
            video,
            sound,
            looping,
            clock_background,
            no_button,
            save,
        } => {
            let mut cfg = Config::load();
            if let Some(p) = cli.port {
                cfg.port = Some(p);
            }
            if let Some(m) = mode {
                cfg.mode = match m {
                    ModeArg::Mirror => Mode::Mirror,
                    ModeArg::Stats => Mode::Stats,
                    ModeArg::Clock => Mode::Clock,
                    ModeArg::Image => Mode::Image,
                    ModeArg::Video => Mode::Video,
                    ModeArg::Album => Mode::Album,
                    ModeArg::Animation => Mode::Animation,
                };
            }
            if let Some(f) = fit {
                cfg.fit = f.into();
            }
            if region.is_some() {
                cfg.region = region;
            }
            if full {
                cfg.region = None;
            }
            if let Some(m) = monitor {
                cfg.monitor = m;
            }
            if let Some(f) = fps {
                cfg.max_fps = f;
            }
            if let Some(i) = image {
                cfg.image = Some(std::fs::canonicalize(&i).unwrap_or(i));
                if mode.is_none() {
                    cfg.mode = Mode::Image;
                }
            }
            if let Some(v) = video {
                cfg.video = Some(std::fs::canonicalize(&v).unwrap_or(v));
                if mode.is_none() {
                    cfg.mode = Mode::Video;
                }
            }
            if let Some(s) = sound {
                cfg.video_sound = s;
            }
            if let Some(l) = looping {
                cfg.video_loop = l;
            }
            if let Some(b) = clock_background {
                cfg.clock_background = Some(std::fs::canonicalize(&b).unwrap_or(b));
            }
            if let Some(r) = rotation {
                cfg.rotation = r;
            }
            if save {
                cfg.save();
            }
            if let Some(pid) = config::background_pid() {
                anyhow::bail!("open-msu2-mini is already running in the background (pid {pid})");
            }
            config::write_pid();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let s2 = stop.clone();
            let _ = ctrlc_handler(move || s2.store(true, std::sync::atomic::Ordering::SeqCst));
            let mut last = String::new();
            let mut h = controller::spawn(cfg, !no_button, |_| {});
            if reselect {
                h.reselect_screen();
            }
            while !stop.load(std::sync::atomic::Ordering::SeqCst) && h.is_running() {
                std::thread::sleep(Duration::from_millis(200));
                let s = h.status.lock().unwrap().clone();
                if s.text != last {
                    println!("{}", s.text);
                    last = s.text;
                }
            }
            h.stop();
            config::remove_pid();
            Ok(())
        }
        Cmd::Info => {
            let d = Device::open(cli.port.as_deref())?;
            println!("port:      {}", d.port_name);
            println!(
                "firmware:  {}",
                d.version.map(|v| format!("v{v}")).unwrap_or("? (re-plug or wait a few seconds to see it)".into())
            );
            println!("screen:    {WIDTH}x{HEIGHT} (ST7735S)");
            Ok(())
        }
        Cmd::Test => {
            let mut d = Device::open(cli.port.as_deref())?;
            d.draw_full(&render::test_pattern())?;
            println!("Test pattern: red, green, blue, white boxes at the top, grey ramp, yellow text at the bottom.");
            keep_alive(&mut d, Duration::from_secs(10))
        }
        Cmd::Show { file, fit } => {
            let mut d = Device::open(cli.port.as_deref())?;
            let r = saved_rotation();
            d.set_flipped(device::hw_flip(r))?;
            let frames: Vec<(Frame565, Duration)> = render::load_frames(&file, fit.into(), device::canvas_size(r))?
                .into_iter()
                .map(|(f, d)| (device::orient(&f, r), d))
                .collect();
            println!("Showing {}, press Ctrl+C to stop.", file.display());
            let mut prev: Option<Frame565> = None;
            loop {
                for (f, dur) in &frames {
                    d.draw_diff(f, prev.as_deref())?;
                    prev = Some(f.clone());
                    keep_alive(&mut d, if frames.len() == 1 { Duration::from_secs(3600) } else { *dur })?;
                }
            }
        }
        Cmd::Flash { what, source, fit } => {
            let mut d = Device::open(cli.port.as_deref())?;
            let t = Instant::now();
            let progress = |done: usize, total: usize| {
                eprint!("\r  writing flash: {done}/{total} pages ({}%)", 100 * done / total);
                if done == total {
                    eprintln!();
                }
            };
            match what {
                FlashWhat::Album => flash::write_album(&mut d, &source, fit.into(), saved_rotation(), progress)?,
                FlashWhat::ClockBackground => {
                    flash::write_clock_background(&mut d, &source, fit.into(), saved_rotation(), progress)?
                }
                FlashWhat::Animation => {
                    flash::write_animation(&mut d, &source, fit.into(), saved_rotation(), progress)?
                }
            }
            println!("Done in {:.1} s.", t.elapsed().as_secs_f64());
            Ok(())
        }
        Cmd::Bench { seconds, mirror } => bench(cli.port.as_deref(), seconds, mirror),
    }
}

fn ctrlc_handler(f: impl Fn() + Send + 'static) -> Result<()> {
    // Ctrl+C / SIGTERM: the handler only sets a flag, which is signal-safe.
    // On Windows, closing the console ends the process, which is fine here.
    #[cfg(unix)]
    unsafe {
        static mut CB: Option<Box<dyn Fn() + Send>> = None;
        extern "C" fn handler(_: i32) {
            unsafe {
                #[allow(static_mut_refs)]
                if let Some(cb) = CB.as_ref() {
                    cb();
                }
            }
        }
        CB = Some(Box::new(f));
        extern "C" {
            fn signal(sig: i32, h: extern "C" fn(i32)) -> usize;
        }
        signal(2, handler); // SIGINT
        signal(15, handler); // SIGTERM
    }
    #[cfg(not(unix))]
    let _ = f;
    Ok(())
}

/// Keep the connection alive (button polling) for `dur`.
fn keep_alive(d: &mut Device, dur: Duration) -> Result<()> {
    let end = Instant::now() + dur;
    while Instant::now() < end {
        d.read_adc(9)?;
        std::thread::sleep(Duration::from_millis(200).min(end.saturating_duration_since(Instant::now())));
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// Benchmark

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn bench(port: Option<&str>, secs: f64, mirror: bool) -> Result<()> {
    let mut d = Device::open(port)?;
    d.read_adc(9)?;
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let ui = {
        let mut s = stats::StatsScreen::new(config::StatsConfig::default(), (WIDTH, HEIGHT));
        std::thread::sleep(Duration::from_millis(300));
        s_render(&mut s)
    };
    let pattern = render::test_pattern();

    type Gen<'a> = Box<dyn FnMut(u64) -> Frame565 + 'a>;
    let mut tests: Vec<(&str, Gen)> = vec![
        ("random noise (worst case)", Box::new(|_| (0..WIDTH * HEIGHT).map(|_| rng.next() as u16).collect())),
        (
            "scrolling picture",
            Box::new(|n| {
                (0..WIDTH * HEIGHT).map(|i| pattern[(i / WIDTH) * WIDTH + (i % WIDTH + n as usize) % WIDTH]).collect()
            }),
        ),
        (
            "video in a window (64x36)",
            Box::new(|n| {
                let mut f = ui.clone();
                let mut r = Rng(n.wrapping_mul(2654435761) | 1);
                for y in 22..58 {
                    for x in 48..112 {
                        f[y * WIDTH + x] = r.next() as u16;
                    }
                }
                f
            }),
        ),
        (
            "moving mouse pointer only",
            Box::new(|n| {
                let mut f = ui.clone();
                let (cx, cy) = ((n as usize * 3) % (WIDTH - 8), (n as usize * 2) % (HEIGHT - 12));
                for y in 0..12 {
                    for x in 0..=(y / 2) {
                        f[(cy + y) * WIDTH + cx + x] = 0xFFFF;
                    }
                }
                f
            }),
        ),
    ];

    println!("{:<30} {:>14} {:>14}   {:>9} {:>9}", "content", "vendor fps", "new fps", "vendor KB", "new KB");
    for (name, gen) in tests.iter_mut() {
        let mut res = Vec::new();
        for legacy in [true, false] {
            d.legacy = legacy;
            let mut prev: Option<Frame565> = None;
            let (start, b0) = (Instant::now(), d.stats.bytes);
            let mut n = 0u64;
            let mut next_poll = Instant::now();
            while start.elapsed().as_secs_f64() < secs {
                let f = gen(n);
                d.draw_diff(&f, prev.as_deref())?;
                prev = Some(f);
                n += 1;
                if Instant::now() >= next_poll {
                    d.read_adc(9)?; // keep-alive and flow control
                    next_poll = Instant::now() + Duration::from_millis(200);
                }
            }
            d.read_adc(9)?; // wait until the device has processed everything
            let dt = start.elapsed().as_secs_f64();
            res.push((n as f64 / dt, (d.stats.bytes - b0) as f64 / n as f64 / 1024.0));
        }
        println!("{:<30} {:>14.1} {:>14.1}   {:>9.1} {:>9.1}", name, res[0].0, res[1].0, res[0].1, res[1].1);
    }
    d.legacy = false;

    if mirror {
        drop(d);
        println!("\nMeasuring screen mirroring for {secs:.0} s (move windows or play a video)...");
        let mut cfg = Config::load();
        cfg.mode = Mode::Mirror;
        let mut h = controller::spawn(cfg, false, |_| {});
        let start = Instant::now();
        let mut samples = Vec::new();
        while start.elapsed().as_secs_f64() < secs + 3.0 {
            std::thread::sleep(Duration::from_millis(1000));
            let s = h.status.lock().unwrap().clone();
            if s.fps > 0.0 {
                samples.push((s.fps, s.kbps));
                println!("  {:5.1} fps  {:6.0} KB/s", s.fps, s.kbps);
            }
        }
        h.stop();
        if !samples.is_empty() {
            let avg = samples.iter().map(|s| s.0).sum::<f32>() / samples.len() as f32;
            println!("  average {avg:.1} fps");
        }
    }
    Ok(())
}

fn s_render(s: &mut dyn render::Screen) -> Frame565 {
    s.render().0
}
