//! Settings dialog for the system monitor.

use crate::config::{StatItem, StatStyle, StatsConfig};
use std::cell::RefCell;
use std::rc::Rc;
use wxdragon::prelude::*;

const ROW_CHOICES: [u32; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];
const INTERVALS_MS: [u32; 6] = [250, 500, 1000, 2000, 5000, 10000];

/// Show the dialog; returns the new settings, or None when cancelled.
pub fn customize(parent: &dyn WxWidget, current: &StatsConfig) -> Option<StatsConfig> {
    let dlg = Dialog::builder(parent, "System monitor").build();
    let root = BoxSizer::builder(Orientation::Vertical).build();
    let pad = 8;

    // -- items: tick to show, arrows to reorder ----------------------------------------
    let items_box = StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, &dlg, "Shown values").build();
    let ib = items_box.get_static_box().unwrap();
    let hint =
        StaticText::builder(&ib).with_label("Tick what to show. The order on the screen is the order here.").build();
    items_box.add(&hint, 0, SizerFlag::All, 4);

    // all items: chosen ones first (in their order), then the rest
    let mut order: Vec<(StatItem, bool)> = current.items.iter().map(|i| (*i, true)).collect();
    for i in StatItem::ALL {
        if !current.items.contains(&i) {
            order.push((i, false));
        }
    }
    let order = Rc::new(RefCell::new(order));

    let list_row = BoxSizer::builder(Orientation::Horizontal).build();
    let list = CheckListBox::builder(&ib).with_size(Size::new(240, 300)).build();
    let refill = {
        let order = order.clone();
        move |list: &CheckListBox, select: Option<u32>| {
            list.clear();
            for (i, (item, on)) in order.borrow().iter().enumerate() {
                list.append(item.name());
                list.check(i as u32, *on);
            }
            if let Some(s) = select {
                list.set_selection(s, true);
            }
        }
    };
    refill(&list, None);
    {
        let order = order.clone();
        list.on_toggled(move |e| {
            if let (Some(i), Some(on)) = (e.get_selection(), e.is_checked()) {
                if let Some(entry) = order.borrow_mut().get_mut(i as usize) {
                    entry.1 = on;
                }
            }
        });
    }
    let arrows = BoxSizer::builder(Orientation::Vertical).build();
    let up = Button::builder(&ib).with_label("▲ Up").build();
    let down = Button::builder(&ib).with_label("▼ Down").build();
    for (btn, delta) in [(up, -1i32), (down, 1i32)] {
        let (order, refill) = (order.clone(), refill.clone());
        btn.on_click(move |_| {
            // keep the tick marks in sync before moving
            let Some(sel) = list.get_selection() else { return };
            let n = order.borrow().len() as i32;
            let target = sel as i32 + delta;
            if target < 0 || target >= n {
                return;
            }
            for i in 0..n as u32 {
                order.borrow_mut()[i as usize].1 = list.is_checked(i);
            }
            order.borrow_mut().swap(sel as usize, target as usize);
            refill(&list, Some(target as u32));
        });
        arrows.add(&btn, 0, SizerFlag::Expand | SizerFlag::Bottom, 4);
    }
    list_row.add(&list, 1, SizerFlag::Expand, 0);
    list_row.add_sizer(&arrows, 0, SizerFlag::Left, 6);
    items_box.add_sizer(&list_row, 1, SizerFlag::Expand | SizerFlag::All, 4);
    root.add_sizer(&items_box, 1, SizerFlag::Expand | SizerFlag::All, pad);

    // -- look ---------------------------------------------------------------------------
    let look = StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, &dlg, "Look").build();
    let lb = look.get_static_box().unwrap();
    let grid = FlexGridSizer::builder(0, 2).with_vgap(6).with_hgap(8).build();
    let cv = SizerFlag::AlignCenterVertical;
    let label = |t: &str| StaticText::builder(&lb).with_label(t).build();

    let style =
        Choice::builder(&lb).with_choices(StatStyle::ALL.iter().map(|s| s.name().to_string()).collect()).build();
    style.set_selection(StatStyle::ALL.iter().position(|s| *s == current.style).unwrap_or(0) as u32);
    grid.add(&label("Display:"), 0, cv, 0);
    grid.add(&style, 0, cv, 0);

    let rows = Choice::builder(&lb)
        .with_choices(
            ROW_CHOICES.iter().map(|r| if *r == 0 { "automatic".to_string() } else { r.to_string() }).collect(),
        )
        .build();
    rows.set_selection(ROW_CHOICES.iter().position(|r| *r == current.rows).unwrap_or(0) as u32);
    grid.add(&label("Rows per page:"), 0, cv, 0);
    grid.add(&rows, 0, cv, 0);

    let page = SpinCtrl::builder(&lb)
        .with_min_value(1)
        .with_max_value(120)
        .with_initial_value(current.page_seconds as i32)
        .build();
    page.set_tooltip("When there are more values than rows, pages are shown one after another.");
    grid.add(&label("Seconds per page:"), 0, cv, 0);
    grid.add(&page, 0, cv, 0);

    let interval = Choice::builder(&lb)
        .with_choices(INTERVALS_MS.iter().map(|ms| format!("{} s", *ms as f32 / 1000.0)).collect())
        .build();
    interval.set_selection(INTERVALS_MS.iter().position(|i| *i >= current.interval_ms).unwrap_or(2) as u32);
    grid.add(&label("Update every:"), 0, cv, 0);
    grid.add(&interval, 0, cv, 0);

    let [r, g, b] = current.accent;
    let accent = ColourPickerCtrl::builder(&lb).with_initial_colour(Colour::new(r, g, b, 255)).build();
    grid.add(&label("Label colour:"), 0, cv, 0);
    grid.add(&accent, 0, cv, 0);
    look.add_sizer(&grid, 0, SizerFlag::All, 6);

    let load_colors = CheckBox::builder(&lb).with_label("Colour bars by load (green / yellow / red)").build();
    load_colors.set_value(current.load_colors);
    let temp_with = CheckBox::builder(&lb).with_label("Show temperature next to CPU and GPU usage").build();
    temp_with.set_value(current.temp_with_usage);
    let fahrenheit = CheckBox::builder(&lb).with_label("Temperatures in °F").build();
    fahrenheit.set_value(current.fahrenheit);
    for c in [&load_colors, &temp_with, &fahrenheit] {
        look.add(c, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 6);
    }
    root.add_sizer(&look, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right, pad);

    // -- buttons ------------------------------------------------------------------------
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let reset = Button::builder(&dlg).with_label("Defaults").build();
    let cancel = Button::builder(&dlg).with_id(ID_CANCEL).with_label("Cancel").build();
    let ok = Button::builder(&dlg).with_id(ID_OK).with_label("OK").build();
    buttons.add(&reset, 0, SizerFlag::Right, 0);
    buttons.add_stretch_spacer(1);
    buttons.add(&cancel, 0, SizerFlag::Right, 6);
    buttons.add(&ok, 0, SizerFlag::Right, 0);
    root.add_sizer(&buttons, 0, SizerFlag::Expand | SizerFlag::All, pad);
    {
        let d = dlg;
        reset.on_click(move |_| d.end_modal(1));
    }

    dlg.set_sizer_and_fit(root, true);
    dlg.centre();
    let code = dlg.show_modal();
    if code == 1 {
        return Some(StatsConfig::default());
    }
    if code != ID_OK {
        return None;
    }
    let mut out = current.clone();
    out.items = (0..list.get_count()).filter(|i| list.is_checked(*i)).map(|i| order.borrow()[i as usize].0).collect();
    out.style = StatStyle::ALL[style.get_selection().unwrap_or(0) as usize];
    out.rows = ROW_CHOICES[rows.get_selection().unwrap_or(0) as usize];
    out.page_seconds = page.value().max(1) as u32;
    out.interval_ms = INTERVALS_MS[interval.get_selection().unwrap_or(2) as usize];
    let c = accent.get_colour();
    out.accent = [c.r, c.g, c.b];
    out.load_colors = load_colors.get_value();
    out.temp_with_usage = temp_with.get_value();
    out.fahrenheit = fahrenheit.get_value();
    Some(out)
}
