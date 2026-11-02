//! The look: the built-in stylesheet (`style.css`, then `adwaita.css`, the dialog's
//! rules ported from libadwaita), after the variables the config sets, over the fallbacks
//! for the named colours it uses (`fallback.css`, below the GTK theme), and the user's own, `$XDG_CONFIG_HOME/rust-wl-shell/style.css`, over it and loaded again
//! whenever it changes.

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::{self as gtk, gdk, gio, glib, prelude::*};
use shell_core::config::Config;
use tracing::{info, warn};

/// Load the fallbacks, the built-in stylesheet, then the user's. The monitor returned
/// follows the user's file while it is kept.
pub fn load(display: &gdk::Display, config: &Config) -> Option<gio::FileMonitor> {
    let fallback = gtk::CssProvider::new();
    fallback.load_from_string(include_str!("fallback.css"));
    let priority = gtk::STYLE_PROVIDER_PRIORITY_FALLBACK;
    gtk::style_context_add_provider_for_display(display, &fallback, priority);
    let builtin = gtk::CssProvider::new();
    builtin.load_from_string(&stylesheet(config));
    let priority = gtk::STYLE_PROVIDER_PRIORITY_APPLICATION;
    gtk::style_context_add_provider_for_display(display, &builtin, priority);

    let path = glib::user_config_dir().join("rust-wl-shell/style.css");
    let user = gtk::CssProvider::new();
    let failed = Rc::new(Cell::new(false));
    let (shown, seen) = (path.display().to_string(), failed.clone());
    user.connect_parsing_error(move |_, section, error| {
        warn!("{shown}: {}: {error}", section.to_str());
        // A warning (an empty declaration, say) leaves the rest of the file applied.
        if error.kind::<gtk::CssParserWarning>().is_none() {
            seen.set(true);
        }
    });
    load_user(&user, &path, &failed);
    let priority = gtk::STYLE_PROVIDER_PRIORITY_USER;
    gtk::style_context_add_provider_for_display(display, &user, priority);
    // A monitor on the file is one on its directory, for its name: it sees the file
    // written, and replaced (home-manager's symlink, renamed over it).
    let file = gio::File::for_path(&path);
    let flags = gio::FileMonitorFlags::WATCH_MOVES;
    let monitor = file.monitor_file(flags, gio::Cancellable::NONE);
    let monitor = monitor.inspect_err(|e| warn!("not following {}: {e}", path.display()));
    let monitor = monitor.ok()?;
    monitor.connect_changed(move |_, _, _, event| {
        // A write's end comes as its own event after the changes.
        if event != gio::FileMonitorEvent::Changed {
            load_user(&user, &path, &failed);
        }
    });
    Some(monitor)
}

/// The user's stylesheet, or none: a missing file, or one GTK cannot parse in full (its
/// errors logged), leaves the built-in look. Warnings are logged only.
fn load_user(user: &gtk::CssProvider, path: &PathBuf, failed: &Cell<bool>) {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            warn!("reading {}: {e}", path.display());
            String::new()
        }
    };
    failed.set(false);
    user.load_from_string(&text);
    if failed.get() {
        warn!("{} has errors: the built-in look stays", path.display());
        user.load_from_string("");
    } else if !text.is_empty() {
        info!("style from {}", path.display());
    }
}

/// The built-in stylesheet, after the variables it takes from the config.
fn stylesheet(config: &Config) -> String {
    let sizes = config.sizes();
    let (height, font, icon, spacing) = (sizes.height, sizes.font, sizes.icon, sizes.spacing);
    let (popover, panel) = (config.popover_radius, config.panel_radius);
    format!(
        "window {{ --height: {height}px; --font-size: {font}px; --icon-size: {icon}px; \
         --spacing: {spacing}px; --popover-radius: {popover}px; --panel-radius: {panel}px; }}\n{}{}",
        include_str!("style.css"),
        include_str!("adwaita.css")
    )
}
