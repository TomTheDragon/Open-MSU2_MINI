//! Sound for the video mode: decodes a video's soundtrack and plays it on the default
//! output device. It also serves as the video's clock, so picture and sound stay together.
//!
//! Linux / Windows: the soundtrack is decoded by ffmpeg.
//! macOS: decoded inside the program (Symphonia: AAC, MP3, FLAC, Vorbis, PCM in
//! MP4/MOV, MKV/WebM, OGG, WAV); other formats are handed to ffmpeg, if it is installed.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How much decoded sound is kept ready (seconds)
const BUFFER_SECS: usize = 1;

pub struct Sound {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    stop: AtomicBool,
}

#[derive(Default)]
struct State {
    /// Counts restarts; the decoder starts over when it changes
    generation: u64,
    /// Decoded sound: stereo, interleaved, at the device's rate
    queue: VecDeque<f32>,
    /// Output takes from the queue (the clock runs)
    playing: bool,
    /// Frames handed to the device since `play`
    played: u64,
    /// Time of the last output callback and how many frames it took from the queue
    last_callback: Option<(Instant, u64)>,
    /// Delay between handing sound to the device and hearing it
    latency: Duration,
    /// The decoder reached the end of the soundtrack
    input_done: bool,
    /// When the queue ran empty after the end; the clock then continues on its own
    drained_at: Option<Instant>,
    /// Device sample rate (0 while the device is being opened)
    rate: u32,
    /// No sound (no output device, no soundtrack, unsupported format): use another clock
    failed: bool,
}

impl Sound {
    /// Starts decoding in the background. The sound stays silent until `play`.
    pub fn start(path: &Path) -> Sound {
        let shared = Arc::new(Shared::default());
        let s = shared.clone();
        let path = path.to_owned();
        let spawned = std::thread::Builder::new().name("sound".into()).spawn(move || {
            if let Err(e) = run(&s, &path) {
                eprintln!("sound: {e:#}");
            }
            s.state.lock().unwrap().failed = true;
        });
        if spawned.is_err() {
            shared.state.lock().unwrap().failed = true;
        }
        Sound { shared }
    }

    /// Starts the sound (and with it the clock).
    pub fn play(&self) {
        self.shared.state.lock().unwrap().playing = true;
    }

    /// Goes back to the beginning, silent until `play`.
    pub fn restart(&self) {
        let mut st = self.shared.state.lock().unwrap();
        st.generation += 1;
        st.queue.clear();
        st.playing = false;
        st.played = 0;
        st.last_callback = None;
        st.input_done = false;
        st.drained_at = None;
    }

    /// How far the sound has played since `play`. None when there is no sound.
    pub fn position(&self) -> Option<Duration> {
        let st = self.shared.state.lock().unwrap();
        if st.failed {
            return None;
        }
        if !st.playing || st.rate == 0 {
            return Some(Duration::ZERO);
        }
        let rate = st.rate as f64;
        let secs = |frames: u64| Duration::from_secs_f64(frames as f64 / rate);
        let pos = match (st.drained_at, st.last_callback) {
            (Some(at), _) => secs(st.played) + at.elapsed(),
            // between callbacks, move on smoothly within what the last one handed over
            (None, Some((at, n))) => secs(st.played - n) + at.elapsed().min(secs(n)),
            (None, None) => Duration::ZERO,
        };
        Some(pos.saturating_sub(st.latency))
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

/// Sound thread: owns the output stream (not movable between threads on every system)
/// and decodes one pass per `restart`.
fn run(shared: &Arc<Shared>, path: &Path) -> Result<()> {
    let (_stream, rate) = open_output(shared)?;
    let mut backend = None;
    loop {
        let generation = shared.state.lock().unwrap().generation;
        let mut source = match backend {
            None => {
                let (s, b) = open_source(path, rate)?;
                backend = Some(b);
                s
            }
            #[cfg(target_os = "macos")]
            Some(Backend::Builtin) => Source::Builtin(Box::new(Builtin::open(path)?)),
            Some(Backend::Ffmpeg(ref ff)) => Source::Ffmpeg(Ffmpeg::start(ff, path, rate)?),
        };
        let mut resample = Resampler::new(rate);
        let mut buf = Vec::new();
        let mut out = Vec::new();
        let complete = loop {
            match source.next_chunk(&mut buf) {
                Ok(Some((src_rate, channels))) => {
                    out.clear();
                    resample.push(&buf, channels, src_rate, &mut out);
                    if !push(shared, generation, &out, rate) {
                        break false;
                    }
                }
                Ok(None) => break true,
                Err(e) => {
                    eprintln!("sound: {e:#}");
                    break true;
                }
            }
        };
        drop(source);
        if complete {
            let mut st = shared.state.lock().unwrap();
            if st.generation == generation {
                st.input_done = true;
            }
        }
        // wait for the next pass (or the end)
        loop {
            if shared.stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            if shared.state.lock().unwrap().generation != generation {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Adds sound to the queue, waiting while it is full. False when the pass was cancelled.
fn push(shared: &Shared, generation: u64, samples: &[f32], rate: u32) -> bool {
    let limit = rate as usize * 2 * BUFFER_SECS;
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return false;
        }
        {
            let mut st = shared.state.lock().unwrap();
            if st.generation != generation {
                return false;
            }
            if st.queue.len() < limit {
                st.queue.extend(samples);
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// -- output ------------------------------------------------------------------------------

fn open_output(shared: &Arc<Shared>) -> Result<(cpal::Stream, u32)> {
    let device = cpal::default_host().default_output_device().ok_or_else(|| anyhow!("no sound output device"))?;
    let config = device.default_output_config().context("cannot use the sound output device")?;
    let rate = config.sample_rate();
    let format = config.sample_format();
    let config: cpal::StreamConfig = config.into();
    let stream = match format {
        cpal::SampleFormat::F32 => build::<f32>(&device, config, shared),
        cpal::SampleFormat::F64 => build::<f64>(&device, config, shared),
        cpal::SampleFormat::I16 => build::<i16>(&device, config, shared),
        cpal::SampleFormat::I32 => build::<i32>(&device, config, shared),
        cpal::SampleFormat::U16 => build::<u16>(&device, config, shared),
        cpal::SampleFormat::U8 => build::<u8>(&device, config, shared),
        f => Err(anyhow!("unsupported sample format {f}")),
    }?;
    stream.play().context("cannot start the sound output")?;
    shared.state.lock().unwrap().rate = rate;
    Ok((stream, rate))
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream> {
    let channels = config.channels as usize;
    let s = shared.clone();
    let data = move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
        let ts = info.timestamp();
        let mut st = s.state.lock().unwrap();
        st.latency = ts.playback.duration_since(ts.callback).min(Duration::from_millis(500));
        let mut taken = 0u64;
        for frame in data.chunks_mut(channels) {
            let (l, r) = if st.playing && st.queue.len() >= 2 {
                taken += 1;
                (st.queue.pop_front().unwrap(), st.queue.pop_front().unwrap())
            } else {
                (0.0, 0.0)
            };
            if channels == 1 {
                frame[0] = T::from_sample((l + r) * 0.5);
            } else {
                for (i, x) in frame.iter_mut().enumerate() {
                    *x = T::from_sample(match i {
                        0 => l,
                        1 => r,
                        _ => 0.0,
                    });
                }
            }
        }
        if st.playing {
            st.played += taken;
            st.last_callback = Some((Instant::now(), taken));
            if st.input_done && st.queue.is_empty() && st.drained_at.is_none() {
                st.drained_at = Some(Instant::now());
            }
        }
    };
    let err = |e: cpal::Error| eprintln!("sound output: {e}");
    device.build_output_stream(config, data, err, None).context("cannot open the sound output")
}

/// Linear interpolation to the device rate, mixed down to stereo.
struct Resampler {
    rate: u32,
    /// Position in source frames; 0 = `prev`, 1 = first frame of the next chunk
    pos: f64,
    prev: [f32; 2],
    frames: Vec<[f32; 2]>,
}

impl Resampler {
    fn new(rate: u32) -> Self {
        Resampler { rate, pos: 1.0, prev: [0.0; 2], frames: Vec::new() }
    }

    fn push(&mut self, input: &[f32], channels: usize, src_rate: u32, out: &mut Vec<f32>) {
        if channels == 0 || src_rate == 0 {
            return;
        }
        self.frames.clear();
        self.frames.extend(input.chunks_exact(channels).map(|f| match channels {
            1 => [f[0], f[0]],
            2 => [f[0], f[1]],
            // surround: front left/right plus some of the centre
            _ => [f[0] + 0.7 * f[2], f[1] + 0.7 * f[2]],
        }));
        let n = self.frames.len();
        if n == 0 {
            return;
        }
        let step = src_rate as f64 / self.rate as f64;
        while self.pos < n as f64 {
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            let a = if i == 0 { self.prev } else { self.frames[i - 1] };
            let b = self.frames[i];
            out.push((a[0] + (b[0] - a[0]) * t).clamp(-1.0, 1.0));
            out.push((a[1] + (b[1] - a[1]) * t).clamp(-1.0, 1.0));
            self.pos += step;
        }
        self.pos -= n as f64;
        self.prev = self.frames[n - 1];
    }
}

// -- sources -----------------------------------------------------------------------------

enum Backend {
    #[cfg(target_os = "macos")]
    Builtin,
    Ffmpeg(PathBuf),
}

enum Source {
    #[cfg(target_os = "macos")]
    Builtin(Box<Builtin>),
    Ffmpeg(Ffmpeg),
}

impl Source {
    /// Next piece of sound (interleaved) and its sample rate and channel count; None at the end.
    fn next_chunk(&mut self, buf: &mut Vec<f32>) -> Result<Option<(u32, usize)>> {
        match self {
            #[cfg(target_os = "macos")]
            Source::Builtin(b) => b.next_chunk(buf),
            Source::Ffmpeg(f) => f.next_chunk(buf),
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn open_source(path: &Path, rate: u32) -> Result<(Source, Backend)> {
    let ffmpeg =
        crate::video::find_ffmpeg().ok_or_else(|| anyhow!("sound needs ffmpeg; {}", crate::video::INSTALL_FFMPEG))?;
    let f = Ffmpeg::start(&ffmpeg, path, rate)?;
    Ok((Source::Ffmpeg(f), Backend::Ffmpeg(ffmpeg)))
}

#[cfg(target_os = "macos")]
fn open_source(path: &Path, rate: u32) -> Result<(Source, Backend)> {
    match Builtin::open(path) {
        Ok(b) => Ok((Source::Builtin(Box::new(b)), Backend::Builtin)),
        Err(e) if e.is::<NoSoundtrack>() => Err(e),
        Err(builtin_err) => {
            let ffmpeg = crate::video::find_ffmpeg().ok_or_else(|| {
                builtin_err.context("this sound format needs ffmpeg; install ffmpeg (or put it next to this program)")
            })?;
            let f = Ffmpeg::start(&ffmpeg, path, rate)?;
            Ok((Source::Ffmpeg(f), Backend::Ffmpeg(ffmpeg)))
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct NoSoundtrack;

#[cfg(target_os = "macos")]
impl std::fmt::Display for NoSoundtrack {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("the video has no sound")
    }
}

#[cfg(target_os = "macos")]
impl std::error::Error for NoSoundtrack {}

#[cfg(target_os = "macos")]
struct Builtin {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    track: u32,
}

#[cfg(target_os = "macos")]
impl Builtin {
    fn open(path: &Path) -> Result<Self> {
        use symphonia::core::formats::probe::Hint;
        use symphonia::core::formats::TrackType;
        use symphonia::core::io::MediaSourceStream;
        let file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(&hint, mss, Default::default(), Default::default())
            .map_err(|e| anyhow!("unsupported container ({e})"))?;
        let track = format.default_track(TrackType::Audio).ok_or(NoSoundtrack)?;
        let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or(NoSoundtrack)?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &Default::default())
            .map_err(|e| anyhow!("unsupported sound format ({e})"))?;
        let track = track.id;
        Ok(Builtin { format, decoder, track })
    }

    fn next_chunk(&mut self, buf: &mut Vec<f32>) -> Result<Option<(u32, usize)>> {
        use symphonia::core::errors::Error;
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(None),
                Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(anyhow!("cannot read the sound ({e})")),
            };
            if packet.track_id != self.track {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio) => {
                    let spec = audio.spec();
                    let (rate, channels) = (spec.rate(), spec.channels().count());
                    audio.copy_to_vec_interleaved(buf);
                    return Ok(Some((rate, channels)));
                }
                Err(Error::DecodeError(_)) => continue, // skip a broken piece
                Err(e) => return Err(anyhow!("cannot decode the sound ({e})")),
            }
        }
    }
}

struct Ffmpeg {
    child: Child,
    out: ChildStdout,
    rate: u32,
    bytes: Vec<u8>,
}

impl Ffmpeg {
    /// ffmpeg converts to stereo at the device rate.
    fn start(ffmpeg: &Path, path: &Path, rate: u32) -> Result<Self> {
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-v", "error", "-nostdin", "-i"])
            .arg(path)
            .args(["-vn", "-sn", "-dn", "-ac", "2", "-ar"])
            .arg(rate.to_string())
            .args(["-f", "f32le", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::video::no_window(&mut cmd);
        let mut child = cmd.spawn().context("cannot start ffmpeg")?;
        let out = child.stdout.take().expect("piped");
        Ok(Ffmpeg { child, out, rate, bytes: vec![0; 4096 * 8] })
    }

    fn next_chunk(&mut self, buf: &mut Vec<f32>) -> Result<Option<(u32, usize)>> {
        let mut filled = 0;
        while filled < self.bytes.len() {
            match self.out.read(&mut self.bytes[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        let whole = filled / 8 * 8; // whole stereo frames
        if whole == 0 {
            return Ok(None);
        }
        buf.clear();
        buf.extend(self.bytes[..whole].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
        Ok(Some((self.rate, 2)))
    }
}

impl Drop for Ffmpeg {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
