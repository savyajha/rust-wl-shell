//! The Bluetooth item, as macOS's menu: the adapter's switch, then the paired devices, a
//! click connecting or disconnecting one. No pairing: that is Settings' job.

use std::cell::RefCell;

use gtk4::{self as gtk, prelude::*};
use shell_core::bluetooth::{Bluetooth, Device, Request};
use tokio::sync::mpsc::UnboundedSender;

use crate::popover::{Circle, Item, Row, Switch};

pub struct BluetoothItem {
    pub item: Item,
    icon: gtk::Image,
    switch: Switch,
    /// "Devices", the rows, or "No devices".
    devices: (gtk::Box, gtk::Box, gtk::Label),
    shown: RefCell<Option<Bluetooth>>,
    requests: UnboundedSender<Request>,
}

impl BluetoothItem {
    pub fn new(requests: UnboundedSender<Request>) -> Self {
        let icon = gtk::Image::new();
        let item = Item::new(&icon, "Bluetooth");
        let sent = requests.clone();
        let switch = Switch::new("Bluetooth", move |on| {
            let _ = sent.send(Request::Power(on));
        });
        let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
        section.add_css_class("section");
        let title = gtk::Label::builder().label("Devices").xalign(0.0).build();
        title.add_css_class("title");
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let none = gtk::Label::builder()
            .label("No devices")
            .xalign(0.0)
            .build();
        none.add_css_class("line");
        for widget in [
            title.upcast_ref::<gtk::Widget>(),
            rows.upcast_ref(),
            none.upcast_ref(),
        ] {
            section.append(widget);
        }
        item.content.append(&switch.row);
        item.content.append(&section);
        Self {
            item,
            icon,
            switch,
            devices: (section, rows, none),
            shown: RefCell::default(),
            requests,
        }
    }

    pub fn show(&self, bluetooth: Option<&Bluetooth>) {
        self.item.button.set_visible(bluetooth.is_some());
        let Some(bluetooth) = bluetooth else {
            self.item.popover.popdown();
            return;
        };
        if self.shown.borrow().as_ref() == Some(bluetooth) {
            return;
        }
        self.shown.replace(Some(bluetooth.clone()));
        let connected = bluetooth.devices.iter().any(|d| d.connected);
        let (icon, label) = match (bluetooth.powered, connected) {
            (false, _) => ("bluetooth-disabled-symbolic", "Bluetooth: off"),
            (true, true) => ("bluetooth-active-symbolic", "Bluetooth: connected"),
            (true, false) => ("bluetooth-disconnected-symbolic", "Bluetooth: on"),
        };
        self.icon.set_icon_name(Some(icon));
        self.item
            .button
            .update_property(&[gtk::accessible::Property::Label(label)]);
        self.switch.show(bluetooth.powered);
        let (section, rows, none) = &self.devices;
        section.set_visible(bluetooth.powered);
        none.set_visible(bluetooth.devices.is_empty());
        let entries: Vec<Row> = bluetooth
            .devices
            .iter()
            .map(|d| Row {
                label: d.name.clone(),
                circle: Circle::Icon(device_icon(d)),
                chosen: d.connected,
                after: d.battery.map(|b| Circle::Text(format!("{b}%"))),
            })
            .collect();
        let (requests, devices) = (self.requests.clone(), bluetooth.devices.clone());
        self.item.rows(rows, &entries, true, move |i| {
            let device = &devices[i];
            let path = device.path.clone();
            let request = match device.connected {
                true => Request::Disconnect(path),
                false => Request::Connect(path),
            };
            let _ = requests.send(request);
        });
    }
}

/// The theme's symbolic icon for BlueZ's name of a device's kind.
fn device_icon(device: &Device) -> &'static str {
    match device.icon.as_deref() {
        Some("audio-headphones" | "audio-headset") => "audio-headphones-symbolic",
        Some("audio-card") => "audio-speakers-symbolic",
        Some("input-mouse") => "input-mouse-symbolic",
        Some("input-keyboard") => "input-keyboard-symbolic",
        Some("input-gaming") => "input-gaming-symbolic",
        Some("input-tablet") => "input-tablet-symbolic",
        Some("phone") => "phone-symbolic",
        Some("computer") => "computer-symbolic",
        _ => "bluetooth-symbolic",
    }
}
