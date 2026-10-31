//! Settings dialog for the system monitor.

use crate::config::{StatGraphic, StatItem, StatsConfig, ValuePosition};
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

    // -- items: shown (in screen order) and available --------------------------------
    let items_box = StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, &dlg, "Values").build();
    let ib = items_box.get_static_box().unwrap();
    let hint = StaticText::builder(&ib)
        .with_label("Left: shown on the screen, in this order. Double-click to move a value across.")
        .build();
    items_box.add(&hint, 0, SizerFlag::All, 4);

    let shown = Rc::new(RefCell::new(current.items.clone()));
    let lists = BoxSizer::builder(Orientation::Horizontal).build();
    let list_size = Size::new(190, 300);
    let shown_list = ListBox::builder(&ib).with_size(list_size).build();
    let avail_list = ListBox::builder(&ib).with_size(list_size).build();
    // fill both lists; `sel` selects an entry in the shown (true) or available list
    let refill = {
        let shown = shown.clone();
        Rc::new(move |sel: Option<(bool, StatItem)>| {
            let s = shown.borrow();
            let avail: Vec<StatItem> = StatItem::ALL.into_iter().filter(|i| !s.contains(i)).collect();
            shown_list.clear();
            for i in s.iter() {
                shown_list.append(i.name());
            }
            avail_list.clear();
            for i in &avail {
                avail_list.append(i.name());
            }
            match sel {
                Some((true, item)) => {
                    if let Some(p) = s.iter().position(|i| *i == item) {
                        shown_list.set_selection(p as u32, true);
                    }
                }
                Some((false, item)) => {
                    if let Some(p) = avail.iter().position(|i| *i == item) {
                        avail_list.set_selection(p as u32, true);
                    }
                }
                None => {}
            }
        })
    };
    refill(None);
    let selected_avail = move |shown: &[StatItem]| {
        let avail: Vec<StatItem> = StatItem::ALL.into_iter().filter(|i| !shown.contains(i)).collect();
        avail_list.get_selection().and_then(|i| avail.get(i as usize).copied())
    };
    let add = {
        let (shown, refill) = (shown.clone(), refill.clone());
        Rc::new(move || {
            let item = selected_avail(&shown.borrow());
            if let Some(item) = item {
                shown.borrow_mut().push(item);
                refill(Some((true, item)));
            }
        })
    };
    let remove = {
        let (shown, refill) = (shown.clone(), refill.clone());
        Rc::new(move || {
            let Some(i) = shown_list.get_selection() else { return };
            let item = shown.borrow_mut().remove(i as usize);
            refill(Some((false, item)));
        })
    };
    let buttons_col = BoxSizer::builder(Orientation::Vertical).build();
    let up = Button::builder(&ib).with_label("▲ Up").build();
    let down = Button::builder(&ib).with_label("▼ Down").build();
    let add_btn = Button::builder(&ib).with_label("◀ Add").build();
    let remove_btn = Button::builder(&ib).with_label("Remove ▶").build();
    for (btn, delta) in [(up, -1i32), (down, 1i32)] {
        let (shown, refill) = (shown.clone(), refill.clone());
        btn.on_click(move |_| {
            let Some(sel) = shown_list.get_selection() else { return };
            let target = sel as i32 + delta;
            let n = shown.borrow().len() as i32;
            if target < 0 || target >= n {
                return;
            }
            shown.borrow_mut().swap(sel as usize, target as usize);
            let item = shown.borrow()[target as usize];
            refill(Some((true, item)));
        });
    }
    {
        let a = add.clone();
        add_btn.on_click(move |_| a());
        let a = add.clone();
        avail_list.on_item_double_clicked(move |_| a());
        let r = remove.clone();
        remove_btn.on_click(move |_| r());
        let r = remove.clone();
        shown_list.on_item_double_clicked(move |_| r());
    }
    for b in [&up, &down] {
        buttons_col.add(b, 0, SizerFlag::Expand | SizerFlag::Bottom, 4);
    }
    buttons_col.add_spacer(16);
    for b in [&add_btn, &remove_btn] {
        buttons_col.add(b, 0, SizerFlag::Expand | SizerFlag::Bottom, 4);
    }
    lists.add(&shown_list, 1, SizerFlag::Expand, 0);
    lists.add_sizer(&buttons_col, 0, SizerFlag::Left | SizerFlag::Right, 6);
    lists.add(&avail_list, 1, SizerFlag::Expand, 0);
    items_box.add_sizer(&lists, 1, SizerFlag::Expand | SizerFlag::All, 4);
    root.add_sizer(&items_box, 1, SizerFlag::Expand | SizerFlag::All, pad);

    // -- look ---------------------------------------------------------------------------
    let look = StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, &dlg, "Look").build();
    let lb = look.get_static_box().unwrap();
    let grid = FlexGridSizer::builder(0, 2).with_vgap(6).with_hgap(8).build();
    grid.add_growable_col(1, 1);
    let cv = SizerFlag::AlignCenterVertical;
    let label = |t: &str| StaticText::builder(&lb).with_label(t).build();

    let graphic =
        Choice::builder(&lb).with_choices(StatGraphic::ALL.iter().map(|s| s.name().to_string()).collect()).build();
    graphic.set_selection(StatGraphic::ALL.iter().position(|s| *s == current.graphic).unwrap_or(1) as u32);
    graphic.set_tooltip("Bars show the current value, the histogram the last minutes.");
    grid.add(&label("Graphic:"), 0, cv, 0);
    grid.add(&graphic, 0, cv | SizerFlag::Expand, 0);

    let value_pos =
        Choice::builder(&lb).with_choices(ValuePosition::ALL.iter().map(|s| s.name().to_string()).collect()).build();
    value_pos.set_selection(ValuePosition::ALL.iter().position(|s| *s == current.value_position).unwrap_or(2) as u32);
    value_pos.set_tooltip(
        "Where the number (e.g. 45%) goes. On top of the graphic it is drawn in the opposite\n\
         colour of what is underneath, with a black outline.",
    );
    grid.add(&label("Value:"), 0, cv, 0);
    grid.add(&value_pos, 0, cv | SizerFlag::Expand, 0);

    let gap = SpinCtrl::builder(&lb)
        .with_min_value(0)
        .with_max_value(40)
        .with_initial_value(current.label_gap as i32)
        .build();
    gap.set_tooltip("Space in pixels between the labels (CPU, RAM, ...) and the graphic or number after them.");
    grid.add(&label("Gap after labels (px):"), 0, cv, 0);
    grid.add(&gap, 0, cv, 0);

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
    look.add_sizer(&grid, 0, SizerFlag::Expand | SizerFlag::All, 6);

    let outline = CheckBox::builder(&lb).with_label("Outline around bars and histograms").build();
    outline.set_value(current.outline);
    let load_colors =
        CheckBox::builder(&lb).with_label("Colour bars and histograms by level (green / yellow / red)").build();
    load_colors.set_value(current.load_colors);
    load_colors.set_tooltip("Otherwise they use the label colour.");
    let temp_with = CheckBox::builder(&lb).with_label("Show temperature next to CPU and GPU usage").build();
    temp_with.set_value(current.temp_with_usage);
    let fahrenheit = CheckBox::builder(&lb).with_label("Temperatures in °F").build();
    fahrenheit.set_value(current.fahrenheit);
    for c in [&outline, &load_colors, &temp_with, &fahrenheit] {
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
    out.items = shown.borrow().clone();
    out.graphic = StatGraphic::ALL[graphic.get_selection().unwrap_or(1) as usize];
    out.value_position = ValuePosition::ALL[value_pos.get_selection().unwrap_or(2) as usize];
    out.outline = outline.get_value();
    out.label_gap = gap.value().clamp(0, 40) as u32;
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
