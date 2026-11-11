//! YouTube part of the control window: address, play / stop, sound, favourites.

use crate::config::{Config, Favorite, Mode};
use crate::controller::{Handle, Media, Status};
use crate::gui::{play_label, Load, Refresh};
use std::cell::RefCell;
use std::rc::Rc;
use wxdragon::prelude::*;

const TITLE: &str = "Open MSU2_MINI";

/// Builds the "YouTube" box.
pub fn build(
    panel: &Panel,
    frame: Frame,
    handle: &Rc<RefCell<Handle>>,
    cfg: &Config,
) -> (StaticBoxSizer, Refresh, Load) {
    let yt_box = StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, panel, "YouTube").build();
    let p = yt_box.get_static_box().unwrap();
    let cv = SizerFlag::AlignCenterVertical;
    let grid = FlexGridSizer::builder(0, 2).with_vgap(6).with_hgap(8).build();
    grid.add_growable_col(1, 1);

    // address + play / stop / sound
    let url = TextCtrl::builder(&p)
        .with_value(cfg.youtube_url.as_deref().unwrap_or_default())
        .with_style(TextCtrlStyle::ProcessEnter)
        .build();
    url.set_tooltip(
        "Address of a YouTube live stream or video, e.g. https://www.youtube.com/@channel/live\n\
         (other sites yt-dlp supports work too). Enter or Play starts it.",
    );
    let play = Button::builder(&p).with_label(play_label(false)).build();
    play.set_tooltip("Play the address, or pause and go on (like a tap on the device's touch button).");
    let stop = Button::builder(&p).with_label("■ Stop").build();
    let sound = CheckBox::builder(&p).with_label("Sound").build();
    sound.set_value(cfg.youtube_sound);
    sound.set_tooltip("Play the sound on the computer's default sound output.");
    let start = {
        let h = handle.clone();
        Rc::new(move || {
            let u = url.get_value().trim().to_string();
            if u.is_empty() {
                warn(&frame, "Enter the address of a YouTube live stream or video.");
                return;
            }
            h.borrow().play_youtube(u);
        })
    };
    {
        // the address playing: pause / go on; another address (or stopped): play it
        let (h, s) = (handle.clone(), start.clone());
        play.on_click(move |_| {
            let u = url.get_value().trim().to_string();
            let c = h.borrow().cfg.lock().unwrap().clone();
            if c.mode == Mode::YouTube && c.youtube_playing && c.youtube_url.as_deref() == Some(u.as_str()) {
                h.borrow().play_pause();
            } else {
                s();
            }
        });
        let s = start.clone();
        url.on_text_enter(move |_| s());
        let h = handle.clone();
        stop.on_click(move |_| h.borrow().stop_media());
        let h = handle.clone();
        sound.on_toggled(move |_| h.borrow().set_youtube_sound(sound.get_value()));
    }
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    row.add(&url, 1, cv, 0);
    row.add(&play, 0, cv | SizerFlag::Left, 6);
    row.add(&stop, 0, cv | SizerFlag::Left, 4);
    row.add(&sound, 0, cv | SizerFlag::Left, 10);
    grid.add(&StaticText::builder(&p).with_label("Address:").build(), 0, cv, 0);
    grid.add_sizer(&row, 1, SizerFlag::Expand | cv, 0);

    // favourites
    let favs = Choice::builder(&p).build();
    favs.set_tooltip("Choose a favourite to play it.");
    let add = Button::builder(&p).with_label("Add...").build();
    add.set_tooltip("Save the address above as a favourite.");
    let remove = Button::builder(&p).with_label("Remove").build();
    remove.set_tooltip("Remove the chosen favourite.");
    let fill = {
        let h = handle.clone();
        Rc::new(move |select: Option<&str>| {
            let list = h.borrow().cfg.lock().unwrap().youtube_favorites.clone();
            favs.clear();
            if list.is_empty() {
                favs.append("(no favourites yet)");
                favs.set_selection(0);
            }
            for f in &list {
                favs.append(&f.name);
            }
            if let Some(i) = select.and_then(|u| list.iter().position(|f| f.url == u)) {
                favs.set_selection(i as u32);
            }
            favs.enable(!list.is_empty());
            remove.enable(!list.is_empty());
        })
    };
    fill(cfg.youtube_url.as_deref());
    {
        let (h, s) = (handle.clone(), start.clone());
        favs.on_selection_changed(move |e| {
            let Some(i) = e.get_selection() else { return };
            let fav = h.borrow().cfg.lock().unwrap().youtube_favorites.get(i as usize).cloned();
            if let Some(f) = fav {
                url.set_value(&f.url);
                s();
            }
        });
    }
    {
        let (h, fill) = (handle.clone(), fill.clone());
        add.on_click(move |_| {
            let u = url.get_value().trim().to_string();
            if u.is_empty() {
                warn(&frame, "Enter the address of a YouTube live stream or video first.");
                return;
            }
            // suggest the title of the stream when it is the one playing
            let (playing, existing) = {
                let c = h.borrow().cfg.lock().unwrap().clone();
                (c.youtube_url.as_deref() == Some(u.as_str()), c.youtube_favorites.iter().find(|f| f.url == u).cloned())
            };
            let title = if playing { h.borrow().status.lock().unwrap().title.clone() } else { None };
            let suggestion = existing.map(|f| f.name).or(title).unwrap_or_else(|| u.clone());
            let d = TextEntryDialog::builder(&frame, "Name of the favourite:", TITLE)
                .with_default_value(&suggestion)
                .build();
            if d.show_modal() != ID_OK {
                return;
            }
            let name = d.get_value().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or(u.clone());
            h.borrow().update(|c| match c.youtube_favorites.iter_mut().find(|f| f.url == u) {
                Some(f) => f.name = name,
                None => c.youtube_favorites.push(Favorite { name, url: u.clone() }),
            });
            fill(Some(&u));
        });
    }
    {
        let (h, fill) = (handle.clone(), fill.clone());
        remove.on_click(move |_| {
            let Some(i) = favs.get_selection() else {
                warn(&frame, "Choose the favourite to remove first.");
                return;
            };
            h.borrow().update(|c| {
                if (i as usize) < c.youtube_favorites.len() {
                    c.youtube_favorites.remove(i as usize);
                }
            });
            fill(None);
        });
    }
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    row.add(&favs, 1, cv, 0);
    row.add(&add, 0, cv | SizerFlag::Left, 6);
    row.add(&remove, 0, cv | SizerFlag::Left, 4);
    grid.add(&StaticText::builder(&p).with_label("Favourites:").build(), 0, cv, 0);
    grid.add_sizer(&row, 1, SizerFlag::Expand | cv, 0);

    yt_box.add_sizer(&grid, 0, SizerFlag::Expand | SizerFlag::All, 6);

    // the touch button can switch the address; the button shows Play or Pause
    let last_url = Rc::new(RefCell::new(cfg.youtube_url.clone()));
    // another display chosen: its address, sound and favourites
    let load: Load = {
        let (last_url, fill) = (last_url.clone(), fill.clone());
        Box::new(move |c: &Config| {
            *last_url.borrow_mut() = c.youtube_url.clone();
            url.set_value(c.youtube_url.as_deref().unwrap_or_default());
            sound.set_value(c.youtube_sound);
            fill(c.youtube_url.as_deref());
        })
    };
    let refresh: Refresh = Rc::new(move |s: &Status, c: &Config| {
        if *last_url.borrow() != c.youtube_url {
            *last_url.borrow_mut() = c.youtube_url.clone();
            url.set_value(c.youtube_url.as_deref().unwrap_or_default());
            fill(c.youtube_url.as_deref());
        }
        let l = play_label(s.mode == Mode::YouTube && s.media == Media::Playing);
        if play.get_label() != l {
            play.set_label(l);
        }
    });
    (yt_box, refresh, load)
}

fn warn(frame: &Frame, msg: &str) {
    MessageDialog::builder(frame, msg, TITLE)
        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
        .build()
        .show_modal();
}
