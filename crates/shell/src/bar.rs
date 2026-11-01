//! One output's bar: its workspaces and the focused app on the left; the keyboard layout,
//! the brightness, the volume, VPN, Bluetooth, the network, the battery (with the power
//! mode) and the clock on the right.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::{self as gtk, gdk, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::{Component, ComponentParts, ComponentSender};
use shell_core::config::{Config, Position};
use shell_core::niri::Workspace;
use shell_core::{Senders, Update};

use crate::bluetooth::BluetoothItem;
use crate::clock::ClockItem;
use crate::dialog::Dialog;
use crate::items::{BatteryItem, LayoutItem};
use crate::network::{NetworkItem, VpnItem};
use crate::notifications::Banners;
use crate::popover::Item;
use crate::power::PowerItem;
use crate::sliders::{BrightnessItem, VolumeItem};
use crate::toggles::{self, Toggle};
use crate::workspaces::Workspaces;

pub struct Bar {
    /// Its connector is the output's name, as niri names outputs.
    monitor: gdk::Monitor,
    /// Every output's workspaces, and this output's shown, as drawn.
    all: Vec<Workspace>,
    workspaces: Vec<Workspace>,
    switcher: Workspaces,
    app: String,
    /// The clock, with the notifications' history in its popover.
    clock: ClockItem,
    /// After the clock.
    power: Option<PowerItem>,
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
    pub senders: Senders,
    /// The shell's banners, hidden while the clock's popover is open.
    pub banners: Rc<RefCell<Banners>>,
    /// The shell's one password dialog.
    pub dialog: Rc<Dialog>,
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
                    #[local_ref]
                    switcher -> gtk::Box {
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
            all: Vec::new(),
            workspaces: Vec::new(),
            switcher: Workspaces::new(init.senders.niri.clone(), config),
            app: String::new(),
            power: (config.power).then(|| PowerItem::new(init.senders.power.clone())),
            clock: ClockItem::new(
                config,
                config
                    .notifications
                    .then(|| init.senders.notifications.clone()),
                init.banners.clone(),
            ),
            layout: (config.keyboard_layout).then(|| LayoutItem::new(init.senders.niri.clone())),
            brightness: (config.brightness)
                .then(|| BrightnessItem::new(init.senders.brightness.clone())),
            night_light: (config.night_light).then(|| {
                let sent = init.senders.night_light.clone();
                Toggle::new(toggles::NIGHT_LIGHT, "Night light", move |on| {
                    let _ = sent.send(on);
                })
            }),
            keep_awake: (config.keep_awake).then(|| {
                let sent = init.senders.keep_awake.clone();
                Toggle::new(toggles::KEEP_AWAKE, "Keep awake", move |on| {
                    let _ = sent.send(on);
                })
            }),
            volume: (config.volume).then(|| VolumeItem::new(init.senders.audio.clone())),
            vpn: (config.vpn).then(|| VpnItem::new(init.senders.network.clone())),
            bluetooth: (config.bluetooth)
                .then(|| BluetoothItem::new(init.senders.bluetooth.clone())),
            network: (config.network)
                .then(|| NetworkItem::new(init.senders.network.clone(), init.dialog.clone())),
            battery: (config.battery).then(|| {
                let profiles = config.power_profile.then(|| init.senders.profiles.clone());
                BatteryItem::new(config.battery_percentage, profiles)
            }),
        };
        let switcher = &model.switcher.row;
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
            widgets.right.append(&button);
        }
        widgets.right.append(&model.clock.item.button);
        if let Some(power) = &model.power {
            widgets.right.append(&power.item.button);
        }
        // Each popover opens away from the bar's edge, `popover-gap` from it (GTK's offset
        // is not flipped with it).
        let gap = config.popover_gap as i32;
        let (direction, offset) = match config.position {
            Position::Top => (gtk::ArrowType::Down, gap),
            Position::Bottom => (gtk::ArrowType::Up, -gap),
        };
        let mut child = widgets.right.first_child();
        while let Some(widget) = child {
            if let Some(button) = widget.downcast_ref::<gtk::MenuButton>() {
                button.set_direction(direction);
                if let Some(popover) = button.popover() {
                    popover.set_offset(0, offset);
                }
            }
            child = widget.next_sibling();
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
            Msg::Update(Update::Clock(clock)) => self.clock.label.set_label(&clock),
            Msg::Update(Update::Notifications(state)) => self.clock.show(&state),
            Msg::Update(Update::Power(offered)) => {
                if let Some(item) = &self.power {
                    item.show(offered.as_ref());
                }
            }
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
            Msg::Update(
                Update::Joined(_)
                | Update::Prompt(_)
                | Update::PromptClosed(_)
                | Update::Asked(..)
                | Update::Acted(..)
                | Update::Interrupted,
            ) => {}
            Msg::App(app) => self.app = app,
            Msg::Connector => {}
        }
        let connector = self.monitor.connector().map(String::from);
        // Those with windows, and the one shown (vibepanel's).
        let ours: Vec<Workspace> = self
            .all
            .iter()
            .filter(|ws| ws.output.is_some() && ws.output == connector)
            .filter(|ws| ws.occupied || ws.active)
            .cloned()
            .collect();
        if ours != self.workspaces {
            self.switcher.show(&ours);
            self.workspaces = ours;
        }
        self.update_view(widgets, sender);
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
