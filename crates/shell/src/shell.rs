//! The bars, one per monitor as GDK lists them, and the core's updates passed to each.

use std::cell::RefCell;
use std::mem;
use std::rc::Rc;

use gio_unix::DesktopAppInfo;
use gtk4::{gdk, gio, glib, prelude::*};
use relm4::{Component, ComponentController, Controller};
use shell_core::config::Config;
use shell_core::{Core, Update, audio, bluetooth, network, niri};
use tokio::sync::mpsc::UnboundedSender;
use tracing::info;

use crate::Marks;
use crate::bar::{self, Bar, Msg};
use crate::blur::Blur;
use crate::dialog::Dialog;
use crate::osd::Osd;
use crate::sliders::{self, BRIGHTNESS};

struct Shell {
    config: Rc<Config>,
    niri: UnboundedSender<niri::Command>,
    profiles: UnboundedSender<String>,
    audio: audio::Requests,
    brightness: UnboundedSender<u8>,
    network: UnboundedSender<network::Request>,
    bluetooth: UnboundedSender<bluetooth::Request>,
    night_light: UnboundedSender<bool>,
    keep_awake: UnboundedSender<bool>,
    dialog: Rc<Dialog>,
    osd: Option<Osd>,
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
        audio,
        brightness,
        network,
        answers,
        bluetooth,
        night_light,
        keep_awake,
    } = core;
    let blur = Blur::new(display).map(Rc::new);
    let shell = Rc::new(RefCell::new(Shell {
        niri,
        profiles,
        audio,
        brightness,
        dialog: Dialog::new(network.clone(), answers, &blur),
        network,
        bluetooth,
        night_light,
        keep_awake,
        osd: config.osd.then(|| Osd::new(&blur)),
        config: Rc::new(config),
        blur,
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
                audio: self.audio.clone(),
                brightness: self.brightness.clone(),
                network: self.network.clone(),
                bluetooth: self.bluetooth.clone(),
                night_light: self.night_light.clone(),
                keep_awake: self.keep_awake.clone(),
                dialog: self.dialog.clone(),
                blur: self.blur.clone(),
            })
            .detach();
        Blur::attach(&self.blur, bar.widget(), 0);
        for update in &self.latest {
            bar.emit(Msg::Update(update.clone()));
        }
        bar.emit(Msg::App(self.app.clone()));
        bar.widget().present();
        bar
    }

    fn update(&mut self, update: Update) {
        // The dialog's alone, not kept for bars to come.
        match update {
            Update::Joined(joined) => return self.dialog.joined(&joined),
            Update::Prompt(prompt) => return self.dialog.ask(prompt),
            Update::PromptClosed(id) => return self.dialog.closed(id),
            _ => {}
        }
        if let Update::Niri(snapshot) = &update {
            self.dialog.focus(snapshot.focused_output.clone());
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
        self.show_osd(&update);
        let kind = mem::discriminant(&update);
        self.latest
            .retain(|latest| mem::discriminant(latest) != kind);
        self.latest.push(update);
    }

    /// The OSD, for a change of volume, mute or brightness from outside the shell: not the
    /// first value (at start-up, or when the service comes back), not a switch of output,
    /// and not while that item's popover is open on a bar, where the user moves its slider.
    fn show_osd(&self, update: &Update) {
        let Some(osd) = &self.osd else {
            return;
        };
        let before = self
            .latest
            .iter()
            .find(|latest| mem::discriminant(*latest) == mem::discriminant(update));
        let open = self.bars.iter().map(|(_, bar)| bar.model().sliders_open());
        let (volume_open, brightness_open) =
            open.fold((false, false), |a, b| (a.0 || b.0, a.1 || b.1));
        let (icon, level) = match (before, update) {
            // A switch of output (here or elsewhere) is not a change of volume.
            (Some(Update::Audio(Some(before))), Update::Audio(Some(audio)))
                if !volume_open
                    && before.default == audio.default
                    && (before.volume, before.muted) != (audio.volume, audio.muted) =>
            {
                (sliders::volume_icon(audio), audio.volume)
            }
            (Some(Update::Backlight(Some(before))), Update::Backlight(Some(level)))
                if !brightness_open && before != level =>
            {
                (BRIGHTNESS, u32::from(*level))
            }
            _ => return,
        };
        let focused = self.latest.iter().find_map(|latest| match latest {
            Update::Niri(snapshot) => snapshot.focused_output.clone(),
            _ => None,
        });
        let monitor = self
            .bars
            .iter()
            .map(|(monitor, _)| monitor)
            .find(|monitor| focused.is_some() && monitor.connector().map(String::from) == focused);
        osd.show(icon, level, monitor);
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
