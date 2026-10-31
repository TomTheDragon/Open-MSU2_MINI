//! Control window. Uses wxWidgets, i.e. the platform's own controls and theme:
//! GTK on Linux, Win32 (incl. dark mode) on Windows, Cocoa on macOS.

use crate::config::{self, fps_class, fps_label, Config, Mode, TrayIcon, FPS_CHOICES};
use crate::controller;
use crate::flash;
use crate::gui_tray::{self, Action, Tray};
use crate::scale::Scaling;
use anyhow::Result;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use wxdragon::prelude::*;

const IMAGE_WILDCARD: &str =
    "Pictures (*.png;*.jpg;*.jpeg;*.bmp;*.gif;*.webp)|*.png;*.jpg;*.jpeg;*.bmp;*.gif;*.webp|All files|*";
const ROTATIONS: [u16; 4] = [0, 90, 180, 270];

fn file_name(p: &Option<PathBuf>) -> String {
    p.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "none".into())
}

#[derive(Default)]
struct FlashState {
    busy: bool,
    done: usize,
    total: usize,
    result: Option<std::result::Result<(), String>>,
}

pub fn run(port: Option<String>) -> Result<()> {
    // Take the device over from a running background instance and give it back afterwards.
    let resume_background = match config::background_pid() {
        Some(pid) => {
            config::kill_background(pid);
            true
        }
        None => false,
    };

    let mut cfg = Config::load();
    if port.is_some() {
        cfg.port = port;
    }

    let result = wxdragon::main(move |_| {
        // Follow the system light/dark setting (needed on Windows, automatic elsewhere).
        let _ = set_appearance(Appearance::System);
        build(cfg, resume_background);
    });
    result.map_err(|e| anyhow::anyhow!("{e}"))
}

fn build(cfg: Config, resume_background: bool) {
    let handle = Rc::new(RefCell::new(controller::spawn(cfg.clone(), true, |_| {})));

    let frame = Frame::builder().with_title("Open MSU2_MINI").build();
    if let Some(icon) =
        BitmapBundle::from_svg_text(gui_tray::ICON_SVG, Size::new(64, 64)).and_then(|b| b.get_bitmap(Size::new(64, 64)))
    {
        frame.set_icon(&icon);
    }
    let panel = Panel::builder(&frame).build();
    let root = BoxSizer::builder(Orientation::Vertical).build();
    let pad = 8;

    // -- status ---------------------------------------------------------------------
    let status_row = BoxSizer::builder(Orientation::Horizontal).build();
    let status_title = StaticText::builder(&panel).with_label("Device:").build();
    let status = StaticText::builder(&panel).with_label("starting...").build();
    let rate = StaticText::builder(&panel).with_label("").build();
    status_row.add(&status_title, 0, SizerFlag::AlignCenterVertical | SizerFlag::Right, 4);
    status_row.add(&status, 1, SizerFlag::AlignCenterVertical, 0);
    status_row.add(&rate, 0, SizerFlag::AlignCenterVertical | SizerFlag::Left, 8);
    root.add_sizer(&status_row, 0, SizerFlag::Expand | SizerFlag::All, pad);

    // -- modes ----------------------------------------------------------------------
    let mode_box = StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, &panel, "Show on the screen").build();
    let mode_parent = mode_box.get_static_box().unwrap();
    let mode_grid = GridSizer::builder(4, 2).with_vgap(4).with_hgap(16).build();
    let mut radios = Vec::new();
    for (i, m) in Mode::ALL.iter().enumerate() {
        let mut b = RadioButton::builder(&mode_parent).with_label(m.label());
        if i == 0 {
            b = b.first_in_group();
        }
        let rb = b.build();
        rb.set_value(*m == cfg.mode);
        let (h, m) = (handle.clone(), *m);
        rb.on_selected(move |_| h.borrow().update(|c| c.mode = m));
        mode_grid.add(&rb, 0, SizerFlag::AlignLeft, 0);
        radios.push((m, rb));
    }
    mode_box.add_sizer(&mode_grid, 0, SizerFlag::Expand | SizerFlag::All, 6);
    root.add_sizer(&mode_box, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right, pad);

    // -- options --------------------------------------------------------------------
    let opt_box = StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, &panel, "Options").build();
    let op = opt_box.get_static_box().unwrap();
    let grid = FlexGridSizer::builder(0, 3).with_vgap(6).with_hgap(8).build();
    grid.add_growable_col(1, 1);
    let label = |t: &str| StaticText::builder(&op).with_label(t).build();
    let cv = SizerFlag::AlignCenterVertical;

    let rotation = Choice::builder(&op).with_choices(ROTATIONS.iter().map(|r| format!("{r}°")).collect()).build();
    rotation.set_selection(ROTATIONS.iter().position(|r| *r == cfg.rotation).unwrap_or(0) as u32);
    rotation.set_tooltip(
        "Turns the picture clockwise. 90° and 270° are for a screen mounted upright.\n\
         Pictures stored on the device are turned when they are written.",
    );
    {
        let h = handle.clone();
        rotation.on_selection_changed(move |e| {
            if let Some(i) = e.get_selection() {
                h.borrow().update(|c| c.rotation = ROTATIONS[i as usize]);
            }
        });
    }
    let fit = Choice::builder(&op).with_choices(Scaling::ALL.iter().map(|f| f.name().to_string()).collect()).build();
    fit.set_selection(Scaling::ALL.iter().position(|f| *f == cfg.fit).unwrap_or(0) as u32);
    {
        let h = handle.clone();
        fit.on_selection_changed(move |e| {
            if let Some(i) = e.get_selection() {
                h.borrow().update(|c| c.fit = Scaling::ALL[i as usize]);
            }
        });
    }
    let fit_row = BoxSizer::builder(Orientation::Horizontal).build();
    fit_row.add(&label("Scaling:"), 0, cv | SizerFlag::Right, 6);
    fit_row.add(&fit, 0, cv, 0);
    grid.add(&label("Rotation:"), 0, cv, 0);
    grid.add(&rotation, 0, cv, 0);
    grid.add_sizer(&fit_row, 0, SizerFlag::AlignRight, 0);

    // frame-rate limit, each choice marked with how well the device can reach it
    let fps = Choice::builder(&op).with_choices(FPS_CHOICES.iter().map(|f| fps_label(*f)).collect()).build();
    let fps_index = FPS_CHOICES.iter().position(|f| *f >= cfg.max_fps - 0.001).unwrap_or(FPS_CHOICES.len() - 1);
    fps.set_selection(fps_index as u32);
    fps.set_tooltip(
        "✓ full screen: reached even when the whole picture changes\n\
         ◐ partial changes: reached when parts change; full-screen changes stay at ~25 fps\n\
         ⚠ small changes only: above the panel's ~61 fps limit, uses more CPU\n\
         eco: saves CPU and USB bandwidth",
    );
    let fps_note = StaticText::builder(&op).with_label(fps_class(FPS_CHOICES[fps_index]).explanation()).build();
    fps_note.set_foreground_color(Colour::new(128, 128, 128, 255));
    fps_note.wrap(300);
    {
        let (h, note, f) = (handle.clone(), fps_note, frame);
        fps.on_selection_changed(move |e| {
            if let Some(i) = e.get_selection() {
                let v = FPS_CHOICES[i as usize];
                note.set_label(fps_class(v).explanation());
                note.wrap(300);
                f.layout();
                h.borrow().update(|c| c.max_fps = v);
            }
        });
    }
    grid.add(&label("Mirror speed limit:"), 0, cv, 0);
    grid.add(&fps, 0, cv, 0);
    grid.add(&label(""), 0, cv, 0);
    grid.add(&label(""), 0, cv, 0);
    grid.add(&fps_note, 0, cv, 0);
    grid.add(&label(""), 0, cv, 0);

    // system monitor
    let stats_btn = Button::builder(&op).with_label("Customize...").build();
    {
        let (h, f) = (handle.clone(), frame);
        stats_btn.on_click(move |_| {
            let current = h.borrow().cfg.lock().unwrap().stats.clone();
            if let Some(new) = crate::gui_stats::customize(&f, &current) {
                h.borrow().update(|c| c.stats = new);
            }
        });
    }
    grid.add(&label("System monitor:"), 0, cv, 0);
    grid.add(&stats_btn, 0, cv, 0);
    grid.add(&label(""), 0, cv, 0);

    // mirror area
    let region = TextCtrl::builder(&op)
        .with_value(&cfg.region.map(|r| format!("{},{},{},{}", r[0], r[1], r[2], r[3])).unwrap_or_default())
        .build();
    region.set_tooltip("x,y,width,height in pixels of the shared screen. Empty = everything.");
    let apply = Button::builder(&op).with_label("Apply").build();
    {
        let (h, region, f) = (handle.clone(), region, frame);
        apply.on_click(move |_| {
            let t = region.get_value();
            let t = t.trim();
            if t.is_empty() {
                h.borrow().update(|c| c.region = None);
                return;
            }
            let v: Vec<u32> = t.replace('x', ",").split(',').filter_map(|p| p.trim().parse().ok()).collect();
            match <[u32; 4]>::try_from(v) {
                Ok(r) if r[2] > 0 && r[3] > 0 => h.borrow().update(|c| c.region = Some(r)),
                _ => {
                    MessageDialog::builder(
                        &f,
                        "Enter the area as x,y,width,height, or leave it empty for everything.",
                        "Open MSU2_MINI",
                    )
                    .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
                    .build()
                    .show_modal();
                }
            }
        });
    }
    grid.add(&label("Mirror area (x,y,w,h):"), 0, cv, 0);
    grid.add(&region, 1, SizerFlag::Expand | cv, 0);
    grid.add(&apply, 0, cv, 0);

    // which screen
    #[cfg(target_os = "linux")]
    {
        let again = Button::builder(&op).with_label("Choose screen / window again").build();
        let h = handle.clone();
        let radios2 = radios.clone();
        again.on_click(move |_| {
            h.borrow().reselect_screen();
            for (m, rb) in &radios2 {
                rb.set_value(*m == Mode::Mirror);
            }
        });
        grid.add(&label("Shared screen:"), 0, cv, 0);
        grid.add(&again, 0, cv, 0);
        grid.add(&label(""), 0, cv, 0);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let names = crate::capture::monitor_names();
        let mon = Choice::builder(&op).with_choices(names.clone()).build();
        if !names.is_empty() {
            mon.set_selection(cfg.monitor.min(names.len() - 1) as u32);
        }
        let h = handle.clone();
        mon.on_selection_changed(move |e| {
            if let Some(i) = e.get_selection() {
                h.borrow().update(|c| c.monitor = i as usize);
            }
        });
        grid.add(&label("Monitor:"), 0, cv, 0);
        grid.add(&mon, 0, cv, 0);
        grid.add(&label(""), 0, cv, 0);
    }

    // image + clock background
    let image_name = StaticText::builder(&op).with_label(&file_name(&cfg.image)).build();
    let image_btn = Button::builder(&op).with_label("Choose...").build();
    {
        let (h, f, name, radios2) = (handle.clone(), frame, image_name, radios.clone());
        image_btn.on_click(move |_| {
            if let Some(p) = pick_file(&f, "Choose a picture or GIF") {
                name.set_label(&file_name(&Some(p.clone())));
                h.borrow().update(|c| {
                    c.image = Some(p);
                    c.mode = Mode::Image;
                });
                for (m, rb) in &radios2 {
                    rb.set_value(*m == Mode::Image);
                }
            }
        });
    }
    grid.add(&label("Image / GIF:"), 0, cv, 0);
    grid.add(&image_name, 0, cv, 0);
    grid.add(&image_btn, 0, cv, 0);

    let video_name = StaticText::builder(&op).with_label(&file_name(&cfg.video)).build();
    let video_btn = Button::builder(&op).with_label("Choose...").build();
    video_btn.set_tooltip(
        "MP4/MOV with H.264 works directly; other formats\n\
         (WebM, MKV, AVI, HEVC, ...) need ffmpeg installed.",
    );
    {
        let (h, f, name, radios2) = (handle.clone(), frame, video_name, radios.clone());
        video_btn.on_click(move |_| {
            if let Some(p) = pick_video(&f) {
                name.set_label(&file_name(&Some(p.clone())));
                h.borrow().update(|c| {
                    c.video = Some(p);
                    c.mode = Mode::Video;
                });
                for (m, rb) in &radios2 {
                    rb.set_value(*m == Mode::Video);
                }
            }
        });
    }
    grid.add(&label("Video:"), 0, cv, 0);
    grid.add(&video_name, 0, cv, 0);
    grid.add(&video_btn, 0, cv, 0);

    let video_sound = CheckBox::builder(&op).with_label("Sound").build();
    video_sound.set_value(cfg.video_sound);
    video_sound.set_tooltip("Play the video's sound on the computer's default sound output.");
    {
        let (h, cb) = (handle.clone(), video_sound);
        video_sound.on_toggled(move |_| {
            let on = cb.get_value();
            h.borrow().update(|c| c.video_sound = on);
        });
    }
    let video_loop = CheckBox::builder(&op).with_label("Loop").build();
    video_loop.set_value(cfg.video_loop);
    video_loop.set_tooltip("Start the video again at the end.\nOff: it plays once and its last picture stays.");
    {
        let (h, cb) = (handle.clone(), video_loop);
        video_loop.on_toggled(move |_| {
            let on = cb.get_value();
            h.borrow().update(|c| c.video_loop = on);
        });
    }
    let video_opts = BoxSizer::builder(Orientation::Horizontal).build();
    video_opts.add(&video_sound, 0, cv, 0);
    video_opts.add(&video_loop, 0, cv | SizerFlag::Left, 16);
    grid.add(&label("Video playback:"), 0, cv, 0);
    grid.add_sizer(&video_opts, 0, cv, 0);
    grid.add(&label(""), 0, cv, 0);

    let bg_name = StaticText::builder(&op).with_label(&file_name(&cfg.clock_background)).build();
    let bg_btns = BoxSizer::builder(Orientation::Horizontal).build();
    let bg_btn = Button::builder(&op).with_label("Choose...").build();
    let bg_clear = Button::builder(&op).with_label("×").with_style(ButtonStyle::ExactFit).build();
    bg_clear.set_tooltip("No background picture");
    {
        let (h, f, name) = (handle.clone(), frame, bg_name);
        bg_btn.on_click(move |_| {
            if let Some(p) = pick_file(&f, "Choose a background picture for the clock") {
                name.set_label(&file_name(&Some(p.clone())));
                h.borrow().update(|c| c.clock_background = Some(p));
            }
        });
        let (h, name) = (handle.clone(), bg_name);
        bg_clear.on_click(move |_| {
            name.set_label("none");
            h.borrow().update(|c| c.clock_background = None);
        });
    }
    bg_btns.add(&bg_btn, 0, cv, 0);
    bg_btns.add(&bg_clear, 0, cv | SizerFlag::Left, 4);
    grid.add(&label("Clock background:"), 0, cv, 0);
    grid.add(&bg_name, 0, cv, 0);
    grid.add_sizer(&bg_btns, 0, cv, 0);

    // window: taskbar / notification area, start minimized
    let tray_choice =
        Choice::builder(&op).with_choices(TrayIcon::ALL.iter().map(|t| t.name().to_string()).collect()).build();
    tray_choice.set_selection(TrayIcon::ALL.iter().position(|t| *t == cfg.tray_icon).unwrap_or(0) as u32);
    tray_choice.set_tooltip(
        "Notification area: the window leaves the taskbar when minimized;\n\
         click the icon or use its menu to bring it back.",
    );
    let start_min = CheckBox::builder(&op).with_label("Start minimized").build();
    start_min.set_value(cfg.start_minimized);
    start_min.set_tooltip(
        "Open the window minimized. With the icon in the notification area only,\n\
         the window stays hidden until you click the icon.",
    );
    {
        let (h, cb) = (handle.clone(), start_min);
        start_min.on_toggled(move |_| {
            let on = cb.get_value();
            h.borrow().update(|c| c.start_minimized = on);
        });
    }
    grid.add(&label("Window icon in:"), 0, cv, 0);
    grid.add(&tray_choice, 0, cv, 0);
    grid.add(&start_min, 0, cv, 0);

    opt_box.add_sizer(&grid, 0, SizerFlag::Expand | SizerFlag::All, 6);
    root.add_sizer(&opt_box, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Top, pad);

    // -- flash ----------------------------------------------------------------------
    let fl_box =
        StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, &panel, "Store on the device (stays without PC)")
            .build();
    let fp = fl_box.get_static_box().unwrap();
    let fl_row = BoxSizer::builder(Orientation::Horizontal).build();
    let album_btn = Button::builder(&fp).with_label("Album image...").build();
    let anim_btn = Button::builder(&fp).with_label("Offline animation (36 frames)...").build();
    fl_row.add(&album_btn, 0, SizerFlag::Right, 8);
    fl_row.add(&anim_btn, 0, SizerFlag::Right, 0);
    let gauge = Gauge::builder(&fp).with_range(100).build();
    fl_box.add_sizer(&fl_row, 0, SizerFlag::All, 6);
    fl_box.add(&gauge, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 6);
    root.add_sizer(&fl_box, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Top, pad);

    let flash_state = Arc::new(Mutex::new(FlashState::default()));
    let start_flash = {
        let (h, st, f) = (handle.clone(), flash_state.clone(), frame);
        Rc::new(move |animation: bool| {
            if st.lock().unwrap().busy {
                return;
            }
            let src = if animation {
                let gif = MessageDialog::builder(
                    &f,
                    "Use an animated GIF?\n\n(No = choose a folder of pictures, e.g. A0.png ... A35.png)",
                    "Offline animation",
                )
                .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
                .build()
                .show_modal()
                    == ID_YES;
                if gif {
                    pick_file(&f, "Choose an animated GIF")
                } else {
                    let d = DirDialog::builder(&f, "Choose a folder of pictures", "").build();
                    if d.show_modal() == ID_OK {
                        d.get_path().map(PathBuf::from)
                    } else {
                        None
                    }
                }
            } else {
                pick_file(&f, "Choose the album picture")
            };
            let Some(src) = src else { return };
            let (fit, rotation) = {
                let c = h.borrow().cfg.lock().unwrap().clone();
                (c.fit, c.rotation)
            };
            *st.lock().unwrap() = FlashState { busy: true, ..Default::default() };
            let (st1, st2) = (st.clone(), st.clone());
            h.borrow().run_job(
                Box::new(move |dev| {
                    let progress = |d: usize, t: usize| {
                        let mut s = st1.lock().unwrap();
                        s.done = d;
                        s.total = t;
                    };
                    if animation {
                        flash::write_animation(dev, &src, fit, rotation, progress)
                    } else {
                        flash::write_album(dev, &src, fit, rotation, progress)
                    }
                }),
                Box::new(move |r| st2.lock().unwrap().result = Some(r.map_err(|e| format!("{e:#}")))),
            );
        })
    };
    {
        let s = start_flash.clone();
        album_btn.on_click(move |_| s(false));
        let s = start_flash.clone();
        anim_btn.on_click(move |_| s(true));
    }

    let tip = StaticText::builder(&panel).with_label("Tip: the button on the device cycles through the modes.").build();
    root.add(&tip, 0, SizerFlag::All, pad);

    panel.set_sizer(root, true);
    let outer = BoxSizer::builder(Orientation::Vertical).build();
    outer.add(&panel, 1, SizerFlag::Expand, 0);
    frame.set_sizer_and_fit(outer, true);

    // -- notification area icon -------------------------------------------------------
    let quit = Rc::new(Cell::new(false));
    let on_tray = {
        let (h, f, quit) = (handle.clone(), frame, quit.clone());
        move |a: Action| match a {
            Action::ToggleWindow => {
                if f.is_shown() && !f.is_iconized() {
                    if h.borrow().cfg.lock().unwrap().tray_icon == TrayIcon::Tray {
                        f.hide();
                    } else {
                        f.iconize(true);
                    }
                } else {
                    f.show(true);
                    f.iconize(false);
                    f.raise();
                }
            }
            Action::Mode(m) => h.borrow().update(|c| c.mode = m),
            // closed from the timer: the tray icon must not be removed inside its own event
            Action::Quit => quit.set(true),
        }
    };
    let tray: Rc<RefCell<Option<Tray>>> = Rc::new(RefCell::new(None));
    if cfg.tray_icon.in_tray() {
        *tray.borrow_mut() = Some(Tray::new(cfg.mode, on_tray.clone()));
    }
    {
        let (h, tray, f) = (handle.clone(), tray.clone(), frame);
        tray_choice.on_selection_changed(move |e| {
            let Some(i) = e.get_selection() else { return };
            let t = TrayIcon::ALL[i as usize];
            h.borrow().update(|c| c.tray_icon = t);
            let mut tr = tray.borrow_mut();
            if t.in_tray() && tr.is_none() {
                *tr = Some(Tray::new(h.borrow().cfg.lock().unwrap().mode, on_tray.clone()));
            } else if !t.in_tray() {
                if let Some(old) = tr.take() {
                    old.remove();
                }
                f.show(true);
            }
        });
    }

    // -- periodic UI refresh (status, button-driven mode changes, flash progress) -------
    let timer = Rc::new(Timer::new(&frame));
    {
        let (h, st, f) = (handle.clone(), flash_state.clone(), frame);
        let buttons = [album_btn, anim_btn];
        let last_text = RefCell::new(String::new());
        let last_mode = RefCell::new(cfg.mode);
        let (tray, quit) = (tray.clone(), quit.clone());
        timer.on_tick(move |_| {
            if quit.get() {
                f.close(true);
                return;
            }
            // notification area only: a minimized window leaves the taskbar
            let tray_only = h.borrow().cfg.lock().unwrap().tray_icon == TrayIcon::Tray;
            if tray_only && tray.borrow().is_some() && f.is_shown() && f.is_iconized() {
                f.hide();
            }
            let s = h.borrow().status.lock().unwrap().clone();
            if *last_text.borrow() != s.text {
                status.set_label(&s.text);
                *last_text.borrow_mut() = s.text.clone();
                f.layout();
            }
            let r = if s.connected
                && s.fps > 0.0
                && matches!(s.mode, Mode::Mirror | Mode::Animation | Mode::Image | Mode::Video)
            {
                format!("{:.0} fps · {:.0} KB/s", s.fps, s.kbps)
            } else {
                String::new()
            };
            if rate.get_label() != r {
                rate.set_label(&r);
                f.layout();
            }
            let mode = h.borrow().cfg.lock().unwrap().mode;
            if mode != *last_mode.borrow() {
                *last_mode.borrow_mut() = mode;
                for (m, rb) in &radios {
                    rb.set_value(*m == mode);
                }
                if let Some(t) = tray.borrow().as_ref() {
                    t.set_mode(mode);
                }
            }
            let mut fs = st.lock().unwrap();
            for b in &buttons {
                b.enable(!fs.busy);
            }
            if fs.busy {
                gauge.set_value((100 * fs.done).checked_div(fs.total).unwrap_or(0) as i32);
            }
            if let Some(res) = fs.result.take() {
                fs.busy = false;
                gauge.set_value(0);
                drop(fs);
                let (msg, style) = match res {
                    Ok(()) => ("Written to the device.".to_string(), MessageDialogStyle::IconInformation),
                    Err(e) => (format!("Writing failed: {e}"), MessageDialogStyle::IconError),
                };
                MessageDialog::builder(&f, &msg, "Open MSU2_MINI")
                    .with_style(MessageDialogStyle::OK | style)
                    .build()
                    .show_modal();
            }
        });
    }
    timer.start(250, false);

    {
        let (h, t, tray) = (handle.clone(), timer.clone(), tray.clone());
        frame.on_close(move |e| {
            t.stop();
            if let Some(tr) = tray.borrow_mut().take() {
                tr.remove();
            }
            h.borrow_mut().stop();
            if resume_background {
                restart_background();
            }
            e.skip(true);
        });
    }

    frame.centre();
    if !cfg.start_minimized {
        frame.show(true);
    } else if !(cfg.tray_icon == TrayIcon::Tray && tray.borrow().is_some()) {
        frame.show(true);
        frame.iconize(true);
    }
    // (notification area only + start minimized: stay hidden until the icon is clicked)
    // keep the timer alive for the lifetime of the window
    std::mem::forget(timer);
}

fn pick_file(parent: &Frame, title: &str) -> Option<PathBuf> {
    pick(parent, title, IMAGE_WILDCARD)
}

fn pick_video(parent: &Frame) -> Option<PathBuf> {
    let pattern: Vec<String> = crate::video::EXTENSIONS.iter().map(|e| format!("*.{e}")).collect();
    let pattern = pattern.join(";");
    pick(parent, "Choose a video", &format!("Videos ({pattern})|{pattern}|All files|*"))
}

fn pick(parent: &Frame, title: &str, wildcard: &str) -> Option<PathBuf> {
    let d = FileDialog::builder(parent)
        .with_message(title)
        .with_wildcard(wildcard)
        .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
        .build();
    if d.show_modal() == ID_OK {
        d.get_path().map(PathBuf::from)
    } else {
        None
    }
}

fn restart_background() {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let _ = cmd.spawn();
}
