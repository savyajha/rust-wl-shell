//! The component every right-side item is built on: a flat button in the bar (hidden until
//! its item has something to show), opening GTK's own popover under it. GTK places it,
//! closes it on an outside click (one open at a time) and on Escape; it is frosted as the
//! bar is.

use std::cell::Cell;
use std::rc::Rc;

use gtk4::{self as gtk, glib, prelude::*};
use gtk4_layer_shell::{KeyboardMode, LayerShell};

use crate::blur::Blur;

pub struct Item {
    pub button: gtk::MenuButton,
    pub popover: gtk::Popover,
    /// The popover's rows, top to bottom.
    pub content: gtk::Box,
}

impl Item {
    /// An item showing `face` in the bar, named `name` for screen readers.
    pub fn new(face: &impl IsA<gtk::Widget>, name: &str, blur: &Option<Rc<Blur>>) -> Self {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let popover = gtk::Popover::builder()
            .child(&content)
            .has_arrow(false)
            .css_classes(["item-popover"])
            .build();
        let button = gtk::MenuButton::builder()
            .child(face)
            .popover(&popover)
            .css_classes(["item"])
            .visible(false)
            .build();
        button.update_property(&[gtk::accessible::Property::Label(name)]);
        // The bar takes no keyboard, so that clicks on it leave the focus where it is; while
        // a popover is open it asks for it on demand, for Escape (a click on any window
        // takes it back, should it ever be left asked for).
        popover.connect_show(|popover| keyboard(popover, KeyboardMode::OnDemand));
        popover.connect_closed(|popover| keyboard(popover, KeyboardMode::None));
        // The radius of style.css.
        Blur::attach(blur, &popover, 10);
        Self {
            button,
            popover,
            content,
        }
    }

    /// Replace the rows in `rows` (part of the content) with `entries`; a click on one
    /// calls `choose` with its index and closes. A row is a flat button: a round icon
    /// (filled with the accent when chosen), its label, and what comes after it. In a list
    /// of choices a click on the chosen row does nothing, and for screen readers each is a
    /// radio button; in a list of `toggles` (any number on), a click on any row counts, and
    /// each is a check box.
    pub fn rows(
        &self,
        rows: &gtk::Box,
        entries: &[Row],
        toggles: bool,
        choose: impl Fn(usize) + Clone + 'static,
    ) {
        while let Some(child) = rows.first_child() {
            rows.remove(&child);
        }
        for (i, entry) in entries.iter().enumerate() {
            let circle = gtk::CenterBox::builder().css_classes(["circle"]).build();
            let inside = entry.circle.widget();
            inside.set_halign(gtk::Align::Center);
            inside.set_valign(gtk::Align::Center);
            circle.set_center_widget(Some(&inside));
            circle.set_valign(gtk::Align::Center);
            if entry.chosen {
                circle.add_css_class("chosen");
            }
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            content.append(&circle);
            let label = gtk::Label::builder()
                .label(&entry.label)
                .hexpand(true)
                .xalign(0.0)
                .build();
            content.append(&label);
            if let Some(after) = &entry.after {
                let after = after.widget();
                after.add_css_class("after");
                content.append(&after);
            }
            let role = match toggles {
                true => gtk::AccessibleRole::Checkbox,
                false => gtk::AccessibleRole::Radio,
            };
            let row = gtk::Button::builder()
                .child(&content)
                .css_classes(["row"])
                .accessible_role(role)
                .build();
            let checked = match entry.chosen {
                true => gtk::AccessibleTristate::True,
                false => gtk::AccessibleTristate::False,
            };
            row.update_property(&[gtk::accessible::Property::Label(&entry.label)]);
            row.update_state(&[gtk::accessible::State::Checked(checked)]);
            let (popover, choose, chosen) = (self.popover.clone(), choose.clone(), entry.chosen);
            row.connect_clicked(move |_| {
                // Shown as chosen only once the service says so: a refused choice leaves
                // the rows as they were.
                if toggles || !chosen {
                    choose(i);
                    popover.popdown();
                }
            });
            rows.append(&row);
        }
    }
}

/// A row of `Item::rows`.
pub struct Row {
    pub label: String,
    pub circle: Circle,
    pub chosen: bool,
    /// At the row's end: a lock, a battery's level.
    pub after: Option<Circle>,
}

/// What a row's circle holds, or what is after its label.
pub enum Circle {
    Icon(&'static str),
    /// A short text, as "DE".
    Text(String),
}

impl Circle {
    fn widget(&self) -> gtk::Widget {
        match self {
            Circle::Icon(name) => gtk::Image::from_icon_name(name).upcast(),
            Circle::Text(text) => gtk::Label::new(Some(text)).upcast(),
        }
    }
}

fn keyboard(popover: &gtk::Popover, mode: KeyboardMode) {
    if let Some(window) = popover.root().and_downcast::<gtk::Window>() {
        window.set_keyboard_mode(mode);
    }
}

/// A slider from 0 to 100 % in a popover, after `icon` (an image, or a button). Moved by the
/// user, it calls `moved`; `show` moves it without.
pub struct Slider {
    pub row: gtk::Box,
    scale: gtk::Scale,
    shown: Rc<Cell<u32>>,
}

impl Slider {
    pub fn new(icon: &impl IsA<gtk::Widget>, name: &str, moved: impl Fn(u32) + 'static) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.add_css_class("slider-row");
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        scale.set_hexpand(true);
        scale.update_property(&[gtk::accessible::Property::Label(name)]);
        row.append(icon);
        row.append(&scale);
        let shown = Rc::new(Cell::new(u32::MAX));
        scale.connect_value_changed(glib::clone!(
            #[strong]
            shown,
            move |scale| {
                let value = scale.value().round() as u32;
                if value != shown.replace(value) {
                    moved(value);
                }
            }
        ));
        Self { row, scale, shown }
    }

    pub fn show(&self, value: u32) {
        // Above 100 (set so elsewhere) the slider stops at 100: not a move to 100.
        self.shown.set(value.min(100));
        self.scale.set_value(f64::from(value));
    }
}

/// A popover's title with a switch at its end, as macOS's "Wi-Fi" and "Bluetooth". Flipped
/// by the user, it calls `flipped`; `show` sets it without.
pub struct Switch {
    pub row: gtk::Box,
    switch: gtk::Switch,
    shown: Rc<Cell<bool>>,
}

impl Switch {
    pub fn new(title: &str, flipped: impl Fn(bool) + 'static) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.add_css_class("switch-row");
        let label = gtk::Label::builder()
            .label(title)
            .hexpand(true)
            .xalign(0.0)
            .build();
        label.add_css_class("title");
        let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        switch.update_property(&[gtk::accessible::Property::Label(title)]);
        row.append(&label);
        row.append(&switch);
        let shown = Rc::new(Cell::new(false));
        switch.connect_state_set(glib::clone!(
            #[strong]
            shown,
            move |_, on| {
                if on != shown.get() {
                    flipped(on);
                }
                // The switch moves, but stays on its state until the service says so.
                glib::Propagation::Stop
            }
        ));
        Self { row, switch, shown }
    }

    pub fn show(&self, on: bool) {
        self.shown.set(on);
        self.switch.set_active(on);
        self.switch.set_state(on);
    }
}
