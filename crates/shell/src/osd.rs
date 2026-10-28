//! The on-screen display: a frosted pill 48 px above the bottom edge of the focused output,
//! with an icon, a level and its value, shown for 1.5 s after the last change (vibepanel's
//! look and numbers). It is on the overlay layer, takes no input and reserves no space.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::{self as gtk, cairo, gdk, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::blur::Blur;

const MARGIN: i32 = 48;
const SHOWN_FOR: Duration = Duration::from_millis(1500);

pub struct Osd {
    window: gtk::Window,
    icon: gtk::Image,
    level: gtk::LevelBar,
    value: gtk::Label,
    /// Each showing's number; a hiding timer hides only its own.
    showings: Rc<Cell<u32>>,
}

impl Osd {
    pub fn new(blur: &Option<Rc<Blur>>) -> Self {
        let window = gtk::Window::builder().css_classes(["osd"]).build();
        window.init_layer_shell();
        window.set_namespace(Some("rust-wl-shell-osd"));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_anchor(Edge::Bottom, true);
        window.set_margin(Edge::Bottom, MARGIN);
        // No input: clicks go to what is under it.
        window.connect_realize(|window| {
            if let Some(surface) = window.surface() {
                surface.set_input_region(Some(&cairo::Region::create()));
            }
        });
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let (icon, value) = (gtk::Image::new(), gtk::Label::new(None));
        let level = gtk::LevelBar::for_interval(0.0, 100.0);
        level.set_valign(gtk::Align::Center);
        row.append(&icon);
        row.append(&level);
        row.append(&value);
        window.set_child(Some(&row));
        // A pill: rounded at half its height.
        Blur::attach(blur, &window, i32::MAX);
        Self {
            window,
            icon,
            level,
            value,
            showings: Rc::default(),
        }
    }

    /// Show `icon` and `percent` on `monitor`, for 1.5 s from now.
    pub fn show(&self, icon: &str, percent: u32, monitor: Option<&gdk::Monitor>) {
        self.icon.set_icon_name(Some(icon));
        self.level.set_value(f64::from(percent.min(100)));
        self.value.set_label(&percent.to_string());
        if self.window.monitor().as_ref() != monitor {
            self.window.set_monitor(monitor);
        }
        self.window.present();
        let showing = self.showings.get() + 1;
        self.showings.set(showing);
        let (window, showings) = (self.window.clone(), self.showings.clone());
        glib::timeout_add_local_once(SHOWN_FOR, move || {
            if showings.get() == showing {
                window.set_visible(false);
            }
        });
    }
}
