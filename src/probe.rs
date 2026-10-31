//! `open-msu2-mini probe-backlight`: looks for a backlight command in the device's
//! firmware by trying unused display commands one by one, with the test number shown big
//! on a white screen so a change of the backlight is easy to see.
//!
//! Only the display command group (02) is tried; flash commands (03) are never sent.

use crate::device::{Device, Frame565, HEIGHT, WIDTH};
use crate::render::{blank, bold, text, Anchor, Style};
use crate::scale::rgb_to_565;
use anyhow::Result;
use image::Rgb;
use std::time::{Duration, Instant};

/// Candidate commands; the value bytes are filled in (00 = off, FF = full).
fn candidates() -> Vec<[u8; 3]> {
    let mut c: Vec<[u8; 3]> = (0x0C..=0x1F).map(|s| [2, 3, s]).collect();
    c.extend((0x05..=0x0F).map(|s| [2, s, 0]));
    c
}

fn screen(big: &str, small: &str, bg: [u8; 3]) -> Frame565 {
    let mut img = blank((WIDTH, HEIGHT));
    for p in img.pixels_mut() {
        *p = Rgb(bg);
    }
    text(&mut img, big, 80, 32, Style::new(26.0, bold(), [0, 0, 0]).anchor(Anchor::Center));
    text(&mut img, small, 80, 66, Style::new(12.0, bold(), [0, 0, 0]).anchor(Anchor::Center));
    rgb_to_565(&img)
}

/// Keep the connection (the device goes offline after ~5 s of silence).
fn hold(d: &mut Device, dur: Duration) -> Result<()> {
    let end = Instant::now() + dur;
    while Instant::now() < end {
        d.read_adc(9)?;
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

pub fn run(port: Option<&str>, from: usize, to: Option<usize>) -> Result<()> {
    let list = candidates();
    let mut d = Device::open(port)?;
    for s in (1..=5).rev() {
        d.draw_diff(&screen(&format!("START IN {s}"), "watch the brightness", [255, 255, 255]), None)?;
        println!("starting in {s}");
        hold(&mut d, Duration::from_secs(1))?;
    }
    let to = to.unwrap_or(list.len()).min(list.len());
    for (i, c) in list.iter().enumerate().take(to).skip(from.saturating_sub(1)) {
        let n = i + 1;
        let bytes = format!("{:02X} {:02X} {:02X}", c[0], c[1], c[2]);
        let label = format!("TEST {n}/{}", list.len());
        println!("{label}: {bytes} 00 00 00, then FF");
        d.draw_diff(&screen(&label, &bytes, [255, 255, 255]), None)?;
        hold(&mut d, Duration::from_millis(1500))?;
        d.raw_command([c[0], c[1], c[2], 0, 0, 0])?;
        hold(&mut d, Duration::from_secs(3))?;
        d.raw_command([c[0], c[1], c[2], 0xFF, 0xFF, 0xFF])?;
        // the command may have changed what is drawn: redraw
        d.draw_diff(&screen(&label, &format!("{bytes} done"), [255, 255, 255]), None)?;
        hold(&mut d, Duration::from_millis(1000))?;
    }
    d.draw_diff(&screen("DONE", "backlight probe finished", [160, 255, 160]), None)?;
    hold(&mut d, Duration::from_secs(3))?;
    println!("done");
    Ok(())
}
