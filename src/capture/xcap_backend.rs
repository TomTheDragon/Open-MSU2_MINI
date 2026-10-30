//! Windows / macOS capture with xcap. Uses the streaming recorder (DXGI desktop
//! duplication on Windows, AVFoundation on macOS) and falls back to screenshots.

use super::{CaptureOptions, Shared};
use crate::scale::{Order, Scaler};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn start(opts: CaptureOptions, shared: Arc<Shared>) -> Box<dyn FnMut() + Send> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            if let Err(e) = run(&opts, &shared, &stop2) {
                shared.fail(format!("{e:#}"));
            }
        })
        .unwrap();
    Box::new(move || stop.store(true, Ordering::SeqCst))
}

fn run(opts: &CaptureOptions, shared: &Shared, stop: &AtomicBool) -> anyhow::Result<()> {
    let monitors = xcap::Monitor::all()?;
    if monitors.is_empty() {
        anyhow::bail!("no monitor found");
    }
    let mut sorted = monitors;
    sorted.sort_by_key(|m| !m.is_primary().unwrap_or(false)); // primary first
    let monitor = sorted.get(opts.monitor).unwrap_or(&sorted[0]).clone();
    let mut scaler = Scaler::default().sized(opts.size);
    let min_interval = Duration::from_secs_f64(1.0 / opts.max_fps.max(0.1) as f64);

    // Streaming recorder: delivers frames only when the screen changes.
    if let Ok((recorder, rx)) = monitor.video_recorder() {
        if recorder.start().is_ok() {
            let mut last = Instant::now() - Duration::from_secs(1);
            while !stop.load(Ordering::SeqCst) {
                let Ok(mut frame) = rx.recv_timeout(Duration::from_millis(200)) else { continue };
                while let Ok(newer) = rx.try_recv() {
                    frame = newer;
                }
                if last.elapsed() < min_interval {
                    continue;
                }
                last = Instant::now();
                let (w, h) = (frame.width, frame.height);
                if let Some(f) =
                    scaler.panel_frame(&frame.raw, w, h, w as usize * 4, Order::Rgba, opts.region, opts.fit)
                {
                    shared.publish(f);
                }
            }
            let _ = recorder.stop();
            return Ok(());
        }
    }

    // Fallback: screenshots in a loop.
    while !stop.load(Ordering::SeqCst) {
        let t = Instant::now();
        let img = monitor.capture_image()?;
        if let Some(f) = scaler.panel_frame(
            img.as_raw(),
            img.width(),
            img.height(),
            img.width() as usize * 4,
            Order::Rgba,
            opts.region,
            opts.fit,
        ) {
            shared.publish(f);
        }
        if let Some(rest) = min_interval.checked_sub(t.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    Ok(())
}
