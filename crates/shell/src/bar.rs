//! One output's bar: its workspaces and the focused app on the left; the keyboard layout,
//! the power profile, the brightness, the volume, the battery and the clock on the right.

use std::rc::Rc;

use gtk4::{self as gtk, gdk, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::{Component, ComponentParts, ComponentSender};
use shell_core::config::{Config, Position};
use shell_core::niri::{self, Workspace};
use shell_core::{Update, audio};
use tokio::sync::mpsc::UnboundedSender;

use crate::blur::Blur;
use crate::items::{BatteryItem, LayoutItem, ProfileItem};
use crate::sliders::{BrightnessItem, VolumeItem};

pub struct Bar {
    /// Its connector is the output's name, as niri names outputs.
    monitor: gdk::Monitor,
    niri: UnboundedSender<niri::Command>,
    /// Every output's workspaces, and this output's, as drawn.
    all: Vec<Workspace>,
    workspaces: Vec<Workspace>,
    app: String,
    clock: String,
    /// Each one `None` when the config switches it off.
    layout: Option<LayoutItem>,
    profile: Option<ProfileItem>,
    brightness: Option<BrightnessItem>,
    volume: Option<VolumeItem>,
    battery: Option<BatteryItem>,
}

pub struct Init {
    pub monitor: gdk::Monitor,
    pub config: Rc<Config>,
    pub niri: UnboundedSender<niri::Command>,
    /// The power profile to set.
    pub profiles: UnboundedSender<String>,
    pub audio: audio::Requests,
    /// The backlight's level to set.
    pub brightness: UnboundedSender<u8>,
    pub blur: Option<Rc<Blur>>,
}

#[derive(Debug)]
pub enum Msg {
    Update(Update),
    /// The focused window's app, by name.
    App(String),
    /// The monitor's connector became known: GDK can list a new monitor before it.
    Connector,
}

#[relm4::component(pub)]
impl Component for Bar {
    type Init = Init;
    type Input = Msg;
    type Output = ();
    type CommandOutput = ();

    view! {
        gtk::Window {
            add_css_class: "bar",
            gtk::CenterBox {
                #[wrap(Some)]
                set_start_widget = &gtk::Box {
                    add_css_class: "left",
                    #[name = "workspaces"]
                    gtk::Box {
                        set_visible: init.config.workspaces,
                    },
                    gtk::Label {
                        add_css_class: "app",
                        set_visible: init.config.app_name,
                        #[watch]
                        set_label: &model.app,
                    },
                },
                #[wrap(Some)]
                #[name = "right"]
                set_end_widget = &gtk::Box {
                    add_css_class: "right",
                    #[name = "clock"]
                    gtk::Label {
                        add_css_class: "clock",
                        #[watch]
                        set_label: &model.clock,
                    },
                },
            },
        }
    }

    fn init(init: Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        root.init_layer_shell();
        root.set_namespace(Some("rust-wl-shell"));
        root.set_layer(Layer::Top);
        root.set_keyboard_mode(KeyboardMode::None);
        root.set_monitor(Some(&init.monitor));
        let edge = match init.config.position {
            Position::Top => Edge::Top,
            Position::Bottom => Edge::Bottom,
        };
        for edge in [edge, Edge::Left, Edge::Right] {
            root.set_anchor(edge, true);
        }
        root.auto_exclusive_zone_enable();

        init.monitor
            .connect_connector_notify(move |_| sender.input(Msg::Connector));
        let config = &init.config;
        let model = Bar {
            monitor: init.monitor.clone(),
            niri: init.niri.clone(),
            all: Vec::new(),
            workspaces: Vec::new(),
            app: String::new(),
            clock: String::new(),
            layout: (config.keyboard_layout)
                .then(|| LayoutItem::new(init.niri.clone(), &init.blur)),
            profile: (config.power_profile)
                .then(|| ProfileItem::new(init.profiles.clone(), &init.blur)),
            brightness: (config.brightness)
                .then(|| BrightnessItem::new(init.brightness.clone(), &init.blur)),
            volume: (config.volume).then(|| VolumeItem::new(init.audio.clone(), &init.blur)),
            battery: (config.battery)
                .then(|| BatteryItem::new(config.battery_percentage, &init.blur)),
        };
        let widgets = view_output!();
        let layout = model.layout.as_ref().map(|item| &item.item.button);
        let profile = model.profile.as_ref().map(|item| &item.item.button);
        let brightness = model.brightness.as_ref().map(|item| &item.item.button);
        let volume = model.volume.as_ref().map(|item| &item.item.button);
        let battery = model.battery.as_ref().map(|item| &item.item.button);
        // Before the clock, in this order.
        let buttons = [layout, profile, brightness, volume, battery];
        for button in buttons.into_iter().flatten() {
            button.insert_before(&widgets.right, Some(&widgets.clock));
        }
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Msg,
        sender: ComponentSender<Self>,
        _: &Self::Root,
    ) {
        match msg {
            Msg::Update(Update::Niri(snapshot)) => {
                self.all = snapshot.workspaces;
                if let Some(layout) = &mut self.layout {
                    layout.show(&snapshot.layouts, snapshot.layout);
                }
            }
            Msg::Update(Update::Clock(clock)) => self.clock = clock,
            Msg::Update(Update::Battery(battery)) => {
                if let Some(item) = &self.battery {
                    item.show(battery.as_ref());
                }
            }
            Msg::Update(Update::Audio(audio)) => {
                if let Some(item) = &self.volume {
                    item.show(audio.as_ref());
                }
            }
            Msg::Update(Update::Backlight(level)) => {
                if let Some(item) = &self.brightness {
                    item.show(level);
                }
            }
            Msg::Update(Update::Profiles(profiles)) => {
                if let Some(item) = &self.profile {
                    item.show(profiles.as_ref());
                }
            }
            Msg::App(app) => self.app = app,
            Msg::Connector => {}
        }
        let connector = self.monitor.connector().map(String::from);
        let ours: Vec<Workspace> = self
            .all
            .iter()
            .filter(|ws| ws.output.is_some() && ws.output == connector)
            .cloned()
            .collect();
        if ours != self.workspaces {
            self.workspaces = ours;
            self.draw_workspaces(&widgets.workspaces);
        }
        self.update_view(widgets, sender);
    }
}

impl Bar {
    /// One dot per workspace, rebuilt whenever they change.
    fn draw_workspaces(&self, row: &gtk::Box) {
        while let Some(child) = row.first_child() {
            row.remove(&child);
        }
        for ws in &self.workspaces {
            let dot = gtk::Box::builder()
                .css_classes(["dot"])
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build();
            let button = gtk::Button::builder()
                .css_classes(["workspace"])
                .child(&dot)
                .focus_on_click(false)
                .build();
            if ws.active {
                button.add_css_class("active");
            }
            let label = match &ws.name {
                Some(name) => name.clone(),
                None => format!("Workspace {}", ws.idx),
            };
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
            let (niri, id) = (self.niri.clone(), ws.id);
            button.connect_clicked(move |_| {
                // Fails only once the core has stopped, and the shell with it.
                let _ = niri.send(niri::Command::FocusWorkspace(id));
            });
            row.append(&button);
        }
    }
}

impl Bar {
    /// Whether this bar's volume and brightness popovers are open, where the user moves
    /// their sliders.
    pub fn sliders_open(&self) -> (bool, bool) {
        let open =
            |item: Option<&crate::popover::Item>| item.is_some_and(|i| i.popover.is_visible());
        (
            open(self.volume.as_ref().map(|v| &v.item)),
            open(self.brightness.as_ref().map(|b| &b.item)),
        )
    }
}
