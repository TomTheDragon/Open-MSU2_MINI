//! Video playback: plays a video file on the screen, optionally with sound and in a loop.
//!
//! Linux: decoded by ffmpeg (the system's package, any format it knows).
//! Windows / macOS: MP4/MOV files with H.264 video (what phones and most websites produce)
//! are decoded inside the program (OpenH264); everything else is handed to ffmpeg, if it is
//! installed (in PATH or next to this program).
//!
//! Every frame carries its time in the video; it is shown when the playback clock gets
//! there, and skipped when the device falls behind. With sound, the sound card is the clock.

use crate::audio::Sound;
use crate::device::Frame565;
use crate::render::{Screen, Size};
use crate::scale::Scaling;
use anyhow::{Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

/// File types offered in the file dialog.
pub const EXTENSIONS: [&str; 9] = ["mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "mpg", "mpeg"];

/// ffmpeg output rate (the panel shows about 25 full frames per second at most)
const FFMPEG_FPS: u32 = 30;

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
    size: Size,
}

impl VideoScreen {
    /// Opens the video and starts decoding in the background. Errors (unreadable file,
    /// unsupported format without ffmpeg) are returned here.
    pub fn new(path: &Path, fit: Scaling, size: Size, playback: Playback) -> Result<Self> {
        let (tx, rx) = sync_channel::<Msg>(4);
        let looping = playback.looping;
        #[cfg(target_os = "linux")]
        {
            let ffmpeg =
                find_ffmpeg().ok_or_else(|| anyhow::anyhow!("videos need ffmpeg; install the ffmpeg package"))?;
            start_ffmpeg_decoder(ffmpeg, path, fit, size, looping, tx)?;
        }
        #[cfg(not(target_os = "linux"))]
        match builtin::Mp4Source::open(path) {
            Ok(src) => {
                std::thread::Builder::new()
                    .name("video".into())
                    .spawn(move || builtin::run(src, fit, size, looping, tx))
                    .context("cannot start the video thread")?;
            }
            Err(builtin_err) => {
                let Some(ffmpeg) = find_ffmpeg() else {
                    return Err(builtin_err.context(
                        "this video format needs ffmpeg; install ffmpeg (or put it next to this program), \
                         or use an MP4 file with H.264 video",
                    ));
                };
                start_ffmpeg_decoder(ffmpeg, path, fit, size, looping, tx)?;
            }
        }
        Ok(VideoScreen {
            rx,
            sound: playback.sound.then(|| Sound::start(path)),
            looping,
            pass_start: None,
            pending: None,
            last: None,
            last_end: Duration::ZERO,
            ended: false,
            finished: false,
            error: None,
            size,
        })
    }

    /// Playback position in the current pass; None before its first picture.
    fn now(&self) -> Option<Duration> {
        let start = self.pass_start?;
        Some(self.sound.as_ref().and_then(Sound::position).unwrap_or_else(|| start.elapsed()))
    }

    fn fail(&mut self, e: &str) -> (Frame565, Duration) {
        eprintln!("video: {e}");
        let msg = crate::render::message(&["Cannot play", "the video"], [255, 120, 120], self.size);
        self.error = Some(msg.clone());
        self.sound = None;
        (msg, Duration::from_millis(500))
    }
}

impl Screen for VideoScreen {
    fn render(&mut self) -> (Frame565, Duration) {
        if let Some(e) = &self.error {
            return (e.clone(), Duration::from_millis(500));
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

// -- built-in: MP4/MOV + H.264 (Windows / macOS) ------------------------------------------

#[cfg(not(target_os = "linux"))]
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
    path: &Path,
    fit: Scaling,
    size: Size,
    looping: bool,
    tx: SyncSender<Msg>,
) -> Result<()> {
    let path = path.to_owned();
    let child = start_ffmpeg(&ffmpeg, &path, fit, size)?;
    std::thread::Builder::new()
        .name("video".into())
        .spawn(move || ffmpeg_loop(child, &ffmpeg, &path, fit, size, looping, tx))
        .context("cannot start the video thread")?;
    Ok(())
}

pub(crate) fn find_ffmpeg() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(name)));
    let candidates = beside.into_iter().filter(|p| p.is_file()).chain([PathBuf::from(name)]);
    for c in candidates {
        let mut cmd = Command::new(&c);
        cmd.arg("-version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
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

/// ffmpeg scales to the canvas size itself and makes a steady frame rate.
fn start_ffmpeg(ffmpeg: &Path, path: &Path, fit: Scaling, size: Size) -> Result<Child> {
    let (w, h) = size;
    let scale = match fit {
        Scaling::Crop => format!("scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}"),
        Scaling::Fit => {
            format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2")
        }
        Scaling::Stretch => format!("scale={w}:{h}"),
    };
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-an", "-sn", "-dn", "-vf"])
        .arg(format!("{scale},setsar=1,fps={FFMPEG_FPS}"))
        .args(["-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut cmd);
    cmd.spawn().context("cannot start ffmpeg")
}

/// One ffmpeg run per pass; `child` is the first one (started early to report errors).
fn ffmpeg_loop(
    mut child: Child,
    ffmpeg: &Path,
    path: &Path,
    fit: Scaling,
    size: Size,
    looping: bool,
    tx: SyncSender<Msg>,
) {
    let frame_time = Duration::from_secs_f64(1.0 / FFMPEG_FPS as f64);
    let mut buf = vec![0u8; size.0 * size.1 * 3];
    loop {
        let mut out = child.stdout.take().expect("piped");
        let mut frames = 0u32;
        let mut closed = false;
        while out.read_exact(&mut buf).is_ok() {
            let frame: Frame565 =
                buf.as_chunks::<3>().0.iter().map(|[r, g, b]| crate::device::rgb565(*r, *g, *b)).collect();
            if tx.send(Msg::Frame(frame, frame_time * frames, frame_time)).is_err() {
                closed = true; // screen closed
                break;
            }
            frames += 1;
        }
        let _ = child.kill();
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        let _ = child.wait();
        if closed {
            return;
        }
        if frames == 0 {
            let msg = err.lines().last().unwrap_or("ffmpeg could not read the video").to_string();
            let _ = tx.send(Msg::Error(msg));
            return;
        }
        if tx.send(Msg::End).is_err() || !looping {
            return;
        }
        child = match start_ffmpeg(ffmpeg, path, fit, size) {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Msg::Error(format!("{e:#}")));
                return;
            }
        };
    }
}
