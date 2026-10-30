//! Scaling pictures and screen captures down to the 160x80 panel.

use crate::device::{Frame565, HEIGHT, WIDTH};
use fast_image_resize::{images::Image, images::ImageRef, FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::RgbImage;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scaling {
    /// Fill the panel, cut off what does not fit (default)
    #[default]
    Crop,
    /// Show everything, black bars where needed
    Fit,
    /// Distort to fill the panel
    Stretch,
}

impl Scaling {
    pub const ALL: [Scaling; 3] = [Scaling::Crop, Scaling::Fit, Scaling::Stretch];
    pub fn name(self) -> &'static str {
        match self {
            Scaling::Crop => "crop",
            Scaling::Fit => "fit",
            Scaling::Stretch => "stretch",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Rgba,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Bgra,
}

/// A rectangle in source pixels: x, y, width, height.
pub type Region = (u32, u32, u32, u32);

pub struct Scaler {
    resizer: Resizer,
    compact: Vec<u8>,
    pub quality: bool,
    /// Output size: the panel (160x80) or a portrait canvas (80x160)
    pub out: (usize, usize),
}

impl Default for Scaler {
    fn default() -> Self {
        Scaler { resizer: Resizer::new(), compact: Vec::new(), quality: false, out: (WIDTH, HEIGHT) }
    }
}

impl Scaler {
    /// Slower, sharper scaling for still pictures.
    pub fn high_quality() -> Self {
        Scaler { quality: true, ..Default::default() }
    }

    pub fn sized(mut self, out: (usize, usize)) -> Self {
        self.out = out;
        self
    }

    /// Scale a 4-byte-per-pixel source (possibly with row padding) to an `out`-sized frame.
    #[allow(clippy::too_many_arguments)]
    pub fn panel_frame(
        &mut self,
        src: &[u8],
        w: u32,
        h: u32,
        stride: usize,
        order: Order,
        region: Option<Region>,
        fit: Scaling,
    ) -> Option<Frame565> {
        if w == 0 || h == 0 {
            return None;
        }
        let row = w as usize * 4;
        let data: &[u8] = if stride == row {
            &src[..row * h as usize]
        } else {
            self.compact.clear();
            for y in 0..h as usize {
                self.compact.extend_from_slice(&src[y * stride..y * stride + row]);
            }
            &self.compact
        };
        // fast_image_resize wants 4-byte aligned data for U8x4
        let aligned;
        let data = if (data.as_ptr() as usize).is_multiple_of(4) {
            data
        } else {
            aligned = data.to_vec();
            &aligned
        };
        let img = ImageRef::new(w, h, data, PixelType::U8x4).ok()?;

        let (rx, ry, rw, rh) = match region {
            Some((x, y, rw, rh)) => {
                let x = x.min(w - 1);
                let y = y.min(h - 1);
                (x, y, rw.clamp(1, w - x), rh.clamp(1, h - y))
            }
            None => (0, 0, w, h),
        };
        let alg = if self.quality {
            ResizeAlg::Convolution(FilterType::Lanczos3)
        } else {
            ResizeAlg::SuperSampling(FilterType::Bilinear, 3)
        };
        let (ow, oh) = self.out;
        let (pw, ph) = (ow as f64, oh as f64);
        let (rwf, rhf) = (rw as f64, rh as f64);
        let mut out = vec![0u16; ow * oh];
        match fit {
            Scaling::Stretch | Scaling::Crop => {
                let (mut cx, mut cy, mut cw, mut ch) = (rx as f64, ry as f64, rwf, rhf);
                if fit == Scaling::Crop {
                    if rwf / rhf > pw / ph {
                        cw = rhf * pw / ph;
                        cx += (rwf - cw) / 2.0;
                    } else {
                        ch = rwf * ph / pw;
                        cy += (rhf - ch) / 2.0;
                    }
                }
                let mut dst = Image::new(ow as u32, oh as u32, PixelType::U8x4);
                let opts = ResizeOptions::new().resize_alg(alg).crop(cx, cy, cw, ch);
                self.resizer.resize(&img, &mut dst, &opts).ok()?;
                pack(dst.buffer(), order, &mut out, ow, (0, 0), ow);
            }
            Scaling::Fit => {
                let s = (pw / rwf).min(ph / rhf);
                let dw = ((rwf * s).round() as u32).clamp(1, ow as u32);
                let dh = ((rhf * s).round() as u32).clamp(1, oh as u32);
                let mut dst = Image::new(dw, dh, PixelType::U8x4);
                let opts = ResizeOptions::new().resize_alg(alg).crop(rx as f64, ry as f64, rwf, rhf);
                self.resizer.resize(&img, &mut dst, &opts).ok()?;
                let ox = (ow - dw as usize) / 2;
                let oy = (oh - dh as usize) / 2;
                pack(dst.buffer(), order, &mut out, ow, (ox, oy), dw as usize);
            }
        }
        Some(out)
    }

    pub fn rgba_image(&mut self, img: &image::RgbaImage, fit: Scaling) -> Frame565 {
        self.panel_frame(img.as_raw(), img.width(), img.height(), img.width() as usize * 4, Order::Rgba, None, fit)
            .unwrap_or_else(|| vec![0; self.out.0 * self.out.1])
    }
}

/// Copy 4-byte pixels (a `width`-wide block) into a frame `stride` pixels wide at `at`, as RGB565.
fn pack(src: &[u8], order: Order, out: &mut [u16], stride: usize, at: (usize, usize), width: usize) {
    for (i, px) in src.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (at.0 + i % width, at.1 + i / width);
        let (r, g, b) = match order {
            Order::Rgba => (px[0], px[1], px[2]),
            Order::Bgra => (px[2], px[1], px[0]),
        };
        out[y * stride + x] = crate::device::rgb565(r, g, b);
    }
}

pub fn rgb_to_565(img: &RgbImage) -> Frame565 {
    img.pixels().map(|p| crate::device::rgb565(p[0], p[1], p[2])).collect()
}

/// Convert a frame to big-endian RGB565 bytes (the layout stored in flash).
pub fn frame_bytes(frame: &[u16]) -> Vec<u8> {
    frame.iter().flat_map(|p| p.to_be_bytes()).collect()
}
