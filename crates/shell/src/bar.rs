//! One output's bar: its workspaces and the focused app on the left; the keyboard layout,
//! the brightness, the volume, VPN, Bluetooth, the network, the battery (with the power
//! mode) and the clock on the right.

use std::rc::Rc;

use gtk4::{self as gtk, gdk, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::{Component, ComponentParts, ComponentSender};
use shell_core::config::{Config, Position};
use shell_core::niri::{self, Workspace};
use shell_core::{Update, audio, bluetooth, network};
use tokio::sync::mpsc::UnboundedSender;

use crate::bluetooth::BluetoothItem;
use crate::blur::Blur;
use crate::dialog::Dialog;
use crate::items::{BatteryItem, LayoutItem};
use crate::network::{NetworkItem, VpnItem};
use crate::popover::Item;
use crate::sliders::{BrightnessItem, VolumeItem};
use crate::toggles::{self, Toggle};

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
    brightness: Option<BrightnessItem>,
    night_light: Option<Toggle>,
    keep_awake: Option<Toggle>,
    volume: Option<VolumeItem>,
    vpn: Option<VpnItem>,
    bluetooth: Option<BluetoothItem>,
    network: Option<NetworkItem>,
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
    pub network: UnboundedSender<network::Request>,
    pub bluetooth: UnboundedSender<bluetooth::Request>,
    pub night_light: UnboundedSender<bool>,
    pub keep_awake: UnboundedSender<bool>,
    /// The shell's one password dialog.
    pub dialog: Rc<Dialog>,
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
        // Its edge as a class too, for its popovers' style.
        let (edge, class) = match init.config.position {
            Position::Top => (Edge::Top, "top"),
            Position::Bottom => (Edge::Bottom, "bottom"),
        };
        root.add_css_class(class);
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
            brightness: (config.brightness)
                .then(|| BrightnessItem::new(init.brightness.clone(), &init.blur)),
            night_light: (config.night_light).then(|| {
                let sent = init.night_light.clone();
                Toggle::new(toggles::NIGHT_LIGHT, "Night light", move |on| {
                    let _ = sent.send(on);
                })
            }),
            keep_awake: (config.keep_awake).then(|| {
                let sent = init.keep_awake.clone();
                Toggle::new(toggles::KEEP_AWAKE, "Keep awake", move |on| {
                    let _ = sent.send(on);
                })
            }),
            volume: (config.volume).then(|| VolumeItem::new(init.audio.clone(), &init.blur)),
            vpn: (config.vpn).then(|| VpnItem::new(init.network.clone(), &init.blur)),
            bluetooth: (config.bluetooth)
                .then(|| BluetoothItem::new(init.bluetooth.clone(), &init.blur)),
            network: (config.network)
                .then(|| NetworkItem::new(init.network.clone(), init.dialog.clone(), &init.blur)),
            battery: (config.battery).then(|| {
                let profiles = config.power_profile.then(|| init.profiles.clone());
                BatteryItem::new(config.battery_percentage, profiles, &init.blur)
            }),
        };
        let widgets = view_output!();
        let item = |item: Option<&Item>| item.map(|i| i.button.clone().upcast::<gtk::Widget>());
        let toggle = |toggle: &Option<Toggle>| toggle.as_ref().map(|t| t.button.clone().upcast());
        // Before the clock, in this order.
        let buttons = [
            item(model.layout.as_ref().map(|i| &i.item)),
            item(model.brightness.as_ref().map(|i| &i.item)),
            toggle(&model.night_light),
            toggle(&model.keep_awake),
            item(model.volume.as_ref().map(|i| &i.item)),
            item(model.vpn.as_ref().map(|i| &i.item)),
            item(model.bluetooth.as_ref().map(|i| &i.item)),
            item(model.network.as_ref().map(|i| &i.item)),
            item(model.battery.as_ref().map(|i| &i.item)),
        ];
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
                if let Some(item) = &self.battery {
                    item.show_profiles(profiles.as_ref());
                }
            }
            Msg::Update(Update::Network(network)) => {
                if let Some(item) = &self.network {
                    item.show(network.as_ref());
                }
                if let Some(item) = &self.vpn {
                    item.show(network.as_ref());
                }
            }
            Msg::Update(Update::NightLight(on)) => {
                if let Some(toggle) = &self.night_light {
                    toggle.show(on);
                }
            }
            Msg::Update(Update::KeepAwake(on)) => {
                if let Some(toggle) = &self.keep_awake {
                    toggle.show(Some(on));
                }
            }
            Msg::Update(Update::Bluetooth(bluetooth)) => {
                if let Some(item) = &self.bluetooth {
                    item.show(bluetooth.as_ref());
                }
            }
            // The dialog's, which the shell passes to it.
            Msg::Update(Update::Joined(_) | Update::Prompt(_) | Update::PromptClosed(_)) => {}
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
