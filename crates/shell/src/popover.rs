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

    /// Replace the rows in `rows` (part of the content) with one radio row per label, the
    /// `chosen` one selected; a click on another calls `choose` with its index and closes.
    pub fn radio_rows(
        &self,
        rows: &gtk::Box,
        labels: &[String],
        chosen: usize,
        choose: impl Fn(usize) + Clone + 'static,
    ) {
        while let Some(child) = rows.first_child() {
            rows.remove(&child);
        }
        let buttons: Vec<gtk::CheckButton> = labels
            .iter()
            .map(|label| gtk::CheckButton::with_label(label))
            .collect();
        // Weak: each row's handler holds it, and rows hold their handlers.
        let shown = buttons.get(chosen).map(|row| row.downgrade());
        for (i, row) in buttons.iter().enumerate() {
            if i > 0 {
                row.set_group(Some(&buttons[0]));
            }
            row.set_active(i == chosen);
            let (popover, choose, shown) = (self.popover.clone(), choose.clone(), shown.clone());
            row.connect_toggled(move |row| {
                if row.is_active() && i != chosen {
                    choose(i);
                    popover.popdown();
                    // Shown again until the service's update rebuilds the rows: a choice
                    // that fails leaves things as they were.
                    if let Some(shown) = shown.as_ref().and_then(|shown| shown.upgrade()) {
                        shown.set_active(true);
                    }
                }
            });
            rows.append(row);
        }
    }
}

fn keyboard(popover: &gtk::Popover, mode: KeyboardMode) {
    if let Some(window) = popover.root().and_downcast::<gtk::Window>() {
        window.set_keyboard_mode(mode);
    }
}

/// A slider from 0 to 100 % in a popover, after `icon`. Moved by the user, it calls
/// `moved`; `show` moves it without.
pub struct Slider {
    pub row: gtk::Box,
    scale: gtk::Scale,
    shown: Rc<Cell<u32>>,
}

impl Slider {
    pub fn new(icon: &str, name: &str, moved: impl Fn(u32) + 'static) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.add_css_class("slider-row");
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        scale.set_hexpand(true);
        scale.update_property(&[gtk::accessible::Property::Label(name)]);
        row.append(&gtk::Image::from_icon_name(icon));
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
