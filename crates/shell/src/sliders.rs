//! The items with a slider: the volume (its icon a mute button, then the outputs) and the
//! brightness.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};
use shell_core::audio::{self, Audio, Kind, Output, Request};
use tokio::sync::mpsc::UnboundedSender;

use crate::blur::Blur;
use crate::popover::{Circle, Item, Row, Slider};

pub struct VolumeItem {
    pub item: Item,
    icon: gtk::Image,
    slider: Slider,
    /// The slider's icon: a click mutes or unmutes.
    mute: gtk::Button,
    muted: Rc<Cell<bool>>,
    /// The outputs' title, their rows, and the outputs the rows show.
    outputs: (gtk::Label, gtk::Box, RefCell<Vec<Output>>),
    requests: audio::Requests,
}

impl VolumeItem {
    pub fn new(requests: audio::Requests, blur: &Option<Rc<Blur>>) -> Self {
        let icon = gtk::Image::new();
        let item = Item::new(&icon, "Volume", blur);
        let mute = gtk::Button::builder().css_classes(["mute"]).build();
        let muted = Rc::new(Cell::new(false));
        let (sent, shown) = (requests.clone(), muted.clone());
        mute.connect_clicked(move |_| sent.send(Request::Mute(!shown.get())));
        let sent = requests.clone();
        let slider = Slider::new(&mute, "Volume", move |volume| {
            sent.send(Request::Volume(volume));
        });
        let outputs = (
            title("Output"),
            gtk::Box::new(gtk::Orientation::Vertical, 0),
            RefCell::default(),
        );
        item.content.append(&title("Sound"));
        item.content.append(&slider.row);
        // Shown once there are outputs.
        outputs.0.set_visible(false);
        item.content.append(&outputs.0);
        item.content.append(&outputs.1);
        Self {
            item,
            icon,
            slider,
            mute,
            muted,
            outputs,
            requests,
        }
    }

    pub fn show(&self, audio: Option<&Audio>) {
        self.item.button.set_visible(audio.is_some());
        let Some(audio) = audio else {
            self.item.popover.popdown();
            return;
        };
        self.icon.set_icon_name(Some(volume_icon(audio)));
        let name = match audio.muted {
            true => "Volume muted".to_string(),
            false => format!("Volume {}%", audio.volume),
        };
        let button = &self.item.button;
        button.update_property(&[gtk::accessible::Property::Label(&name)]);
        self.slider.show(audio.volume);
        self.muted.set(audio.muted);
        let (icon, action) = match audio.muted {
            true => ("audio-volume-muted-symbolic", "Unmute"),
            false => ("audio-volume-high-symbolic", "Mute"),
        };
        self.mute.set_icon_name(icon);
        self.mute
            .update_property(&[gtk::accessible::Property::Label(action)]);
        // Rebuilt only when they change: not on each step of the slider.
        let (title, rows, shown) = &self.outputs;
        if *shown.borrow() == audio.outputs {
            return;
        }
        shown.replace(audio.outputs.clone());
        title.set_visible(!audio.outputs.is_empty());
        let entries: Vec<Row> = audio
            .outputs
            .iter()
            .map(|output| Row {
                label: output.name.clone(),
                circle: Circle::Icon(output_icon(output.kind)),
            })
            .collect();
        let chosen = audio.outputs.iter().position(|output| output.active);
        let (requests, outputs) = (self.requests.clone(), audio.outputs.clone());
        // Its port too, unless its sink already has it.
        let choose = move |i: usize| {
            let output = &outputs[i];
            let port = output.port.clone().filter(|_| !output.port_active);
            let sink = output.sink.clone();
            requests.send(Request::Default { sink, port });
        };
        let chosen = chosen.unwrap_or(usize::MAX);
        self.item.rows(rows, &entries, chosen, choose);
    }
}

pub struct BrightnessItem {
    pub item: Item,
    slider: Slider,
}

impl BrightnessItem {
    pub fn new(requests: UnboundedSender<u8>, blur: &Option<Rc<Blur>>) -> Self {
        let item = Item::new(&gtk::Image::from_icon_name(BRIGHTNESS), "Brightness", blur);
        let sun = gtk::Image::from_icon_name(BRIGHTNESS);
        let slider = Slider::new(&sun, "Brightness", move |level| {
            // Fails only once the core has stopped, and the shell with it.
            let _ = requests.send(level as u8);
        });
        item.content.append(&title("Display"));
        item.content.append(&slider.row);
        Self { item, slider }
    }

    pub fn show(&self, level: Option<u8>) {
        self.item.button.set_visible(level.is_some());
        let Some(level) = level else {
            self.item.popover.popdown();
            return;
        };
        let name = format!("Brightness {level}%");
        let button = &self.item.button;
        button.update_property(&[gtk::accessible::Property::Label(&name)]);
        self.slider.show(level.into());
    }
}

pub const BRIGHTNESS: &str = "display-brightness-symbolic";

/// The theme's icon for an output's kind.
pub fn output_icon(kind: Kind) -> &'static str {
    match kind {
        Kind::Speaker => "audio-speakers-symbolic",
        Kind::Headphones => "audio-headphones-symbolic",
        Kind::Display => "video-display-symbolic",
        Kind::Bluetooth => "bluetooth-symbolic",
    }
}

/// The theme's icon for the volume: muted at 0, then by thirds.
pub fn volume_icon(audio: &Audio) -> &'static str {
    match audio.volume {
        _ if audio.muted => "audio-volume-muted-symbolic",
        0 => "audio-volume-muted-symbolic",
        1..34 => "audio-volume-low-symbolic",
        34..67 => "audio-volume-medium-symbolic",
        _ => "audio-volume-high-symbolic",
    }
}

fn title(text: &str) -> gtk::Label {
    let title = gtk::Label::builder().label(text).xalign(0.0).build();
    title.add_css_class("title");
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_icons_by_level_and_mute() {
        let icon = |volume, muted| {
            let audio = Audio {
                volume,
                muted,
                default: String::new(),
                outputs: Vec::new(),
            };
            volume_icon(&audio)
        };
        assert_eq!(icon(0, false), "audio-volume-muted-symbolic");
        assert_eq!(icon(80, true), "audio-volume-muted-symbolic");
        assert_eq!(icon(20, false), "audio-volume-low-symbolic");
        assert_eq!(icon(50, false), "audio-volume-medium-symbolic");
        assert_eq!(icon(67, false), "audio-volume-high-symbolic");
        assert_eq!(icon(150, false), "audio-volume-high-symbolic");
    }
}
