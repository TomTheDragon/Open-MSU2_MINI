//! Icon in the notification area (system tray) with a small menu.

use crate::config::Mode;
use wxdragon::prelude::*;

pub const ICON_SVG: &str = include_str!("../assets/icon.svg");

const ID_SHOW: Id = 7000;
const ID_QUIT: Id = 7001;
const ID_MODE0: Id = 7010;

/// What the user picked in the tray menu.
pub enum Action {
    ToggleWindow,
    Mode(Mode),
    Quit,
}

pub struct Tray {
    icon: TaskBarIcon,
    /// Template the tray copies each time the menu opens; must stay alive.
    menu: Menu,
    bitmap: Option<BitmapBundle>,
}

impl Tray {
    pub fn new(mode: Mode, on_action: impl Fn(Action) + Clone + 'static) -> Tray {
        let icon = TaskBarIcon::builder().build();
        let mut builder = Menu::builder().append_item(ID_SHOW, "Show / hide window", "").append_separator();
        for (i, m) in Mode::ALL.iter().enumerate() {
            builder = builder.append_radio_item(ID_MODE0 + i as Id, m.label(), "");
        }
        let mut menu = builder.append_separator().append_item(ID_QUIT, "Quit", "").build();
        // The menu must be set before the icon: with AppIndicator (Linux) wxWidgets
        // builds the tray's menu when the icon is set, and crashes if there is none.
        icon.set_popup_menu(&mut menu);
        let bitmap = BitmapBundle::from_svg_text(ICON_SVG, Size::new(32, 32));
        let tray = Tray { icon, menu, bitmap };
        tray.set_mode(mode);

        let act = on_action.clone();
        tray.icon.on_menu(move |e| {
            let id = e.get_id();
            match id {
                ID_SHOW => act(Action::ToggleWindow),
                ID_QUIT => act(Action::Quit),
                _ => {
                    if let Some(m) = id.checked_sub(ID_MODE0).and_then(|i| Mode::ALL.get(i as usize)) {
                        act(Action::Mode(*m));
                    }
                }
            }
        });
        // a click on the icon shows or hides the window (the menu opens on right-click)
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        tray.icon.on_left_down(move |_| on_action(Action::ToggleWindow));
        tray
    }

    /// Tick the current mode in the menu.
    pub fn set_mode(&self, mode: Mode) {
        if let Some(i) = Mode::ALL.iter().position(|m| *m == mode) {
            self.menu.check_item(ID_MODE0 + i as Id, true);
        }
        // (Re)setting the icon also hands AppIndicator a fresh copy of the menu.
        if let Some(b) = &self.bitmap {
            self.icon.set_icon_bundle(b, "Open MSU2_MINI");
        }
    }

    /// Remove the icon. Needed before the program can exit.
    pub fn remove(self) {
        self.icon.remove_icon();
        self.icon.destroy();
        drop(self.menu);
    }
}
