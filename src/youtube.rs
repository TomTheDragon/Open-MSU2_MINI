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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
#[derive(Clone)]
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

/// yt-dlp's answers, so an address played again (or looked up in advance) starts without
/// asking yt-dlp: (address, answer, usable until), and the addresses being looked up now
struct Answers {
    ready: Vec<(String, Stream, SystemTime)>,
    pending: Vec<String>,
}

static ANSWERS: Mutex<Answers> = Mutex::new(Answers { ready: Vec::new(), pending: Vec::new() });

/// YouTube's stream addresses carry their expiry time (usually ~6 h); answers are used until
/// this long before it
const EXPIRY_MARGIN: Duration = Duration::from_secs(15 * 60);
/// Longest time an answer is used
const KEEP_MAX: Duration = Duration::from_secs(5 * 3600);
/// For addresses without an expiry time (other sites)
const KEEP_UNKNOWN: Duration = Duration::from_secs(20 * 60);

/// Expiry time in an address: `expire=1762…` or `/expire/1762…/` (live stream playlists)
fn expiry(url: &str) -> Option<SystemTime> {
    let rest = url.split_once("expire=").or_else(|| url.split_once("/expire/"))?.1;
    let secs: u64 = rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

/// Until when an answer can be used
fn usable_until(s: &Stream) -> SystemTime {
    let now = SystemTime::now();
    let urls = [Some(&s.video), s.sound.as_ref()];
    let expiries: Vec<SystemTime> = urls
        .into_iter()
        .flatten()
        .filter_map(|i| match i {
            Input::Url { url, .. } => expiry(url),
            Input::File(_) => None,
        })
        .collect();
    match expiries.into_iter().min() {
        Some(e) => (e - EXPIRY_MARGIN).min(now + KEEP_MAX),
        None => now + KEEP_UNKNOWN,
    }
}

/// The addresses for `url`: a kept answer when there is one (`fresh`: never), otherwise
/// yt-dlp's. Waits when the same address is being looked up already. Also returns whether
/// the answer was a kept one.
fn resolve_kept(url: &str, cancel: &AtomicBool, mut fresh: bool) -> Result<(Stream, bool)> {
    loop {
        {
            let mut a = ANSWERS.lock().unwrap();
            let now = SystemTime::now();
            a.ready.retain(|(u, _, until)| *until > now && !(fresh && u == url));
            fresh = false;
            if let Some((_, s, _)) = a.ready.iter().find(|(u, _, _)| u == url) {
                return Ok((s.clone(), true));
            }
            if !a.pending.iter().any(|u| u == url) {
                a.pending.push(url.to_string());
                break;
            }
        }
        if cancel.load(Ordering::SeqCst) {
            bail!("cancelled");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let r = resolve(url, cancel);
    let mut a = ANSWERS.lock().unwrap();
    a.pending.retain(|u| u != url);
    if let Ok(s) = &r {
        a.ready.retain(|(u, _, _)| u != url);
        a.ready.push((url.to_string(), s.clone(), usable_until(s)));
    }
    r.map(|s| (s, false))
}

/// Drop the kept answer for `url` (it did not play)
fn forget(url: &str) {
    ANSWERS.lock().unwrap().ready.retain(|(u, _, _)| u != url);
}

/// Look up `url` in the background, so it starts quickly when it is chosen.
fn look_up_in_advance(url: String) {
    let _ = std::thread::Builder::new().name("yt-dlp-ahead".into()).spawn(move || {
        if let Err(e) = resolve_kept(&url, &AtomicBool::new(false), false) {
            eprintln!("youtube: looking up {url} in advance: {e:#}");
        }
    });
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
        rx: Receiver<Result<(Stream, bool)>>,
        cancel: Arc<AtomicBool>,
        since: Instant,
    },
    Playing {
        screen: Box<VideoScreen>,
        live: bool,
        since: Instant,
        ended: bool,
        /// Paused since then
        paused_at: Option<Instant>,
    },
    /// Waiting before connecting to the live stream again
    Retry(Instant),
}

pub struct YouTubeScreen {
    url: String,
    fit: Scaling,
    size: Size,
    sound_on: Arc<AtomicBool>,
    state: State,
    title: Option<String>,
    /// Name of the favourite, for the status line while loading
    name: Option<String>,
    /// The addresses playing are a kept answer (when they fail, yt-dlp is asked again)
    kept: bool,
    /// Looked up in the background once this one plays (the next address of a double tap)
    ahead: Option<String>,
    retries: u32,
    /// Status not yet reported
    status: Option<ScreenStatus>,
}

impl YouTubeScreen {
    /// Starts playing `url` (or shows that nothing plays). The sound is heard
    /// while `sound_on` is set; it can be switched while playing.
    pub fn new(
        url: Option<&str>,
        name: Option<String>,
        ahead: Option<String>,
        playing: bool,
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
            name,
            kept: false,
            ahead,
            retries: 0,
            status: None,
        };
        if s.url.is_empty() {
            s.still(&["No YouTube", "address"], [255, 255, 255], "YouTube: enter the address of a stream or video");
        } else if !playing {
            s.still(&["YouTube", "stopped"], [255, 255, 255], "YouTube: stopped");
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
        self.start_with(false);
    }

    /// `fresh`: ask yt-dlp even when an answer is kept
    fn start_with(&mut self, fresh: bool) {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (url, c) = (self.url.clone(), cancel.clone());
        let spawned = std::thread::Builder::new().name("yt-dlp".into()).spawn(move || {
            let _ = tx.send(resolve_kept(&url, &c, fresh));
        });
        if spawned.is_err() {
            return self.fail("cannot start yt-dlp");
        }
        self.state = State::Resolving { rx, cancel, since: Instant::now() };
        let what = self.title.clone().or_else(|| self.name.clone()).unwrap_or_else(|| self.url.clone());
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
                    Ok(Ok((stream, kept))) => {
                        self.kept = kept;
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
                                    paused_at: None,
                                };
                                // the next address, for a double tap
                                if let Some(next) = self.ahead.take().filter(|n| *n != self.url) {
                                    look_up_in_advance(next);
                                }
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
            State::Playing { screen, live, since, ended, .. } => {
                let (frame, wait) = screen.render();
                let frame = if screen.started() { frame } else { loading(*since, self.size) };
                if *ended {
                    return (frame, wait);
                }
                let (live, played) = (*live, since.elapsed());
                let error = screen.error().map(str::to_string);
                let finished = screen.finished();
                let started = screen.started();
                if (error.is_some() || (live && finished)) && self.kept && !started {
                    // the kept addresses no longer work: ask yt-dlp again, right away
                    self.kept = false;
                    self.start_with(true);
                    return (frame, wait);
                }
                if error.is_some() || (live && finished) {
                    forget(&self.url);
                }
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

    fn toggle_pause(&mut self) -> Option<bool> {
        match &self.state {
            // a video that ended: play it again
            State::Playing { ended: true, .. } => {
                self.start();
                return Some(false);
            }
            State::Playing { .. } => {}
            _ => return None,
        }
        let State::Playing { screen, live, paused_at, .. } = &mut self.state else { return None };
        let live = *live;
        let resume_live_late = live && paused_at.is_some_and(|at| at.elapsed() > LIVE_RESUME_LATE);
        let paused = screen.toggle_pause()?;
        *paused_at = paused.then(Instant::now);
        let title = self.title.clone().unwrap_or_default();
        if resume_live_late {
            // the pictures waiting are old: connect again to show what is live now
            self.start();
            return Some(false);
        }
        self.report(if paused { format!("YouTube: {title} (paused)") } else { format!("YouTube: {title}") });
        Some(paused)
    }
}

/// A live stream paused longer than this connects again when it goes on
const LIVE_RESUME_LATE: Duration = Duration::from_secs(30);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(url: &str) -> Stream {
        Stream {
            title: "t".into(),
            live: false,
            video: Input::Url { url: url.into(), headers: Vec::new() },
            sound: None,
        }
    }

    #[test]
    fn expiry_from_youtube_addresses() {
        let at = |s: u64| Some(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(expiry("https://rr1.googlevideo.com/videoplayback?expire=1762400000&ei=x"), at(1762400000));
        assert_eq!(
            expiry("https://manifest.googlevideo.com/api/manifest/hls_playlist/expire/1762400000/ei/x"),
            at(1762400000)
        );
        assert_eq!(expiry("https://example.com/video.mp4"), None);
        // used until 15 minutes before it expires, at most 5 hours
        let soon = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 3600;
        let until = usable_until(&stream(&format!("https://x/videoplayback?expire={soon}")));
        assert_eq!(until, UNIX_EPOCH + Duration::from_secs(soon) - EXPIRY_MARGIN);
        let far = soon + 24 * 3600;
        let until = usable_until(&stream(&format!("https://x/videoplayback?expire={far}")));
        assert!(until <= SystemTime::now() + KEEP_MAX);
    }

    #[test]
    fn kept_answers_are_used_until_forgotten() {
        let url = "https://www.youtube.com/watch?v=kept-test";
        let until = SystemTime::now() + Duration::from_secs(600);
        ANSWERS.lock().unwrap().ready.push((url.into(), stream("https://x/v?expire=1"), until));
        let (s, kept) = resolve_kept(url, &AtomicBool::new(false), false).unwrap();
        assert!(kept);
        assert!(matches!(s.video, Input::Url { ref url, .. } if url == "https://x/v?expire=1"));
        forget(url);
        assert!(!ANSWERS.lock().unwrap().ready.iter().any(|(u, _, _)| u == url));
        // expired answers are not used
        let old = SystemTime::now() - Duration::from_secs(1);
        ANSWERS.lock().unwrap().ready.push((url.into(), stream("https://x/old"), old));
        let cancel = AtomicBool::new(true); // no yt-dlp in tests: stop at once
        ANSWERS.lock().unwrap().pending.push(url.into()); // as if it were being looked up
        assert!(resolve_kept(url, &cancel, false).is_err());
        ANSWERS.lock().unwrap().pending.retain(|u| u != url);
        assert!(!ANSWERS.lock().unwrap().ready.iter().any(|(u, _, _)| u == url));
    }
}
