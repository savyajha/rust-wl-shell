//! The bars, one per monitor as GDK lists them, and the core's updates passed to each.

use std::cell::RefCell;
use std::mem;
use std::rc::Rc;

use gio_unix::DesktopAppInfo;
use gtk4::{gdk, gio, glib, prelude::*};
use relm4::{Component, ComponentController, Controller};
use shell_core::config::Config;
use shell_core::{Core, Update, niri};
use tokio::sync::mpsc::UnboundedSender;
use tracing::info;

use crate::Marks;
use crate::bar::{self, Bar, Msg};
use crate::blur::Blur;

struct Shell {
    config: Rc<Config>,
    niri: UnboundedSender<niri::Command>,
    profiles: UnboundedSender<String>,
    blur: Option<Rc<Blur>>,
    bars: Vec<(gdk::Monitor, Controller<Bar>)>,
    /// The last update of each kind, for bars added later.
    latest: Vec<Update>,
    /// The focused window's app, by name, from the latest niri update.
    app: String,
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
        niri,
        profiles,
    } = core;
    let shell = Rc::new(RefCell::new(Shell {
        config: Rc::new(config),
        niri,
        profiles,
        blur: Blur::new(display).map(Rc::new),
        bars: Vec::new(),
        latest: Vec::new(),
        app: String::new(),
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
                config: self.config.clone(),
                niri: self.niri.clone(),
                profiles: self.profiles.clone(),
                blur: self.blur.clone(),
            })
            .detach();
        Blur::attach(&self.blur, bar.widget());
        for update in &self.latest {
            bar.emit(Msg::Update(update.clone()));
        }
        bar.emit(Msg::App(self.app.clone()));
        bar.widget().present();
        bar
    }

    fn update(&mut self, update: Update) {
        if let Update::Niri(snapshot) = &update {
            let app = snapshot.app_id.as_deref().map(app_name).unwrap_or_default();
            if app != self.app {
                self.app = app;
                for (_, bar) in &self.bars {
                    bar.emit(Msg::App(self.app.clone()));
                }
            }
        }
        for (_, bar) in &self.bars {
            bar.emit(Msg::Update(update.clone()));
        }
        let kind = mem::discriminant(&update);
        self.latest
            .retain(|latest| mem::discriminant(latest) != kind);
        self.latest.push(update);
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

fn name(monitor: &gdk::Monitor) -> String {
    let name = monitor.connector().map(String::from);
    name.unwrap_or_else(|| "(not named yet)".into())
}

/// The name in the app's desktop file (by its app id, as is, then in lower case), else the
/// app id.
fn app_name(app_id: &str) -> String {
    [app_id.to_owned(), app_id.to_lowercase()]
        .iter()
        .find_map(|id| DesktopAppInfo::new(&format!("{id}.desktop")))
        .map_or_else(|| app_id.to_owned(), |info| info.name().into())
}
