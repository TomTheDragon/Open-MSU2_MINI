//! YouTube mode: plays live streams and videos from YouTube (and the other sites yt-dlp
//! knows). yt-dlp finds the addresses of the picture and the sound; ffmpeg reads and decodes
//! them (see video.rs). A live stream that breaks off is connected again.

use crate::device::Frame565;
use crate::render::{self, Screen, ScreenStatus, Size};
use crate::scale::Scaling;
use crate::video::{self, Input, VideoScreen};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How to install yt-dlp, for error messages
pub const INSTALL_YTDLP: &str = if cfg!(windows) {
    "install it with \"winget install yt-dlp.yt-dlp\" (or put yt-dlp.exe next to this program)"
} else if cfg!(target_os = "macos") {
    "install it with \"brew install yt-dlp\""
} else {
    "install the yt-dlp package"
};

/// Picture: 240p is plenty for 160×80 pixels (the smallest one if there is none that small),
/// H.264 preferred, as it is the cheapest to decode. Sound: always the best there is, as a
/// separate stream (live streams and most videos on YouTube have one); only when there is
/// none, a format with both.
const FORMAT: &str = "bv*[height<=240][vcodec^=avc1]+ba/bv*[height<=240]+ba/wv*+ba/b";

/// Longest wait for yt-dlp
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(60);

/// Attempts to connect a live stream again before giving up
const MAX_RETRIES: u32 = 5;

/// What yt-dlp found
struct Stream {
    title: String,
    live: bool,
    video: Input,
    sound: Option<Input>,
}

#[derive(Deserialize)]
struct Info {
    title: Option<String>,
    /// The title without the date yt-dlp adds to live streams
    fulltitle: Option<String>,
    is_live: Option<bool>,
    /// Separate picture and sound streams, when there are
    requested_formats: Option<Vec<Format>>,
    #[serde(flatten)]
    format: Format,
}

#[derive(Deserialize)]
struct Format {
    url: Option<String>,
    vcodec: Option<String>,
    acodec: Option<String>,
    #[serde(default)]
    http_headers: BTreeMap<String, String>,
}

impl Format {
    /// "none" = certainly not there; missing = not known (some sites)
    fn has_video(&self) -> bool {
        self.vcodec.as_deref() != Some("none")
    }
    fn has_sound(&self) -> bool {
        self.acodec.as_deref() != Some("none")
    }
    fn input(&self) -> Option<Input> {
        let url = self.url.clone()?;
        Some(Input::Url { url, headers: self.http_headers.clone().into_iter().collect() })
    }
}

fn find_ytdlp() -> Option<PathBuf> {
    video::find_tool("yt-dlp", &["--version"])
}

/// Asks yt-dlp for the stream's addresses (takes a few seconds).
fn resolve(url: &str, cancel: &AtomicBool) -> Result<Stream> {
    let ytdlp = find_ytdlp().ok_or_else(|| anyhow!("YouTube needs yt-dlp; {INSTALL_YTDLP}"))?;
    let mut cmd = Command::new(ytdlp);
    cmd.args(["-j", "--no-playlist", "--playlist-items", "1", "--no-warnings", "-f", FORMAT, "--"])
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    video::no_window(&mut cmd);
    let mut child = cmd.spawn().context("cannot start yt-dlp")?;
    // read both outputs while it runs (a full pipe would stop it)
    let reader = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = r {
                let _ = r.read_to_string(&mut s);
            }
            s
        })
    };
    let out = reader(child.stdout.take().map(|r| Box::new(r) as _));
    let err = reader(child.stderr.take().map(|r| Box::new(r) as _));
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if cancel.load(Ordering::SeqCst) || start.elapsed() > RESOLVE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("yt-dlp took too long");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if !status.success() {
        let msg = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("yt-dlp failed");
        bail!("{}", msg.trim_start_matches("ERROR: "));
    }
    let line = out.lines().find(|l| l.starts_with('{')).ok_or_else(|| anyhow!("yt-dlp found nothing to play"))?;
    let info: Info = serde_json::from_str(line).context("cannot read yt-dlp's answer")?;
    let formats = match info.requested_formats {
        Some(f) if !f.is_empty() => f,
        _ => vec![info.format],
    };
    let video = formats.iter().find(|f| f.has_video()).and_then(Format::input);
    let video = video.ok_or_else(|| anyhow!("there is no picture to show"))?;
    let sound = formats.iter().find(|f| f.has_sound()).and_then(Format::input);
    let title = info.fulltitle.or(info.title).unwrap_or_else(|| url.to_string());
    Ok(Stream { title, live: info.is_live == Some(true), video, sound })
}

/// "Loading YouTube..." with moving dots
fn loading(since: Instant, size: Size) -> Frame565 {
    let dots = ".".repeat(since.elapsed().as_millis() as usize / 500 % 4);
    render::message(&["Loading", &format!("YouTube{dots:<3}")], [255, 255, 255], size)
}

enum State {
    /// A fixed picture: stopped, no address, failed
    Still(Frame565),
    Resolving {
        rx: Receiver<Result<Stream>>,
        cancel: Arc<AtomicBool>,
        since: Instant,
    },
    Playing {
        screen: Box<VideoScreen>,
        live: bool,
        since: Instant,
        ended: bool,
    },
    /// Waiting before connecting to the live stream again
    Retry(Instant),
    /// Waiting a moment before starting (the mode was just chosen)
    Delay(Instant),
}

pub struct YouTubeScreen {
    url: String,
    fit: Scaling,
    size: Size,
    sound_on: Arc<AtomicBool>,
    state: State,
    title: Option<String>,
    retries: u32,
    /// Status not yet reported
    status: Option<ScreenStatus>,
}

impl YouTubeScreen {
    /// Starts playing `url` after `delay` (or shows that nothing plays). The sound is heard
    /// while `sound_on` is set; it can be switched while playing.
    pub fn new(
        url: Option<&str>,
        playing: bool,
        delay: Duration,
        fit: Scaling,
        size: Size,
        sound_on: Arc<AtomicBool>,
    ) -> Self {
        let url = url.map(str::trim).unwrap_or_default().to_string();
        let mut s = YouTubeScreen {
            url,
            fit,
            size,
            sound_on,
            state: State::Still(Vec::new()),
            title: None,
            retries: 0,
            status: None,
        };
        if s.url.is_empty() {
            s.still(&["No YouTube", "address"], [255, 255, 255], "YouTube: enter the address of a stream or video");
        } else if !playing {
            s.still(&["YouTube", "stopped"], [255, 255, 255], "YouTube: stopped");
        } else if delay > Duration::ZERO {
            s.state = State::Delay(Instant::now() + delay);
            s.report("YouTube: starting...".into());
        } else {
            s.start();
        }
        s
    }

    fn report(&mut self, text: String) {
        self.status = Some(ScreenStatus { text, title: self.title.clone() });
    }

    fn still(&mut self, lines: &[&str], color: [u8; 3], status: &str) {
        self.state = State::Still(render::message(lines, color, self.size));
        self.report(status.to_string());
    }

    fn fail(&mut self, e: &str) {
        eprintln!("youtube: {e}");
        self.still(&["Cannot play", "the stream"], [255, 120, 120], &format!("YouTube: {e}"));
    }

    fn start(&mut self) {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (url, c) = (self.url.clone(), cancel.clone());
        let spawned = std::thread::Builder::new().name("yt-dlp".into()).spawn(move || {
            let _ = tx.send(resolve(&url, &c));
        });
        if spawned.is_err() {
            return self.fail("cannot start yt-dlp");
        }
        self.state = State::Resolving { rx, cancel, since: Instant::now() };
        let what = self.title.clone().unwrap_or_else(|| self.url.clone());
        self.report(format!("YouTube: loading {what}..."));
    }

    fn loading(&self, since: Instant) -> Frame565 {
        loading(since, self.size)
    }

    /// The live stream broke off (or its address expired): connect again after a moment.
    fn retry(&mut self, why: &str) {
        self.retries += 1;
        if self.retries > MAX_RETRIES {
            return self.fail(why);
        }
        eprintln!("youtube: {why}; connecting again");
        self.state = State::Retry(Instant::now() + Duration::from_secs(3));
        self.report("YouTube: connection lost, connecting again...".into());
    }
}

impl Screen for YouTubeScreen {
    fn render(&mut self) -> (Frame565, Duration) {
        match &mut self.state {
            State::Still(f) => (f.clone(), Duration::from_millis(200)),
            State::Resolving { rx, since, .. } => {
                let since = *since;
                match rx.try_recv() {
                    Ok(Ok(stream)) => {
                        let screen = VideoScreen::stream(
                            &stream.video,
                            stream.sound.as_ref(),
                            self.sound_on.clone(),
                            self.fit,
                            self.size,
                        );
                        match screen {
                            Ok(screen) => {
                                let live = if stream.live { " (live)" } else { "" };
                                self.report(format!("YouTube: {}{live}", stream.title));
                                self.title = Some(stream.title);
                                if let Some(s) = self.status.as_mut() {
                                    s.title = self.title.clone();
                                }
                                self.state = State::Playing {
                                    screen: Box::new(screen),
                                    live: stream.live,
                                    since: Instant::now(),
                                    ended: false,
                                };
                            }
                            Err(e) => self.fail(&format!("{e:#}")),
                        }
                        (self.loading(since), Duration::ZERO)
                    }
                    Ok(Err(e)) => {
                        let e = format!("{e:#}");
                        if self.title.is_some() {
                            self.retry(&e); // a live stream that played before
                        } else {
                            self.fail(&e);
                        }
                        (self.loading(since), Duration::ZERO)
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.fail("yt-dlp stopped");
                        (self.loading(since), Duration::ZERO)
                    }
                    Err(TryRecvError::Empty) => (self.loading(since), Duration::from_millis(100)),
                }
            }
            State::Playing { screen, live, since, ended } => {
                let (frame, wait) = screen.render();
                let frame = if screen.started() { frame } else { loading(*since, self.size) };
                if *ended {
                    return (frame, wait);
                }
                let (live, played) = (*live, since.elapsed());
                let error = screen.error().map(str::to_string);
                let finished = screen.finished();
                if error.is_some() || finished {
                    if played > Duration::from_secs(60) {
                        self.retries = 0; // it played for a while: start counting again
                    }
                    let why = error.unwrap_or_else(|| "the stream ended".into());
                    if live {
                        self.retry(&why);
                    } else if finished {
                        if let State::Playing { ended, .. } = &mut self.state {
                            *ended = true;
                        }
                        let title = self.title.clone().unwrap_or_default();
                        self.report(format!("YouTube: {title} (ended)"));
                    } else {
                        self.fail(&why);
                    }
                }
                (frame, wait)
            }
            State::Delay(at) => {
                if Instant::now() >= *at {
                    self.start();
                }
                (render::message(&["YouTube"], [255, 255, 255], self.size), Duration::from_millis(100))
            }
            State::Retry(at) => {
                if Instant::now() >= *at {
                    self.start();
                }
                let f = render::message(&["Connecting", "again..."], [255, 255, 255], self.size);
                (f, Duration::from_millis(100))
            }
        }
    }

    fn status(&mut self) -> Option<ScreenStatus> {
        self.status.take()
    }
}

impl Drop for YouTubeScreen {
    fn drop(&mut self) {
        if let State::Resolving { cancel, .. } = &self.state {
            cancel.store(true, Ordering::SeqCst);
        }
    }
}

/// A favourite's address, when `name_or_url` is the name of one (command line)
pub fn favorite_url(favorites: &[crate::config::Favorite], name_or_url: &str) -> String {
    favorites
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case(name_or_url.trim()))
        .map(|f| f.url.clone())
        .unwrap_or_else(|| name_or_url.trim().to_string())
}
