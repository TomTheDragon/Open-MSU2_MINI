//! Video playback: plays a video file on the screen, optionally with sound and in a loop.
//!
//! Linux / Windows: decoded by ffmpeg (any format it knows; in PATH or next to this program).
//! macOS: MP4/MOV files with H.264 video (what phones and most websites produce) are decoded
//! inside the program (OpenH264); everything else is handed to ffmpeg, if it is installed.
//!
//! Every frame carries its time in the video; it is shown when the playback clock gets
//! there, and skipped when the device falls behind. With sound, the sound card is the clock.
//!
//! Network streams (the YouTube mode) are always read by ffmpeg.

use crate::audio::Sound;
use crate::device::Frame565;
use crate::render::{Screen, Size};
use crate::scale::Scaling;
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// File types offered in the file dialog.
pub const EXTENSIONS: [&str; 9] = ["mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "mpg", "mpeg"];

/// How to install ffmpeg, for error messages
pub const INSTALL_FFMPEG: &str = if cfg!(windows) {
    "install it with \"winget install Gyan.FFmpeg\" (or put ffmpeg.exe next to this program)"
} else {
    "install the ffmpeg package"
};

/// ffmpeg output rate (the panel shows about 25 full frames per second at most)
const FFMPEG_FPS: u32 = 30;

/// What a video (or its sound) is read from
#[derive(Clone, Debug)]
pub enum Input {
    File(PathBuf),
    /// Network address, with the HTTP headers it needs
    Url {
        url: String,
        headers: Vec<(String, String)>,
    },
}

impl Input {
    /// Adds ffmpeg's input options and `-i <input>`.
    pub(crate) fn add_to(&self, cmd: &mut Command) {
        match self {
            Input::File(p) => {
                cmd.arg("-i").arg(p);
            }
            Input::Url { url, headers } => {
                if url.starts_with("http") {
                    // carry on through short network drops
                    cmd.args(["-reconnect", "1", "-reconnect_streamed", "1", "-reconnect_delay_max", "5"]);
                    if !headers.is_empty() {
                        cmd.arg("-headers")
                            .arg(headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect::<String>());
                    }
                }
                cmd.arg("-i").arg(url);
            }
        }
    }

    /// The file, for the built-in decoders (macOS)
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn file(&self) -> Option<&Path> {
        match self {
            Input::File(p) => Some(p),
            Input::Url { .. } => None,
        }
    }
}

/// The running ffmpeg, so closing the screen ends it even while it waits for the network
type Running = Arc<Mutex<Option<Child>>>;

#[derive(Clone, Copy, Debug)]
pub struct Playback {
    pub sound: bool,
    pub looping: bool,
}

enum Msg {
    /// A picture, when it is due (from the start of the pass) and how long it lasts
    Frame(Frame565, Duration, Duration),
    /// End of one pass through the video
    End,
    Error(String),
}

pub struct VideoScreen {
    rx: Receiver<Msg>,
    sound: Option<Sound>,
    looping: bool,
    /// Start of the current pass (set when its first picture is shown)
    pass_start: Option<Instant>,
    /// Paused since then
    paused_at: Option<Instant>,
    /// Next picture, waiting for its time
    pending: Option<(Frame565, Duration, Duration)>,
    last: Option<Frame565>,
    /// When the last shown picture ends
    last_end: Duration,
    /// The current pass has no more pictures
    ended: bool,
    /// The decoder stopped for good
    finished: bool,
    error: Option<Frame565>,
    error_text: Option<String>,
    size: Size,
    running: Running,
}

impl VideoScreen {
    /// Opens the video and starts decoding in the background. Errors (unreadable file,
    /// unsupported format without ffmpeg) are returned here.
    pub fn new(path: &Path, fit: Scaling, size: Size, playback: Playback) -> Result<Self> {
        let input = Input::File(path.to_owned());
        let sound = playback.sound.then(|| Sound::start(&input, Arc::new(AtomicBool::new(true))));
        Self::open(&input, sound, fit, size, playback.looping)
    }

    /// Plays a network stream (once). `sound` is the soundtrack, heard while `sound_on` is set.
    pub fn stream(
        video: &Input,
        sound: Option<&Input>,
        sound_on: Arc<AtomicBool>,
        fit: Scaling,
        size: Size,
    ) -> Result<Self> {
        let sound = sound.map(|s| Sound::start(s, sound_on));
        Self::open(video, sound, fit, size, false)
    }

    fn open(input: &Input, sound: Option<Sound>, fit: Scaling, size: Size, looping: bool) -> Result<Self> {
        let (tx, rx) = sync_channel::<Msg>(4);
        let running = Running::default();
        #[cfg(target_os = "macos")]
        let builtin = match input.file() {
            Some(path) => Some(builtin::Mp4Source::open(path)),
            None => None,
        };
        #[cfg(not(target_os = "macos"))]
        let builtin: Option<Result<()>> = None;
        match builtin {
            None => {
                let what = if input.file().is_some() { "videos" } else { "streams" };
                let ffmpeg = find_ffmpeg().ok_or_else(|| anyhow::anyhow!("{what} need ffmpeg; {INSTALL_FFMPEG}"))?;
                start_ffmpeg_decoder(ffmpeg, input, fit, size, looping, tx, &running)?;
            }
            #[cfg(target_os = "macos")]
            Some(Ok(src)) => {
                std::thread::Builder::new()
                    .name("video".into())
                    .spawn(move || builtin::run(src, fit, size, looping, tx))
                    .context("cannot start the video thread")?;
            }
            #[cfg(target_os = "macos")]
            Some(Err(builtin_err)) => {
                let Some(ffmpeg) = find_ffmpeg() else {
                    return Err(builtin_err.context(
                        "this video format needs ffmpeg; install ffmpeg (or put it next to this program), \
                         or use an MP4 file with H.264 video",
                    ));
                };
                start_ffmpeg_decoder(ffmpeg, input, fit, size, looping, tx, &running)?;
            }
            #[cfg(not(target_os = "macos"))]
            Some(_) => unreachable!(),
        }
        Ok(VideoScreen {
            rx,
            sound,
            looping,
            pass_start: None,
            paused_at: None,
            pending: None,
            last: None,
            last_end: Duration::ZERO,
            ended: false,
            finished: false,
            error: None,
            error_text: None,
            size,
            running,
        })
    }

    /// The first picture is there
    pub fn started(&self) -> bool {
        self.last.is_some()
    }

    /// Playback has stopped for good (end of the video or stream); the last picture stays
    pub fn finished(&self) -> bool {
        // without looping, the end of the pass is the end
        self.pending.is_none() && (self.finished || (self.ended && !self.looping))
    }

    /// Why playback failed, if it did
    pub fn error(&self) -> Option<&str> {
        self.error_text.as_deref()
    }

    /// Playback position in the current pass; None before its first picture. Stands still
    /// while paused.
    fn now(&self) -> Option<Duration> {
        let start = self.pass_start?;
        let clock = || self.paused_at.unwrap_or_else(Instant::now).saturating_duration_since(start);
        Some(self.sound.as_ref().and_then(Sound::position).unwrap_or_else(clock))
    }

    pub fn paused(&self) -> bool {
        self.paused_at.is_some()
    }

    fn fail(&mut self, e: &str) -> (Frame565, Duration) {
        eprintln!("video: {e}");
        let msg = crate::render::message(&["Cannot play", "the video"], [255, 120, 120], self.size);
        self.error = Some(msg.clone());
        self.error_text = Some(e.to_string());
        self.sound = None;
        (msg, Duration::from_millis(500))
    }
}

impl Screen for VideoScreen {
    fn toggle_pause(&mut self) -> Option<bool> {
        if self.error.is_some() || self.finished() {
            return None;
        }
        match self.paused_at.take() {
            Some(at) => {
                // leave the pause out of the clock
                if let Some(start) = self.pass_start.as_mut() {
                    *start += at.elapsed();
                }
                if let Some(s) = &self.sound {
                    s.set_paused(false);
                }
            }
            None => {
                self.paused_at = Some(Instant::now());
                if let Some(s) = &self.sound {
                    s.set_paused(true);
                }
            }
        }
        Some(self.paused())
    }

    fn render(&mut self) -> (Frame565, Duration) {
        if let Some(e) = &self.error {
            return (e.clone(), Duration::from_millis(500));
        }
        if self.paused() {
            // the decoder waits too: it can only work a few pictures ahead
            let f = self.last.clone().unwrap_or_else(|| vec![0; self.size.0 * self.size.1]);
            return (f, Duration::from_millis(50));
        }
        let mut fresh = None;
        let mut wait = Duration::from_millis(5);
        loop {
            if self.pending.is_none() && !self.ended {
                // wait a little for the very first picture, never later on
                let msg = if self.last.is_none() {
                    self.rx.recv_timeout(Duration::from_millis(100)).map_err(|e| match e {
                        RecvTimeoutError::Timeout => TryRecvError::Empty,
                        RecvTimeoutError::Disconnected => TryRecvError::Disconnected,
                    })
                } else {
                    self.rx.try_recv()
                };
                match msg {
                    Ok(Msg::Frame(f, at, len)) => self.pending = Some((f, at, len)),
                    Ok(Msg::End) => self.ended = true,
                    Ok(Msg::Error(e)) => return self.fail(&e),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.ended = true;
                        self.finished = true;
                    }
                }
                continue;
            }
            if let Some((_, at, _)) = &self.pending {
                let now = match self.now() {
                    Some(t) => t,
                    None => {
                        // first picture of a pass: start the clock (and the sound)
                        self.pass_start = Some(Instant::now());
                        if let Some(s) = &self.sound {
                            s.play();
                        }
                        Duration::ZERO
                    }
                };
                if *at > now {
                    wait = (*at - now).min(Duration::from_millis(15));
                    break;
                }
                // due (or overdue: then the next one may replace it right away)
                let (f, at, len) = self.pending.take().unwrap();
                fresh = Some(f);
                self.last_end = at + len;
                continue;
            }
            // the pass has no more pictures
            if self.looping && !self.finished && self.now().is_some_and(|t| t >= self.last_end) {
                self.ended = false;
                self.pass_start = None;
                if let Some(s) = &self.sound {
                    s.restart();
                }
                continue;
            }
            wait = Duration::from_millis(if self.looping { 5 } else { 50 });
            break;
        }
        if let Some(f) = fresh {
            self.last = Some(f);
        }
        let f = self.last.clone().unwrap_or_else(|| vec![0; self.size.0 * self.size.1]);
        (f, wait)
    }
}

impl Drop for VideoScreen {
    fn drop(&mut self) {
        if let Some(c) = self.running.lock().unwrap().as_mut() {
            let _ = c.kill(); // the video thread then ends and cleans up
        }
    }
}

// -- built-in: MP4/MOV + H.264 (macOS) -----------------------------------------------------

#[cfg(target_os = "macos")]
mod builtin {
    use super::{Msg, Size};
    use crate::scale::{Order, Scaler, Scaling};
    use anyhow::{anyhow, bail, Context, Result};
    use std::io::BufReader;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::SyncSender;
    use std::time::Duration;

    pub(super) struct Mp4Source {
        path: PathBuf,
        track_id: u32,
        samples: u32,
        frame_time: Duration,
        length_size: usize,
        /// SPS and PPS NAL units, in Annex B form (start code + unit)
        params: Vec<u8>,
    }

    fn open_mp4(path: &Path) -> Result<mp4::Mp4Reader<BufReader<std::fs::File>>> {
        let f = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let len = f.metadata()?.len();
        mp4::Mp4Reader::read_header(BufReader::new(f), len).map_err(|e| anyhow!("not an MP4/MOV file ({e})"))
    }

    impl Mp4Source {
        pub(super) fn open(path: &Path) -> Result<Self> {
            let reader = open_mp4(path)?;
            let track = reader
                .tracks()
                .values()
                .find(|t| matches!(t.media_type(), Ok(mp4::MediaType::H264)))
                .ok_or_else(|| anyhow!("the video is not H.264"))?;
            let avcc = &track.trak.mdia.minf.stbl.stsd.avc1.as_ref().ok_or_else(|| anyhow!("no H.264 settings"))?.avcc;
            let mut params = Vec::new();
            for unit in avcc.sequence_parameter_sets.iter().chain(&avcc.picture_parameter_sets) {
                params.extend([0, 0, 0, 1]);
                params.extend(&unit.bytes);
            }
            let fps = track.frame_rate();
            let fps = if fps.is_finite() && fps > 1.0 { fps.min(120.0) } else { 30.0 };
            Ok(Mp4Source {
                path: path.to_owned(),
                track_id: track.track_id(),
                samples: track.sample_count(),
                frame_time: Duration::from_secs_f64(1.0 / fps),
                length_size: avcc.length_size_minus_one as usize + 1,
                params,
            })
        }

        /// MP4 stores NAL units with a length prefix; OpenH264 wants start codes. The
        /// parameter sets live in the file header and are repeated before every key frame.
        fn to_annex_b(&self, sample: &[u8], out: &mut Vec<u8>) {
            out.clear();
            let mut rest = sample;
            let mut params_sent = false;
            while rest.len() > self.length_size {
                let len = rest[..self.length_size].iter().fold(0usize, |a, b| (a << 8) | *b as usize);
                rest = &rest[self.length_size..];
                if len == 0 || len > rest.len() {
                    break;
                }
                let unit = &rest[..len];
                rest = &rest[len..];
                // before the key frame's picture data (encoders often put an SEI unit first)
                if !params_sent && unit[0] & 0x1f == 5 {
                    out.extend(&self.params);
                    params_sent = true;
                }
                out.extend([0, 0, 0, 1]);
                out.extend(unit);
            }
        }
    }

    pub(super) fn run(src: Mp4Source, fit: Scaling, size: Size, looping: bool, tx: SyncSender<Msg>) {
        if let Err(e) = builtin_play(&src, fit, size, looping, &tx) {
            let _ = tx.send(Msg::Error(format!("{e:#}")));
        }
    }

    /// Plays the file (again and again when looping) until the screen is closed (the channel is gone).
    fn builtin_play(src: &Mp4Source, fit: Scaling, size: Size, looping: bool, tx: &SyncSender<Msg>) -> Result<()> {
        use openh264::decoder::{Decoder, DecoderConfig, Flush};
        use openh264::formats::YUVSource;
        let mut scaler = Scaler::default().sized(size);
        let mut packet = Vec::new();
        let mut rgba = Vec::new();
        let mut shown_any = false;
        let mut emit = |img: &openh264::decoder::DecodedYUV, n: u32, scaler: &mut Scaler| -> bool {
            let (w, h) = img.dimensions();
            rgba.resize(w * h * 4, 0);
            img.write_rgba8(&mut rgba);
            let frame = scaler
                .panel_frame(&rgba, w as u32, h as u32, w * 4, Order::Rgba, None, fit)
                .unwrap_or_else(|| vec![0; size.0 * size.1]);
            tx.send(Msg::Frame(frame, src.frame_time * n, src.frame_time)).is_ok()
        };
        loop {
            let mut reader = open_mp4(&src.path)?;
            let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
            let mut decoder = Decoder::with_api_config(openh264::OpenH264API::from_source(), config)
                .map_err(|e| anyhow!("cannot start the H.264 decoder ({e})"))?;
            let mut decoded = 0u32;
            for i in 1..=src.samples {
                let Some(sample) = reader.read_sample(src.track_id, i).map_err(|e| anyhow!("{e}"))? else { continue };
                src.to_annex_b(&sample.bytes, &mut packet);
                // a broken frame is skipped; the next key frame repairs the picture
                if let Ok(Some(img)) = decoder.decode(&packet) {
                    if !emit(&img, decoded, &mut scaler) {
                        return Ok(());
                    }
                    decoded += 1;
                }
            }
            for img in decoder.flush_remaining().unwrap_or_default() {
                if !emit(&img, decoded, &mut scaler) {
                    return Ok(());
                }
                decoded += 1;
            }
            if decoded > 0 {
                shown_any = true;
            } else if !shown_any {
                bail!("no picture could be decoded (unsupported H.264 variant?)");
            }
            if tx.send(Msg::End).is_err() || !looping {
                return Ok(());
            }
        }
    }
}

// -- ffmpeg -------------------------------------------------------------------------------

fn start_ffmpeg_decoder(
    ffmpeg: PathBuf,
    input: &Input,
    fit: Scaling,
    size: Size,
    looping: bool,
    tx: SyncSender<Msg>,
    running: &Running,
) -> Result<()> {
    let input = input.clone();
    let first = start_ffmpeg(&ffmpeg, &input, fit, size, running)?;
    let running = running.clone();
    std::thread::Builder::new()
        .name("video".into())
        .spawn(move || ffmpeg_loop(first, &ffmpeg, &input, fit, size, looping, tx, &running))
        .context("cannot start the video thread")?;
    Ok(())
}

pub(crate) fn find_ffmpeg() -> Option<PathBuf> {
    find_tool("ffmpeg", &["-version"])
}

/// Finds a helper program: next to this program, where winget (Windows) or Homebrew (macOS)
/// put it, or in PATH. `args` is a quick command that must succeed.
pub(crate) fn find_tool(tool: &str, args: &[&str]) -> Option<PathBuf> {
    // found once, kept for the rest of the run (starting yt-dlp just to check takes ~0.3 s);
    // a missing one is searched again, so installing it later works without a restart
    static FOUND: std::sync::Mutex<Vec<(String, PathBuf)>> = std::sync::Mutex::new(Vec::new());
    if let Some((_, p)) = FOUND.lock().unwrap().iter().find(|(t, _)| t == tool) {
        return Some(p.clone());
    }
    let found = search_tool(tool, args)?;
    FOUND.lock().unwrap().push((tool.to_string(), found.clone()));
    Some(found)
}

fn search_tool(tool: &str, args: &[&str]) -> Option<PathBuf> {
    let name = if cfg!(windows) { format!("{tool}.exe") } else { tool.to_string() };
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(&name)));
    // Windows: where "winget install ..." links it (in PATH only for programs
    // started after the installation)
    let winget = std::env::var_os("LOCALAPPDATA")
        .filter(|_| cfg!(windows))
        .map(|d| PathBuf::from(d).join(r"Microsoft\WinGet\Links").join(&name));
    // macOS: programs started from the Finder do not get Homebrew's PATH
    let brew = ["/opt/homebrew/bin", "/usr/local/bin"]
        .iter()
        .filter(|_| cfg!(target_os = "macos"))
        .map(|d| Path::new(d).join(&name));
    let candidates = beside.into_iter().chain(winget).chain(brew).filter(|p| p.is_file()).chain([PathBuf::from(&name)]);
    for c in candidates {
        let mut cmd = Command::new(&c);
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        no_window(&mut cmd);
        if cmd.status().map(|s| s.success()).unwrap_or(false) {
            return Some(c);
        }
    }
    None
}

pub(crate) fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// One ffmpeg run: its pictures, and its last error message once it has ended
struct FfmpegRun {
    out: ChildStdout,
    last_error: std::thread::JoinHandle<String>,
}

/// ffmpeg scales to the canvas size itself and makes a steady frame rate.
fn start_ffmpeg(ffmpeg: &Path, input: &Input, fit: Scaling, size: Size, running: &Running) -> Result<FfmpegRun> {
    let (w, h) = size;
    let scale = match fit {
        Scaling::Crop => format!("scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}"),
        Scaling::Fit => {
            format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2")
        }
        Scaling::Stretch => format!("scale={w}:{h}"),
    };
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-v", "error", "-nostdin"]);
    input.add_to(&mut cmd);
    cmd.args(["-an", "-sn", "-dn", "-vf"])
        .arg(format!("{scale},setsar=1,fps={FFMPEG_FPS}"))
        .args(["-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut cmd);
    let mut child = cmd.spawn().context("cannot start ffmpeg")?;
    let out = child.stdout.take().expect("piped");
    let err = child.stderr.take().expect("piped");
    // read the messages as they come: a full pipe would stop ffmpeg (long streams)
    let last_error = std::thread::spawn(move || {
        let mut last = String::new();
        for line in BufReader::new(err).lines().map_while(|l| l.ok()) {
            if !line.trim().is_empty() {
                last = line;
            }
        }
        last
    });
    *running.lock().unwrap() = Some(child);
    Ok(FfmpegRun { out, last_error })
}

/// One ffmpeg run per pass; `run` is the first one (started early to report errors).
#[allow(clippy::too_many_arguments)]
fn ffmpeg_loop(
    mut run: FfmpegRun,
    ffmpeg: &Path,
    input: &Input,
    fit: Scaling,
    size: Size,
    looping: bool,
    tx: SyncSender<Msg>,
    running: &Running,
) {
    let frame_time = Duration::from_secs_f64(1.0 / FFMPEG_FPS as f64);
    let mut buf = vec![0u8; size.0 * size.1 * 3];
    loop {
        let mut frames = 0u32;
        let mut closed = false;
        while run.out.read_exact(&mut buf).is_ok() {
            let frame: Frame565 =
                buf.as_chunks::<3>().0.iter().map(|[r, g, b]| crate::device::rgb565(*r, *g, *b)).collect();
            if tx.send(Msg::Frame(frame, frame_time * frames, frame_time)).is_err() {
                closed = true; // screen closed
                break;
            }
            frames += 1;
        }
        if let Some(mut child) = running.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        drop(run.out);
        let err = run.last_error.join().unwrap_or_default();
        if closed {
            return;
        }
        if frames == 0 {
            let msg = if err.is_empty() { "ffmpeg could not read the video".to_string() } else { err };
            let _ = tx.send(Msg::Error(msg));
            return;
        }
        if tx.send(Msg::End).is_err() || !looping {
            return;
        }
        run = match start_ffmpeg(ffmpeg, input, fit, size, running) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(Msg::Error(format!("{e:#}")));
                return;
            }
        };
    }
}
