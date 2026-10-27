//! The bars, one per monitor as GDK lists them, and the core's updates passed to each.

use std::cell::RefCell;
use std::rc::Rc;

use gio_unix::DesktopAppInfo;
use gtk4::{gdk, gio, glib, prelude::*};
use relm4::{Component, ComponentController, Controller};
use shell_core::config::Config;
use shell_core::niri::Workspace;
use shell_core::{Command, Core, Update};
use tokio::sync::mpsc::UnboundedSender;
use tracing::info;

use crate::Marks;
use crate::bar::{self, Bar, Msg};
use crate::blur::{Blur, Effect};

struct Shell {
    config: Config,
    commands: UnboundedSender<Command>,
    blur: Option<Rc<Blur>>,
    bars: Vec<(gdk::Monitor, Controller<Bar>)>,
    /// The last of each update, for bars added later.
    workspaces: Vec<Workspace>,
    app: String,
    clock: String,
}

/// Show a bar on every monitor, now and as they come and go, until the core stops; then
/// quit `main_loop`.
pub fn start(
    display: &gdk::Display,
    config: Config,
    core: Core,
    marks: Marks,
    main_loop: glib::MainLoop,
) {
    let Core {
        mut updates,
        commands,
    } = core;
    let shell = Rc::new(RefCell::new(Shell {
        config,
        commands,
        blur: Blur::new(display).map(Rc::new),
        bars: Vec::new(),
        workspaces: Vec::new(),
        app: String::new(),
        clock: String::new(),
    }));
    let monitors = display.monitors();
    shell.borrow_mut().sync(&monitors);
    if let Some((_, bar)) = shell.borrow().bars.first() {
        log_first_frame(bar.widget(), marks);
    }
    monitors.connect_items_changed(glib::clone!(
        #[weak]
        shell,
        move |monitors, _, _, _| shell.borrow_mut().sync(monitors)
    ));
    glib::spawn_future_local(async move {
        while let Some(update) = updates.recv().await {
            shell.borrow_mut().update(update);
        }
        main_loop.quit();
    });
}

impl Shell {
    fn sync(&mut self, monitors: &gio::ListModel) {
        let monitors: Vec<gdk::Monitor> = monitors.iter().flatten().collect();
        self.bars.retain(|(monitor, bar)| {
            let kept = monitors.contains(monitor);
            if !kept {
                info!("output {} removed", name(monitor));
                bar.widget().destroy();
            }
            kept
        });
        for monitor in monitors {
            if !self.bars.iter().any(|(m, _)| *m == monitor) {
                info!("output {} added", name(&monitor));
                let bar = self.launch(&monitor);
                self.bars.push((monitor, bar));
            }
        }
    }

    fn launch(&self, monitor: &gdk::Monitor) -> Controller<Bar> {
        let bar = Bar::builder()
            .launch(bar::Init {
                monitor: monitor.clone(),
                position: self.config.position,
                show_workspaces: self.config.workspaces,
                show_app: self.config.app_name,
                commands: self.commands.clone(),
            })
            .detach();
        let window = bar.widget();
        let effect: Rc<RefCell<Option<Effect>>> = Rc::default();
        let blur = self.blur.clone();
        window.connect_realize(glib::clone!(
            #[strong]
            effect,
            move |window| {
                *effect.borrow_mut() = blur.as_ref().and_then(|blur| blur.apply(window));
            }
        ));
        window.connect_unrealize(move |_| {
            if let Some(effect) = effect.take() {
                effect.destroy();
            }
        });
        bar.emit(Msg::Niri {
            workspaces: self.workspaces.clone(),
            app: self.app.clone(),
        });
        bar.emit(Msg::Clock(self.clock.clone()));
        window.present();
        bar
    }

    fn update(&mut self, update: Update) {
        let msg = match update {
            Update::Niri(snapshot) => {
                self.workspaces = snapshot.workspaces;
                self.app = snapshot.app_id.as_deref().map(app_name).unwrap_or_default();
                Msg::Niri {
                    workspaces: self.workspaces.clone(),
                    app: self.app.clone(),
                }
            }
            Update::Clock(text) => {
                self.clock = text;
                Msg::Clock(self.clock.clone())
            }
        };
        for (_, bar) in &self.bars {
            bar.emit(msg.clone());
        }
    }
}

/// Log the first frame `window` paints; it is realized, as `present` realizes it.
fn log_first_frame(window: &gtk4::Window, marks: Marks) {
    let Some(clock) = window.frame_clock() else {
        return;
    };
    let handler = Rc::new(RefCell::new(None));
    let id = clock.connect_after_paint(glib::clone!(
        #[strong]
        handler,
        move |clock| {
            marks.log("first frame");
            if let Some(id) = handler.take() {
                clock.disconnect(id);
            }
        }
    ));
    *handler.borrow_mut() = Some(id);
}

/// The name in the app's desktop file (by its app id, as is, then in lower case), else the
/// app id.
fn app_name(app_id: &str) -> String {
    [app_id.to_owned(), app_id.to_lowercase()]
        .iter()
        .find_map(|id| DesktopAppInfo::new(&format!("{id}.desktop")))
        .map_or_else(|| app_id.to_owned(), |info| info.name().into())
}

fn name(monitor: &gdk::Monitor) -> String {
    let name = monitor.connector().map(String::from);
    name.unwrap_or_else(|| "(not named yet)".into())
}
