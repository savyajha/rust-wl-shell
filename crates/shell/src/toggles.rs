//! The bar's click toggles, with no popover: the night light and keep awake. Each is a flat
//! button whose face tells its state, never a fill: the night light's moon, struck
//! through when off; keep awake's cup with a green tick while on, as VPN's. A click asks
//! for the other state, shown once the core says it is so. Hidden until the core sends a
//! state.

use std::cell::Cell;
use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};

use crate::popover::badged;

/// How a toggle shows its state.
pub enum Look {
    /// An icon each, on and off.
    Icons([&'static str; 2]),
    /// One icon, with a green tick while on.
    Tick(&'static str),
}

/// The night light's, on and off: the theme's moon, as GNOME's night light.
pub const NIGHT_LIGHT: Look =
    Look::Icons(["night-light-symbolic", "night-light-disabled-symbolic"]);
/// Keep awake's: a steaming cup, as macOS's Amphetamine and GNOME's Caffeine show it; the
/// shell's own, as Adwaita has none.
pub const KEEP_AWAKE: Look = Look::Tick("rust-wl-shell-keep-awake-symbolic");

pub struct Toggle {
    pub button: gtk::Button,
    name: &'static str,
    face: Face,
    on: Rc<Cell<bool>>,
}

/// A toggle's face, as built from its `Look`.
enum Face {
    Icons([&'static str; 2]),
    /// The tick, shown while on.
    Tick(gtk::Image),
}

impl Toggle {
    /// A toggle with its `look`, named `name` for screen readers; a click calls `flipped`.
    pub fn new(look: Look, name: &'static str, flipped: impl Fn(bool) + 'static) -> Self {
        let button = gtk::Button::builder()
            .css_classes(["toggle"])
            .focus_on_click(false)
            .visible(false)
            .build();
        let face = match look {
            Look::Icons(icons) => {
                button.set_icon_name(icons[1]);
                Face::Icons(icons)
            }
            Look::Tick(icon) => {
                let (overlay, tick) = badged(icon);
                button.set_child(Some(&overlay));
                Face::Tick(tick)
            }
        };
        let on = Rc::new(Cell::new(false));
        let shown = on.clone();
        button.connect_clicked(move |_| flipped(!shown.get()));
        Self {
            button,
            name,
            face,
            on,
        }
    }

    /// On or off; `None` hides it.
    pub fn show(&self, on: Option<bool>) {
        self.button.set_visible(on.is_some());
        let on = on.unwrap_or(false);
        self.on.set(on);
        match &self.face {
            Face::Icons(icons) => self.button.set_icon_name(icons[usize::from(!on)]),
            Face::Tick(tick) => tick.set_visible(on),
        }
        let label = format!("{}: {}", self.name, if on { "on" } else { "off" });
        self.button
            .update_property(&[gtk::accessible::Property::Label(&label)]);
    }
}
