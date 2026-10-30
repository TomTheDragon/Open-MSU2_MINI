//! Store pictures in the device's flash (they stay there without a PC).

use crate::device::{canvas_size, orient, Device, Frame565, FLASH_ALBUM, FLASH_ANIMATION, FLASH_CLOCK_BG};
use crate::render::load_frames;
use crate::scale::{frame_bytes, Scaling};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub const ANIM_FRAMES: usize = 36;

/// Picture in panel format. The device shows flash pictures itself, so 90/270-degree
/// rotation is applied here; the 180-degree part is the panel's own flip.
fn single(path: &Path, fit: Scaling, rotation: u16) -> Result<Frame565> {
    Ok(orient(&load_frames(path, fit, canvas_size(rotation))?.remove(0).0, rotation))
}

/// 36 frames from a folder (A0..A35 like the vendor tool, or any sorted pictures)
/// or an animated GIF/WebP/PNG (frames sampled evenly).
pub fn animation_frames(source: &Path, fit: Scaling, rotation: u16) -> Result<Vec<Frame565>> {
    let frames: Vec<Frame565> = if source.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(source)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref(),
                    Some("png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp")
                )
            })
            .collect();
        let named: Vec<Option<&PathBuf>> = (0..ANIM_FRAMES)
            .map(|i| {
                files.iter().find(|f| {
                    f.file_stem().map(|s| s.to_string_lossy().eq_ignore_ascii_case(&format!("A{i}"))).unwrap_or(false)
                })
            })
            .collect();
        if named.iter().all(|n| n.is_some()) {
            files = named.into_iter().map(|n| n.unwrap().clone()).collect();
        } else {
            files.sort_by_key(|f| natural_key(&f.file_name().unwrap_or_default().to_string_lossy()));
        }
        files.iter().map(|f| single(f, fit, rotation)).collect::<Result<_>>()?
    } else {
        load_frames(source, fit, canvas_size(rotation))?.into_iter().map(|(f, _)| orient(&f, rotation)).collect()
    };
    if frames.is_empty() {
        bail!("no pictures found");
    }
    Ok((0..ANIM_FRAMES).map(|i| frames[i * frames.len() / ANIM_FRAMES].clone()).collect())
}

fn natural_key(s: &str) -> Vec<(u8, u64, String)> {
    let mut out = Vec::new();
    let mut num = String::new();
    let mut txt = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            if !txt.is_empty() {
                out.push((1, 0, std::mem::take(&mut txt)));
            }
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push((0, num.parse().unwrap_or(0), String::new()));
                num.clear();
            }
            txt.push(c);
        }
    }
    if !num.is_empty() {
        out.push((0, num.parse().unwrap_or(0), String::new()));
    }
    if !txt.is_empty() {
        out.push((1, 0, txt));
    }
    out
}

pub fn write_album(
    dev: &mut Device,
    path: &Path,
    fit: Scaling,
    rotation: u16,
    progress: impl FnMut(usize, usize),
) -> Result<()> {
    dev.write_flash(FLASH_ALBUM, &frame_bytes(&single(path, fit, rotation)?), progress)
}

pub fn write_clock_background(
    dev: &mut Device,
    path: &Path,
    fit: Scaling,
    rotation: u16,
    progress: impl FnMut(usize, usize),
) -> Result<()> {
    dev.write_flash(FLASH_CLOCK_BG, &frame_bytes(&single(path, fit, rotation)?), progress)
}

pub fn write_animation(
    dev: &mut Device,
    source: &Path,
    fit: Scaling,
    rotation: u16,
    progress: impl FnMut(usize, usize),
) -> Result<()> {
    let data: Vec<u8> = animation_frames(source, fit, rotation)?.iter().flat_map(|f| frame_bytes(f)).collect();
    dev.write_flash(FLASH_ANIMATION, &data, progress)
}
