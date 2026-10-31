//! Things drawn on the PC and sent to the panel.
//!
//! Everything is drawn on a "canvas": 160x80, or 80x160 when the picture is rotated by
//! 90/270 degrees (see `device::canvas_size`). The controller turns it into panel format.

use crate::device::{Frame565, HEIGHT, WIDTH};
use crate::scale::{rgb_to_565, Scaler, Scaling};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use anyhow::{Context, Result};
use image::{AnimationDecoder, GrayImage, Luma, Rgb, RgbImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_hollow_rect_mut, draw_text_mut, text_size};
use imageproc::rect::Rect;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

pub type Size = (usize, usize);

static BOLD: &[u8] = include_bytes!("../assets/DejaVuSans-Bold.ttf");
static REGULAR: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");

pub fn bold() -> &'static FontRef<'static> {
    static F: OnceLock<FontRef<'static>> = OnceLock::new();
    F.get_or_init(|| FontRef::try_from_slice(BOLD).unwrap())
}
pub fn regular() -> &'static FontRef<'static> {
    static F: OnceLock<FontRef<'static>> = OnceLock::new();
    F.get_or_init(|| FontRef::try_from_slice(REGULAR).unwrap())
}

#[derive(Clone, Copy)]
pub enum Anchor {
    Left,
    Center,
    Right,
}

/// Text style for `text`.
#[derive(Clone, Copy)]
pub struct Style<'a> {
    pub size: f32,
    pub font: &'a FontRef<'static>,
    pub color: [u8; 3],
    pub anchor: Anchor,
    pub outline: bool,
}

impl<'a> Style<'a> {
    pub fn new(size: f32, font: &'a FontRef<'static>, color: [u8; 3]) -> Self {
        Style { size, font, color, anchor: Anchor::Left, outline: false }
    }
    pub fn anchor(mut self, a: Anchor) -> Self {
        self.anchor = a;
        self
    }
    pub fn outline(mut self, o: bool) -> Self {
        self.outline = o;
        self
    }
}

pub fn text_width(s: &str, size: f32, font: &FontRef) -> i32 {
    text_size(PxScale::from(size), font, s).0 as i32
}

/// Top-left corner for drawing `s` so that its vertical centre is at `cy`.
fn text_origin(s: &str, x: i32, cy: i32, st: &Style) -> (i32, i32) {
    let scale = PxScale::from(st.size);
    let w = text_size(scale, st.font, s).0 as i32;
    let x = match st.anchor {
        Anchor::Left => x,
        Anchor::Center => x - w / 2,
        Anchor::Right => x - w,
    };
    // centre on the height of digits/capitals, independent of the characters used
    let sf = st.font.as_scaled(scale);
    let cap = sf.ascent() * 0.72;
    (x, cy - (sf.ascent() - cap / 2.0).round() as i32)
}

/// Draw text with its vertical centre at `cy`.
pub fn text(img: &mut RgbImage, s: &str, x: i32, cy: i32, st: Style) {
    let scale = PxScale::from(st.size);
    let (x, y) = text_origin(s, x, cy, &st);
    if st.outline {
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)] {
            draw_text_mut(img, Rgb([0, 0, 0]), x + dx, y + dy, scale, st.font, s);
        }
    }
    draw_text_mut(img, Rgb(st.color), x, y, scale, st.font, s);
}

/// Draw text over a picture: every pixel of the text gets the opposite colour of what is
/// underneath it, and the text gets a 1-pixel black outline. `st.color` is not used.
pub fn text_inverted(img: &mut RgbImage, s: &str, x: i32, cy: i32, st: Style) {
    let (w, h) = img.dimensions();
    let (x, y) = text_origin(s, x, cy, &st);
    let mut mask = GrayImage::new(w, h);
    draw_text_mut(&mut mask, Luma([255]), x, y, PxScale::from(st.size), st.font, s);
    let covered = |px: i32, py: i32| {
        px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h && mask.get_pixel(px as u32, py as u32).0[0] > 0
    };
    for py in 0..h as i32 {
        for px in 0..w as i32 {
            let a = mask.get_pixel(px as u32, py as u32).0[0] as u32;
            let near = a > 0 || (-1..=1).any(|dy| (-1..=1).any(|dx| covered(px + dx, py + dy)));
            if !near {
                continue;
            }
            // inverted colour, blended over the black outline by the glyph's coverage
            let p = img.get_pixel_mut(px as u32, py as u32);
            p.0 = p.0.map(|c| ((255 - c as u32) * a / 255) as u8);
        }
    }
}

pub fn blank(size: Size) -> RgbImage {
    RgbImage::new(size.0 as u32, size.1 as u32)
}

pub trait Screen: Send {
    /// Render the next canvas; returns it and how long to wait before the next one.
    fn render(&mut self) -> (Frame565, Duration);
}

// ---------------------------------------------------------------------------------
// Clock

pub struct ClockScreen {
    bg: Option<RgbImage>,
    size: Size,
}

impl ClockScreen {
    pub fn new(background: Option<&Path>, size: Size) -> Self {
        let bg = background.and_then(|p| image::open(p).ok()).map(|img| {
            let f = Scaler::high_quality().sized(size).rgba_image(&img.to_rgba8(), Scaling::Crop);
            frame_to_rgb(&f, size)
        });
        ClockScreen { bg, size }
    }
}

impl Screen for ClockScreen {
    fn render(&mut self) -> (Frame565, Duration) {
        let now = chrono::Local::now();
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let mut img = self.bg.clone().unwrap_or_else(|| blank(self.size));
        let outline = self.bg.is_some();
        let color = [255, 220, 60];
        let date = Style::new(12.0, regular(), [220, 220, 220]).anchor(Anchor::Center).outline(outline);
        let big = |size| Style::new(size, bold(), color).anchor(Anchor::Center).outline(outline);
        if w >= h {
            text(&mut img, &now.format("%H:%M").to_string(), w / 2, 32, big(46.0));
            text(&mut img, &now.format("%a %d.%m.%Y").to_string(), w / 2, 66, date);
        } else {
            // portrait: hours above minutes
            text(&mut img, &now.format("%H").to_string(), w / 2, 36, big(50.0));
            text(&mut img, &now.format("%M").to_string(), w / 2, 92, big(50.0));
            text(&mut img, &now.format("%a %d.%m.").to_string(), w / 2, 130, date);
            text(&mut img, &now.format("%Y").to_string(), w / 2, 146, date);
        }
        let sec = now.format("%S").to_string().parse::<u32>().unwrap_or(0);
        let bar = (w as u32 - 1) * sec / 59;
        if bar > 0 {
            draw_filled_rect_mut(&mut img, Rect::at(0, h - 2).of_size(bar, 2), Rgb(color));
        }
        let ms = 1000 - now.timestamp_subsec_millis().min(999);
        (rgb_to_565(&img), Duration::from_millis(ms as u64 + 5))
    }
}

// ---------------------------------------------------------------------------------
// Pictures and animations

pub struct ImageScreen {
    frames: Vec<(Frame565, Duration)>,
    i: usize,
}

/// Load a picture or an animation (GIF / WebP / PNG) as canvas-sized frames.
pub fn load_frames(path: &Path, fit: Scaling, size: Size) -> Result<Vec<(Frame565, Duration)>> {
    let mut scaler = Scaler::high_quality().sized(size);
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let fmt = image::guess_format(&data).ok();
    let cur = std::io::Cursor::new(&data);
    let anim: Option<Vec<image::Frame>> = match fmt {
        Some(image::ImageFormat::Gif) => {
            image::codecs::gif::GifDecoder::new(cur).ok().and_then(|d| d.into_frames().collect_frames().ok())
        }
        Some(image::ImageFormat::WebP) => image::codecs::webp::WebPDecoder::new(cur)
            .ok()
            .filter(|d| d.has_animation())
            .and_then(|d| d.into_frames().collect_frames().ok()),
        Some(image::ImageFormat::Png) => image::codecs::png::PngDecoder::new(cur)
            .ok()
            .filter(|d| d.is_apng().unwrap_or(false))
            .and_then(|d| d.apng().ok())
            .and_then(|d| d.into_frames().collect_frames().ok()),
        _ => None,
    };
    if let Some(frames) = anim.filter(|f| !f.is_empty()) {
        return Ok(frames
            .into_iter()
            .map(|f| {
                let (n, d) = f.delay().numer_denom_ms();
                let ms = n.checked_div(d).map_or(100, |ms| ms.max(20));
                (scaler.rgba_image(f.buffer(), fit), Duration::from_millis(ms as u64))
            })
            .collect());
    }
    let img =
        image::load_from_memory(&data).with_context(|| format!("{} is not a supported picture", path.display()))?;
    Ok(vec![(scaler.rgba_image(&img.to_rgba8(), fit), Duration::from_secs(1))])
}

impl ImageScreen {
    pub fn new(path: &Path, fit: Scaling, size: Size) -> Result<Self> {
        Ok(ImageScreen { frames: load_frames(path, fit, size)?, i: 0 })
    }
}

impl Screen for ImageScreen {
    fn render(&mut self) -> (Frame565, Duration) {
        let (f, d) = self.frames[self.i].clone();
        self.i = (self.i + 1) % self.frames.len();
        (f, d)
    }
}

// ---------------------------------------------------------------------------------

pub fn frame_to_rgb(f: &[u16], size: Size) -> RgbImage {
    let mut img = blank(size);
    for (i, p) in img.pixels_mut().enumerate() {
        let v = f[i];
        let r = ((v >> 11) & 31) as u8;
        let g = ((v >> 5) & 63) as u8;
        let b = (v & 31) as u8;
        *p = Rgb([(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]);
    }
    img
}

/// Centered lines of text. Long lines are split on spaces in portrait.
pub fn message(lines: &[&str], color: [u8; 3], size: Size) -> Frame565 {
    let mut img = blank(size);
    let narrow = size.0 < size.1;
    let lines: Vec<&str> = if narrow { lines.iter().flat_map(|l| l.split(' ')).collect() } else { lines.to_vec() };
    let fs = if narrow { 11.0 } else { 12.0 };
    let y0 = (size.1 as i32 - 16 * lines.len() as i32) / 2 + 8;
    for (i, l) in lines.iter().enumerate() {
        text(&mut img, l, size.0 as i32 / 2, y0 + 16 * i as i32, Style::new(fs, bold(), color).anchor(Anchor::Center));
    }
    rgb_to_565(&img)
}

/// Fixed test pattern in panel orientation (not rotated).
pub fn test_pattern() -> Frame565 {
    let mut img = blank((WIDTH, HEIGHT));
    for (i, c) in [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]].iter().enumerate() {
        draw_filled_rect_mut(&mut img, Rect::at(i as i32 * 40, 0).of_size(40, 40), Rgb(*c));
    }
    for x in 0..WIDTH as u32 {
        let v = (x * 255 / (WIDTH as u32 - 1)) as u8;
        draw_filled_rect_mut(&mut img, Rect::at(x as i32, 40).of_size(1, 20), Rgb([v, v, v]));
    }
    text(&mut img, "MSU2 test ↑ top", 80, 70, Style::new(11.0, bold(), [255, 255, 0]).anchor(Anchor::Center));
    draw_hollow_rect_mut(&mut img, Rect::at(0, 0).of_size(WIDTH as u32, HEIGHT as u32), Rgb([255, 0, 255]));
    rgb_to_565(&img)
}
