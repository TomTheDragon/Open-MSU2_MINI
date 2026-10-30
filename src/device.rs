//! Driver for the MSU2 / MSU2_MINI USB screen (CH32X035 + ST7735S, 160x80 pixels).
//!
//! The device is a USB CDC-ACM serial port. Every command is 6 bytes long.
//! See README.md for the protocol table.

use anyhow::{anyhow, bail, Context, Result};
use serialport::{SerialPort, SerialPortType};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

pub const WIDTH: usize = 160;
pub const HEIGHT: usize = 80;
pub const USB_VID: u16 = 0x1A86;
pub const USB_PID: u16 = 0xFE0C;

/// Flash layout used by the stock firmware (1 page = 256 bytes).
pub const FLASH_ANIMATION: u32 = 0; // 36 frames x 100 pages, played when no PC is connected
pub const FLASH_CLOCK_BG: u32 = 3826;
pub const FLASH_ALBUM: u32 = 3926;
pub const FRAME_PAGES: u32 = (WIDTH * HEIGHT * 2 / 256) as u32; // 100

/// A frame in device format: RGB565, one u16 per pixel, row-major.
pub type Frame565 = Vec<u16>;

/// Size of the drawing area for a rotation: 160x80, or 80x160 (portrait) for 90/270 degrees.
pub fn canvas_size(rotation: u16) -> (usize, usize) {
    if rotation % 180 == 90 {
        (HEIGHT, WIDTH)
    } else {
        (WIDTH, HEIGHT)
    }
}

/// 180 and 270 degrees use the panel's own 180-degree flip.
pub fn hw_flip(rotation: u16) -> bool {
    rotation == 180 || rotation == 270
}

/// Turn a canvas (see `canvas_size`) into panel orientation. Portrait canvases are
/// rotated 90 degrees clockwise in software; the panel's flip does the rest.
pub fn orient(canvas: &[u16], rotation: u16) -> Frame565 {
    if rotation % 180 != 90 {
        return canvas.to_vec();
    }
    let (cw, ch) = (HEIGHT, WIDTH); // portrait canvas: 80 wide, 160 tall
    let mut out = vec![0u16; WIDTH * HEIGHT];
    for (dy, row) in out.as_chunks_mut::<WIDTH>().0.iter_mut().enumerate() {
        for (dx, px) in row.iter_mut().enumerate() {
            // clockwise: panel (dx, dy) shows canvas (dy, ch - 1 - dx)
            *px = canvas[(ch - 1 - dx) * cw + dy];
        }
    }
    out
}

pub fn rgb565(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
}

/// Find serial ports that belong to an MSU2 device (best match first).
pub fn find_ports() -> Vec<String> {
    let mut exact = Vec::new();
    let mut other = Vec::new();
    if let Ok(ports) = serialport::available_ports() {
        for p in ports {
            match &p.port_type {
                SerialPortType::UsbPort(u) if u.vid == USB_VID && u.pid == USB_PID => exact.push(p.port_name),
                SerialPortType::UsbPort(_) => other.push(p.port_name),
                _ => {}
            }
        }
    }
    // macOS lists both tty.* and cu.*; cu.* is the one to use for outgoing connections.
    exact.sort_by_key(|n| !n.contains("/cu."));
    if exact.is_empty() {
        other.retain(|n| !n.contains("/tty.") || !cfg!(target_os = "macos"));
        exact = other;
    }
    exact
}

pub struct Device {
    port: Box<dyn SerialPort>,
    pub port_name: String,
    pub version: Option<u8>,
    /// Output buffer: commands are collected and sent in one write.
    out: Vec<u8>,
    /// What the device's 256-byte block buffer currently holds (None = unknown).
    block: Option<[u32; 64]>,
    pub stats: Stats,
    /// Use the vendor tool's encoding (full frames, fill + words per block). For benchmarks.
    pub legacy: bool,
}

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub bytes: u64,
    pub frames: u64,
}

impl Device {
    pub fn open(port: Option<&str>) -> Result<Device> {
        let candidates = match port {
            Some(p) => vec![p.to_string()],
            None => find_ports(),
        };
        if candidates.is_empty() {
            bail!("no MSU2 device found");
        }
        let mut last = anyhow!("no MSU2 device found");
        for name in candidates {
            match Self::try_open(&name) {
                Ok(d) => return Ok(d),
                Err(e) => last = e.context(format!("port {name}")),
            }
        }
        Err(last)
    }

    fn try_open(name: &str) -> Result<Device> {
        // The timeout also applies to writes: a full frame can take a while when the
        // panel is busy, so keep it generous. Reads only fetch bytes that are waiting.
        let builder = serialport::new(name, 19200).timeout(Duration::from_secs(3));
        #[cfg(unix)]
        let port: Box<dyn SerialPort> = {
            let mut p = serialport::TTYPort::open(&builder).with_context(|| format!("cannot open {name}"))?;
            // Keep a second program from writing into the same device.
            p.set_exclusive(true).context("device is used by another program")?;
            Box::new(p)
        };
        #[cfg(not(unix))] // COM ports are exclusive on Windows anyway
        let port = builder.open().with_context(|| format!("cannot open {name}"))?;
        let mut dev = Device {
            port,
            port_name: name.to_string(),
            version: None,
            out: Vec::with_capacity(64 * 1024),
            block: None,
            stats: Stats::default(),
            legacy: false,
        };
        dev.handshake()?;
        Ok(dev)
    }

    fn handshake(&mut self) -> Result<()> {
        // While offline the device announces "\0MSNvv" every 100 ms (vv = firmware version).
        // Right after a previous session it stays silent for a few seconds, which is fine.
        let mut buf = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            buf.extend(self.read_available()?);
            if let Some(i) = find(&buf, b"\0MSN") {
                if buf.len() >= i + 6 && buf[i + 4].is_ascii_digit() && buf[i + 5].is_ascii_digit() {
                    self.version = Some((buf[i + 4] - b'0') * 10 + (buf[i + 5] - b'0'));
                    break;
                }
            }
        }
        self.port.clear(serialport::ClearBuffer::Input).ok();
        self.port.write_all(b"\0MSNCN")?;
        let reply = self.read_for(Duration::from_millis(300), 6)?;
        if find(&reply, b"\0MSNCN").is_none() {
            bail!("unexpected handshake reply {:02x?}", reply);
        }
        Ok(())
    }

    fn read_available(&mut self) -> Result<Vec<u8>> {
        let n = self.port.bytes_to_read()? as usize;
        if n == 0 {
            std::thread::sleep(Duration::from_millis(1));
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; n];
        match self.port.read(&mut buf) {
            Ok(k) => {
                buf.truncate(k);
                Ok(buf)
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn read_for(&mut self, timeout: Duration, want: usize) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut buf = Vec::new();
        while Instant::now() < deadline && buf.len() < want {
            buf.extend(self.read_available()?);
        }
        Ok(buf)
    }

    /// Wait for a 6-byte reply whose first byte is `kind`, skipping LCD acks.
    fn reply(&mut self, kind: u8, timeout: Duration) -> Result<[u8; 6]> {
        let deadline = Instant::now() + timeout;
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.extend(self.read_available()?);
            while buf.len() >= 6 {
                let msg: [u8; 6] = buf[..6].try_into().unwrap();
                buf.drain(..6);
                if msg[0] == kind {
                    return Ok(msg);
                }
                if msg[..4] == *b"\0MSN" {
                    bail!("device dropped the connection (timeout)");
                }
            }
            if Instant::now() > deadline {
                bail!("device did not answer");
            }
        }
    }

    // -- command buffer -------------------------------------------------------
    fn cmd(&mut self, b: [u8; 6]) {
        self.out.extend_from_slice(&b);
    }

    /// Send all buffered commands.
    pub fn flush(&mut self) -> Result<()> {
        if !self.out.is_empty() {
            self.port.write_all(&self.out)?;
            self.stats.bytes += self.out.len() as u64;
            self.out.clear();
        }
        Ok(())
    }

    fn xy(&mut self, x: usize, y: usize) {
        self.cmd([2, 0, (x >> 8) as u8, x as u8, (y >> 8) as u8, y as u8]);
    }
    fn size(&mut self, w: usize, h: usize) {
        self.cmd([2, 1, (w >> 8) as u8, w as u8, (h >> 8) as u8, h as u8]);
    }

    // -- LCD ------------------------------------------------------------------
    /// Rotate the picture by 180 degrees.
    pub fn set_flipped(&mut self, flipped: bool) -> Result<()> {
        self.cmd([2, 3, 10, flipped as u8, 0, 0]);
        self.flush()
    }

    pub fn show_flash_image(&mut self, page: u32) -> Result<()> {
        self.xy(0, 0);
        self.size(WIDTH, HEIGHT);
        self.cmd([2, 3, 0, (page >> 8) as u8, page as u8, 0]);
        self.block = None; // the firmware may use the block buffer to read flash
        self.flush()
    }

    /// Queue the pixels of the rectangle (x, y, w, h) taken from a full frame.
    /// Uses the cheapest encoding per 256-byte block, see `encode_block`.
    pub fn queue_rect(&mut self, frame: &[u16], x: usize, y: usize, w: usize, h: usize) {
        self.xy(x, y);
        self.size(w, h);
        self.cmd([2, 3, 7, 0, 0, 0]); // start pixel transfer (acknowledged, ack is skipped later)
        let mut bytes = Vec::with_capacity(w * h * 2);
        for row in y..y + h {
            for &px in &frame[row * WIDTH + x..row * WIDTH + x + w] {
                bytes.extend_from_slice(&px.to_be_bytes());
            }
        }
        for chunk in bytes.chunks(256) {
            let mut words = [0u32; 64];
            let mut padded = [0xFFu8; 256];
            padded[..chunk.len()].copy_from_slice(chunk);
            for (i, w) in words.iter_mut().enumerate() {
                *w = u32::from_be_bytes(padded[i * 4..i * 4 + 4].try_into().unwrap());
            }
            let used_words = chunk.len().div_ceil(4);
            self.encode_block(&words, used_words);
            if chunk.len() == 256 {
                self.cmd([2, 3, 8, 1, 0, 0]);
            } else {
                self.cmd([2, 3, 8, 0, chunk.len() as u8, 0]);
            }
        }
    }

    /// The device keeps a 256-byte block buffer that survives between blocks. Each block
    /// is sent either as "keep buffer + changed words" or as "fill with the most common
    /// 32-bit word + other words", whichever is fewer bytes.
    fn encode_block(&mut self, words: &[u32; 64], used: usize) {
        let words = &words[..used];
        // most common word
        let mut sorted: Vec<u32> = words.to_vec();
        sorted.sort_unstable();
        let (mut best, mut best_n, mut i) = (sorted[0], 0usize, 0usize);
        while i < sorted.len() {
            let mut j = i;
            while j < sorted.len() && sorted[j] == sorted[i] {
                j += 1;
            }
            if j - i > best_n {
                best_n = j - i;
                best = sorted[i];
            }
            i = j;
        }
        let fill_cost = 1 + (used - best_n);
        let keep_cost = match &self.block {
            _ if self.legacy => usize::MAX,
            Some(cur) => words.iter().zip(cur.iter()).filter(|(a, b)| a != b).count(),
            None => usize::MAX,
        };
        let mut cur = self.block.unwrap_or([0; 64]);
        if fill_cost < keep_cost {
            self.cmd([2, 4, (best >> 24) as u8, (best >> 16) as u8, (best >> 8) as u8, best as u8]);
            cur = [best; 64];
        }
        for (i, &w) in words.iter().enumerate() {
            if cur[i] != w {
                let b = w.to_be_bytes();
                self.cmd([4, i as u8, b[0], b[1], b[2], b[3]]);
                cur[i] = w;
            }
        }
        self.block = Some(cur);
    }

    /// Send a whole frame.
    pub fn draw_full(&mut self, frame: &[u16]) -> Result<()> {
        self.queue_rect(frame, 0, 0, WIDTH, HEIGHT);
        self.stats.frames += 1;
        self.flush()
    }

    /// Send only the parts of `frame` that differ from `prev` (what the screen shows now).
    pub fn draw_diff(&mut self, frame: &[u16], prev: Option<&[u16]>) -> Result<bool> {
        let Some(prev) = prev.filter(|_| !self.legacy) else {
            self.draw_full(frame)?;
            return Ok(true);
        };
        let rects = dirty_rects(frame, prev);
        if rects.is_empty() {
            return Ok(false);
        }
        for (x, y, w, h) in rects {
            self.queue_rect(frame, x, y, w, h);
        }
        self.stats.frames += 1;
        self.flush()?;
        Ok(true)
    }

    // -- flash ------------------------------------------------------------------
    pub fn erase_flash(&mut self, page: u32, count: u32) -> Result<()> {
        self.flush()?;
        self.drain();
        self.cmd([3, 2, (page >> 8) as u8, page as u8, (count >> 8) as u8, count as u8]);
        self.flush()?;
        self.block = None;
        self.reply(3, Duration::from_secs(20)).context("flash erase")?;
        Ok(())
    }

    pub fn write_flash_page(&mut self, page: u32, data: &[u8]) -> Result<()> {
        let mut words = [0u32; 64];
        for (i, w) in words.iter_mut().enumerate() {
            *w = u32::from_be_bytes(data[i * 4..i * 4 + 4].try_into().unwrap());
        }
        self.encode_block(&words, 64);
        self.cmd([3, 3, (page >> 16) as u8, (page >> 8) as u8, page as u8, 1]);
        self.block = None; // unknown whether the firmware reuses the buffer while writing
        self.flush()?;
        self.reply(3, Duration::from_secs(3)).with_context(|| format!("flash write page {page}"))?;
        Ok(())
    }

    pub fn write_flash(&mut self, page: u32, data: &[u8], mut progress: impl FnMut(usize, usize)) -> Result<()> {
        let pages = data.len().div_ceil(256);
        let mut padded = data.to_vec();
        padded.resize(pages * 256, 0xFF);
        self.erase_flash(page, pages as u32)?;
        for i in 0..pages {
            self.write_flash_page(page + i as u32, &padded[i * 256..(i + 1) * 256])?;
            progress(i + 1, pages);
        }
        Ok(())
    }

    // -- misc ----------------------------------------------------------------------
    /// Throw away pending acks.
    pub fn drain(&mut self) {
        let _ = self.read_available();
    }

    /// Read an ADC channel. Channel 9 is the push button. Doubles as keep-alive: the
    /// device drops back to offline mode after ~5 s without traffic.
    pub fn read_adc(&mut self, ch: u8) -> Result<u16> {
        self.cmd([8, ch, 0, 0, 0, 0]);
        self.flush()?;
        let r = self.reply(8, Duration::from_millis(1000))?;
        Ok(u16::from_be_bytes([r[4], r[5]]))
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Rectangles covering all changed pixels. Changed rows are grouped into bands; each
/// band gets its own column range. Small gaps are merged because every rectangle costs
/// a few commands and a partial block.
pub fn dirty_rects(frame: &[u16], prev: &[u16]) -> Vec<(usize, usize, usize, usize)> {
    let mut rows: Vec<Option<(usize, usize)>> = Vec::with_capacity(HEIGHT);
    for y in 0..HEIGHT {
        let a = &frame[y * WIDTH..(y + 1) * WIDTH];
        let b = &prev[y * WIDTH..(y + 1) * WIDTH];
        let first = a.iter().zip(b).position(|(p, q)| p != q);
        rows.push(first.map(|f| {
            let last = WIDTH - 1 - a.iter().rev().zip(b.iter().rev()).position(|(p, q)| p != q).unwrap();
            (f, last)
        }));
    }
    const GAP: usize = 4; // rows of unchanged pixels that are cheaper to resend than a new rect
    let mut out = Vec::new();
    let mut y = 0;
    while y < HEIGHT {
        let Some((mut x0, mut x1)) = rows[y] else {
            y += 1;
            continue;
        };
        let y0 = y;
        let mut y1 = y;
        let mut k = y + 1;
        while k < HEIGHT && k <= y1 + GAP {
            if let Some((a, b)) = rows[k] {
                x0 = x0.min(a);
                x1 = x1.max(b);
                y1 = k;
            }
            k += 1;
        }
        out.push((x0, y0, x1 - x0 + 1, y1 - y0 + 1));
        y = y1 + 1;
    }
    out
}
