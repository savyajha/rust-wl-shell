//! The bar's click toggles, with no popover: the night light and keep awake. Each is a flat
//! button whose icon tells its state (on, or off: no fill); a click asks for the other
//! state, shown once the core says it is so. Hidden until the core sends a state.

use std::cell::Cell;
use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};

/// The night light's, on and off: the theme's moon, as GNOME's night light.
pub const NIGHT_LIGHT: [&str; 2] = ["night-light-symbolic", "night-light-disabled-symbolic"];
/// Keep awake's, on and off: a cup steaming, or not, as macOS's Amphetamine and GNOME's
/// Caffeine show it; the shell's own, as Adwaita has none.
pub const KEEP_AWAKE: [&str; 2] = [
    "rust-wl-shell-keep-awake-symbolic",
    "rust-wl-shell-keep-awake-off-symbolic",
];

pub struct Toggle {
    pub button: gtk::Button,
    name: &'static str,
    icons: [&'static str; 2],
    on: Rc<Cell<bool>>,
}

impl Toggle {
    /// A toggle with `icons` (on, off), named `name` for screen readers; a click calls
    /// `flipped`.
    pub fn new(
        icons: [&'static str; 2],
        name: &'static str,
        flipped: impl Fn(bool) + 'static,
    ) -> Self {
        let button = gtk::Button::builder()
            .icon_name(icons[1])
            .css_classes(["toggle"])
            .focus_on_click(false)
            .visible(false)
            .build();
        let on = Rc::new(Cell::new(false));
        let shown = on.clone();
        button.connect_clicked(move |_| flipped(!shown.get()));
        Self {
            button,
            name,
            icons,
            on,
        }
    }

    /// On or off; `None` hides it.
    pub fn show(&self, on: Option<bool>) {
        self.button.set_visible(on.is_some());
        let on = on.unwrap_or(false);
        self.on.set(on);
        self.button.set_icon_name(self.icons[usize::from(!on)]);
        let label = format!("{}: {}", self.name, if on { "on" } else { "off" });
        self.button
            .update_property(&[gtk::accessible::Property::Label(&label)]);
    }
}
