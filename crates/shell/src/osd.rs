//! The on-screen display: a frosted pill `osd-margin` (48 px) above the bottom edge of the
//! focused output, with an icon, a level and its value, shown for `osd-timeout` (1.5 s)
//! after the last change (vibepanel's look and numbers). It is on the overlay layer, takes no input and reserves no space.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4::{self as gtk, cairo, gdk, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use shell_core::config::Config;

use crate::blur::{Blur, Corners};

/// The OSD's window, with its icon, level and value.
type Shown = (gtk::Window, gtk::Image, gtk::LevelBar, gtk::Label);

pub struct Osd {
    /// Its window and what it shows, only while it is shown: one hidden would be GTK's
    /// last toplevel once no bar is left, which its AT-SPI root crashes on (`Banners`).
    shown: Rc<RefCell<Option<Shown>>>,
    /// Each showing's number; a hiding timer hides only its own.
    showings: Rc<Cell<u32>>,
    shown_for: Duration,
    margin: i32,
    blur: Option<Rc<Blur>>,
}

impl Osd {
    pub fn new(config: &Config, blur: &Option<Rc<Blur>>) -> Self {
        Self {
            shown: Rc::default(),
            showings: Rc::default(),
            shown_for: Duration::from_millis(config.osd_timeout.into()),
            margin: config.osd_margin as i32,
            blur: blur.clone(),
        }
    }

    fn build(&self) -> Shown {
        let window = gtk::Window::builder().css_classes(["osd"]).build();
        window.init_layer_shell();
        window.set_namespace(Some("rust-wl-shell-osd"));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_anchor(Edge::Bottom, true);
        window.set_margin(Edge::Bottom, self.margin);
        // No input: clicks go to what is under it.
        window.connect_realize(|window| {
            if let Some(surface) = window.surface() {
                surface.set_input_region(Some(&cairo::Region::create()));
            }
        });
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let (icon, value) = (gtk::Image::new(), gtk::Label::new(None));
        let level = gtk::LevelBar::for_interval(0.0, 100.0);
        level.set_valign(gtk::Align::Center);
        row.append(&icon);
        row.append(&level);
        row.append(&value);
        window.set_child(Some(&row));
        // A pill: rounded at half its height.
        Blur::attach(&self.blur, &window, Corners::Pill);
        (window, icon, level, value)
    }

    /// Show `icon` and `percent` on `monitor`, for `osd-timeout` from now.
    pub fn show(&self, icon: &str, percent: u32, monitor: Option<&gdk::Monitor>) {
        let (window, image, level, value) = self
            .shown
            .borrow_mut()
            .get_or_insert_with(|| self.build())
            .clone();
        image.set_icon_name(Some(icon));
        level.set_value(f64::from(percent.min(100)));
        value.set_label(&percent.to_string());
        if window.monitor().as_ref() != monitor {
            window.set_monitor(monitor);
        }
        window.present();
        let showing = self.showings.get() + 1;
        self.showings.set(showing);
        let (shown, showings) = (self.shown.clone(), self.showings.clone());
        glib::timeout_add_local_once(self.shown_for, move || {
            if showings.get() == showing
                && let Some((window, ..)) = shown.take()
            {
                window.destroy();
            }
        });
    }
}
