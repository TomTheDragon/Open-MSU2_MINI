//! Wayland/X11 capture through the ScreenCast portal and PipeWire.

use super::{token_file, CaptureOptions, Shared, Source};
use crate::scale::{Order, Scaler};
use anyhow::{anyhow, Context, Result};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::PersistMode;
use pipewire as pw;
use pw::spa;
use pw::spa::param::video::VideoFormat;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Portal {
    proxy: Screencast,
    session: ashpd::desktop::Session<Screencast>,
}

/// Ask the portal for a stream (shows the share dialog unless a restore token is valid).
async fn open_portal(opts: &CaptureOptions) -> Result<(Portal, OwnedFd, u32, Option<(i32, i32)>)> {
    let proxy = Screencast::new().await.context("screen cast portal not available")?;
    let session = proxy.create_session(Default::default()).await?;
    let sources = match opts.source {
        Source::Monitor => SourceType::Monitor.into(),
        Source::Window => SourceType::Window.into(),
        Source::Any => SourceType::Monitor | SourceType::Window,
    };
    let cursor = if proxy.available_cursor_modes().await.map(|m| m.contains(CursorMode::Embedded)).unwrap_or(false) {
        CursorMode::Embedded
    } else {
        CursorMode::Hidden
    };
    let token = if opts.reselect { None } else { std::fs::read_to_string(token_file()).ok() };
    let mut sel = SelectSourcesOptions::default().set_cursor_mode(cursor).set_sources(sources).set_multiple(false);
    if proxy.version() >= 4 {
        sel = sel.set_persist_mode(PersistMode::ExplicitlyRevoked).set_restore_token(token.as_deref().map(str::trim));
    }
    proxy.select_sources(&session, sel).await?.response().context("screen selection was cancelled")?;
    let streams = proxy
        .start(&session, None, Default::default())
        .await?
        .response()
        .map_err(|e| anyhow!("screen sharing was cancelled or refused ({e})"))?;
    if let Some(t) = streams.restore_token() {
        let _ = std::fs::create_dir_all(crate::config::config_dir());
        let _ = std::fs::write(token_file(), t);
    }
    let stream = streams.streams().first().ok_or_else(|| anyhow!("portal returned no stream"))?;
    let node = stream.pipe_wire_node_id();
    let size = stream.size();
    let fd = proxy.open_pipe_wire_remote(&session, Default::default()).await?;
    Ok((Portal { proxy, session }, fd, node, size))
}

pub fn start(opts: CaptureOptions, shared: Arc<Shared>) -> Box<dyn FnMut() + Send> {
    let sender: Arc<Mutex<Option<pw::channel::Sender<()>>>> = Arc::default();
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (s2, st2) = (sender.clone(), stopped.clone());
    std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            if let Err(e) = run(opts, &shared, &s2, &st2) {
                shared.fail(format!("{e:#}"));
            }
        })
        .unwrap();
    Box::new(move || {
        stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(s) = sender.lock().unwrap().take() {
            let _ = s.send(());
        }
    })
}

struct StreamState {
    format: spa::param::video::VideoInfoRaw,
    scaler: Scaler,
    opts: CaptureOptions,
    last: Instant,
    min_interval: Duration,
    last_taken: u64,
    shared: Arc<Shared>,
}

fn run(
    opts: CaptureOptions,
    shared: &Arc<Shared>,
    stop_sender: &Mutex<Option<pw::channel::Sender<()>>>,
    stopped: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    let (portal, fd, node, _size) = pollster::block_on(open_portal(&opts))?;
    if stopped.load(std::sync::atomic::Ordering::SeqCst) {
        let _ = pollster::block_on(portal.session.close());
        return Ok(());
    }

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None).context("cannot connect to PipeWire")?;

    let (tx, rx) = pw::channel::channel::<()>();
    *stop_sender.lock().unwrap() = Some(tx);
    let weak = mainloop.downgrade();
    let _rx = rx.attach(mainloop.loop_(), move |_| {
        if let Some(m) = weak.upgrade() {
            m.quit();
        }
    });

    let stream = pw::stream::StreamBox::new(
        &core,
        "open-msu2-mini",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let state = StreamState {
        format: Default::default(),
        scaler: Scaler::default().sized(opts.size),
        min_interval: Duration::from_secs_f64(1.0 / opts.max_fps.max(0.1) as f64),
        opts,
        last: Instant::now() - Duration::from_secs(1),
        last_taken: u64::MAX,
        shared: shared.clone(),
    };
    let weak2 = mainloop.downgrade();
    let shared2 = shared.clone();
    let _listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed(move |_, _, _, new| {
            if let pw::stream::StreamState::Error(e) = new {
                shared2.fail(format!("PipeWire stream error: {e}"));
                if let Some(m) = weak2.upgrade() {
                    m.quit();
                }
            }
        })
        .param_changed(|_, st, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let _ = st.format.parse(param);
        })
        .process(|stream, st| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            // Skip work if the consumer has not picked up the previous frame yet, or
            // if frames come faster than needed.
            let taken = st.shared.taken.load(std::sync::atomic::Ordering::Acquire);
            if st.last.elapsed() < st.min_interval
                || (taken == st.last_taken && st.last.elapsed() < st.min_interval * 4)
            {
                return;
            }
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let chunk = d.chunk();
            let (offset, size, stride) = (chunk.offset() as usize, chunk.size() as usize, chunk.stride());
            let w = st.format.size().width;
            let h = st.format.size().height;
            let order = match st.format.format() {
                VideoFormat::BGRx | VideoFormat::BGRA => Order::Bgra,
                VideoFormat::RGBx | VideoFormat::RGBA => Order::Rgba,
                _ => return,
            };
            let Some(data) = d.data() else { return };
            if size == 0 || data.len() < offset + size {
                return; // empty buffer (e.g. cursor-only update)
            }
            let stride = if stride > 0 { stride as usize } else { w as usize * 4 };
            if let Some(f) =
                st.scaler.panel_frame(&data[offset..offset + size], w, h, stride, order, st.opts.region, st.opts.fit)
            {
                st.shared.publish(f);
                st.last = Instant::now();
                st.last_taken = taken;
            }
        })
        .register()?;

    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(spa::param::format::FormatProperties::MediaType, Id, spa::param::format::MediaType::Video),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 16384, height: 16384 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 60, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 1000, denom: 1 }
        ),
    );
    let values: Vec<u8> =
        spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
            .map_err(|e| anyhow!("pod: {e:?}"))?
            .0
            .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values).ok_or_else(|| anyhow!("bad pod"))?];
    stream.connect(
        spa::utils::Direction::Input,
        Some(node),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    if !stopped.load(std::sync::atomic::Ordering::SeqCst) {
        mainloop.run();
    }
    let _ = stream.disconnect();
    let _ = pollster::block_on(portal.session.close());
    drop(portal.proxy);
    Ok(())
}
