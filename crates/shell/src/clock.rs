//! The clock: a button with the time (and Do Not Disturb's moon while it is on), which
//! opens the Notification Centre (`centre.rs`) on its output.

use std::cell::Cell;
use std::rc::Rc;

use gtk4::{self as gtk, gdk, glib, prelude::*};
use shell_core::notifications::Notifications;

use crate::centre::Centre;

pub struct ClockItem {
    pub button: gtk::Button,
    pub label: gtk::Label,
    /// Beside the time while Do Not Disturb is on, as macOS's Focus moon.
    dnd_icon: gtk::Image,
}

impl ClockItem {
    /// The clock on `monitor`'s bar; a click opens or closes the `centre` there (none
    /// without the notification server).
    pub fn new(centre: Option<Rc<Centre>>, monitor: &gdk::Monitor) -> Self {
        let label = gtk::Label::new(None);
        label.add_css_class("clock");
        let dnd_icon = gtk::Image::from_icon_name("notifications-disabled-symbolic");
        dnd_icon.set_visible(false);
        dnd_icon.update_property(&[gtk::accessible::Property::Label("Do Not Disturb")]);
        let face = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        face.add_css_class("face");
        face.append(&dnd_icon);
        face.append(&label);
        let button = gtk::Button::builder()
            .child(&face)
            .css_classes(["toggle"])
            .build();
        button.update_property(&[gtk::accessible::Property::Label("Clock")]);
        button.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
        if let Some(centre) = centre {
            // Open or closed as the centre was at the press: niri takes its keyboard on
            // the press, closing it, and the click must not open it again. With no press
            // (a screen reader's click), as it is.
            let was_open = Rc::new(Cell::new(None));
            let press = gtk::GestureClick::new();
            press.set_propagation_phase(gtk::PropagationPhase::Capture);
            let (pressed, opened) = (was_open.clone(), centre.clone());
            press.connect_pressed(move |_, _, _, _| pressed.set(Some(opened.is_open())));
            // Forgotten once the press is released or cancelled, so that a screen reader's
            // click later goes by the state. Not on `stopped`: GTK's double-click timer
            // emits it 400 ms into a held press, and a slow click would then reopen it.
            let forget = {
                let was_open = was_open.clone();
                move || {
                    let was_open = was_open.clone();
                    glib::idle_add_local_once(move || was_open.set(None));
                }
            };
            let cancelled = forget.clone();
            press.connect_cancel(move |_, _| cancelled());
            press.connect_released(move |_, _, _, _| forget());
            button.add_controller(press);
            let monitor = monitor.clone();
            button.connect_clicked(move |button| {
                match was_open.take().unwrap_or_else(|| centre.is_open()) {
                    true => centre.close(),
                    false => centre.open(&monitor, button),
                }
            });
        }
        Self {
            button,
            label,
            dnd_icon,
        }
    }

    pub fn show(&self, state: &Notifications) {
        self.dnd_icon.set_visible(state.dnd);
    }
}
