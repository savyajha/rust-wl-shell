//! One output's bar: its workspaces and the focused app on the left, the clock on the right.

use gtk4::{self as gtk, gdk, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use relm4::{Component, ComponentParts, ComponentSender};
use shell_core::Command;
use shell_core::config::Position;
use shell_core::niri::Workspace;
use tokio::sync::mpsc::UnboundedSender;

pub struct Bar {
    /// Its connector is the output's name, as niri names outputs.
    monitor: gdk::Monitor,
    commands: UnboundedSender<Command>,
    /// Every output's workspaces, and this output's, as drawn.
    all: Vec<Workspace>,
    workspaces: Vec<Workspace>,
    app: String,
    clock: String,
}

pub struct Init {
    pub monitor: gdk::Monitor,
    pub position: Position,
    pub show_workspaces: bool,
    pub show_app: bool,
    pub commands: UnboundedSender<Command>,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Niri {
        /// Every output's.
        workspaces: Vec<Workspace>,
        /// The focused window's app name.
        app: String,
    },
    Clock(String),
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
                        set_visible: init.show_workspaces,
                    },
                    gtk::Label {
                        add_css_class: "app",
                        set_visible: init.show_app,
                        #[watch]
                        set_label: &model.app,
                    },
                },
                #[wrap(Some)]
                set_end_widget = &gtk::Label {
                    add_css_class: "clock",
                    #[watch]
                    set_label: &model.clock,
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
        let edge = match init.position {
            Position::Top => Edge::Top,
            Position::Bottom => Edge::Bottom,
        };
        for edge in [edge, Edge::Left, Edge::Right] {
            root.set_anchor(edge, true);
        }
        root.auto_exclusive_zone_enable();

        init.monitor
            .connect_connector_notify(move |_| sender.input(Msg::Connector));
        let model = Bar {
            monitor: init.monitor.clone(),
            commands: init.commands.clone(),
            all: Vec::new(),
            workspaces: Vec::new(),
            app: String::new(),
            clock: String::new(),
        };
        let widgets = view_output!();
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
            Msg::Niri { workspaces, app } => {
                self.all = workspaces;
                self.app = app;
            }
            Msg::Clock(clock) => self.clock = clock,
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
            let (commands, id) = (self.commands.clone(), ws.id);
            button.connect_clicked(move |_| {
                // Fails only once the core has stopped, and the shell with it.
                let _ = commands.send(Command::FocusWorkspace(id));
            });
            row.append(&button);
        }
    }
}
