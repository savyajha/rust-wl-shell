//! rust-wl-shell: a macOS-style top bar for niri, one per output.
//!
//! `--config <path>` runs it (without, with the defaults); `--check <path>` validates a
//! config. The core (`shell_core`) runs on its own thread; this is the GTK side.

mod bar;
mod bluetooth;
mod blur;
mod clock;
mod dialog;
mod items;
mod network;
mod notifications;
mod osd;
mod popover;
mod power;
mod shell;
mod sliders;
mod toggles;

use std::env;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, bail};
use gtk4::{self as gtk, gdk, glib};
use shell_core::config::{Config, Sizes};
use tracing::{error, info};
use tracing_subscriber::filter::LevelFilter;

#[derive(Debug, PartialEq)]
enum Mode {
    Run(Option<PathBuf>),
    Check(PathBuf),
}

fn parse_args(mut args: impl Iterator<Item = OsString>) -> Option<Mode> {
    match (args.next(), args.next(), args.next()) {
        (None, None, None) => Some(Mode::Run(None)),
        (Some(flag), Some(path), None) if flag == "--config" => Some(Mode::Run(Some(path.into()))),
        (Some(flag), Some(path), None) if flag == "--check" => Some(Mode::Check(path.into())),
        _ => None,
    }
}

/// Start-up times, from `main`. Before it, loading GTK's libraries takes about 21 ms warm
/// on AC and about 60 ms on battery, for any GTK binary here.
#[derive(Clone, Copy)]
pub struct Marks(Instant);

impl Marks {
    pub fn log(&self, what: &str) {
        let ms = self.0.elapsed().as_secs_f64() * 1e3;
        info!("+{ms:.1} ms: {what}");
    }
}

fn main() -> ExitCode {
    let marks = Marks(Instant::now());
    let Some(mode) = parse_args(env::args_os().skip(1)) else {
        eprintln!("usage: rust-wl-shell [--config <path> | --check <path>]");
        return ExitCode::from(2);
    };
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_writer(io::stderr)
        .init();
    let result = match mode {
        Mode::Run(path) => run(marks, path),
        Mode::Check(path) => Config::load(&path).map(|_| info!("{} is valid", path.display())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(marks: Marks, path: Option<PathBuf>) -> anyhow::Result<()> {
    let config = match path {
        Some(path) => Config::load(&path)?,
        None => Config::default(),
    };
    // The name AT-SPI shows, rather than the Nix wrapper's.
    glib::set_prgname(Some("rust-wl-shell"));
    init_gtk()?;
    marks.log("gtk_init");
    let display = gdk::Display::default().context("no display")?;
    let css = gtk::CssProvider::new();
    css.load_from_string(&stylesheet(config.sizes()));
    gtk::style_context_add_provider_for_display(
        &display,
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    // The shell's own icons (keep awake's cup), installed by Nix, else in the source tree.
    let icons = option_env!("RUST_WL_SHELL_ICONS");
    let icons = icons.unwrap_or(concat!(env!("CARGO_MANIFEST_DIR"), "/icons"));
    gtk::IconTheme::for_display(&display).add_search_path(icons);

    let core = shell_core::start(&config);
    let main_loop = glib::MainLoop::new(None, false);
    shell::start(&display, config, core, marks, main_loop.clone());
    main_loop.run();
    bail!("the core stopped");
}

/// GTK's start-up, without waiting for xdg-desktop-portal (GTK then reads its settings
/// from GSettings) and on its default renderer. `GDK_DEBUG` is restored right after, so
/// the apps the shell starts don't inherit it; but `GSK_RENDERER` stays removed for them,
/// and the wrapper's `XDG_DATA_DIRS`, `GIO_EXTRA_MODULES` and `GDK_PIXBUF_MODULE_FILE`
/// reach them too. The shell starts nothing yet; when it does, it sets each child's
/// environment.
fn init_gtk() -> anyhow::Result<()> {
    let debug = env::var_os("GDK_DEBUG");
    let mut no_portals = debug.clone().unwrap_or_default();
    if !no_portals.is_empty() {
        no_portals.push(",");
    }
    no_portals.push("no-portals");
    // SAFETY: the process has one thread so far: no Rust thread exists yet, and GTK starts
    // its own only in `gtk::init`. So `shell_core::start` must stay after `init_gtk`.
    unsafe {
        env::remove_var("GSK_RENDERER");
        env::set_var("GDK_DEBUG", &no_portals);
    }
    let init = gtk::init();
    // SAFETY: GTK has started a thread by now, which may read the environment, but glibc
    // changes only GDK_DEBUG's own entry: set while unset, it went last, and removing the
    // last entry moves no other; restoring a value swaps one pointer and frees nothing.
    unsafe {
        match &debug {
            Some(debug) => env::set_var("GDK_DEBUG", debug),
            None => env::remove_var("GDK_DEBUG"),
        }
    }
    init.context("starting GTK")
}

/// The fixed stylesheet, after the variables it takes its sizes from.
fn stylesheet(sizes: Sizes) -> String {
    let Sizes {
        height,
        font,
        icon,
        spacing,
        dot,
    } = sizes;
    format!(
        "window.bar, window.osd {{ --height: {height}px; --font-size: {font}px; \
         --icon-size: {icon}px; --spacing: {spacing}px; --dot-size: {dot}px; }}\n{}",
        include_str!("style.css")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn parse(args: &[&str]) -> Option<Mode> {
        parse_args(args.iter().map(OsStr::new).map(OsString::from))
    }

    #[test]
    fn arguments() {
        assert_eq!(parse(&[]), Some(Mode::Run(None)));
        assert_eq!(parse(&["--config", "a"]), Some(Mode::Run(Some("a".into()))));
        assert_eq!(parse(&["--check", "a"]), Some(Mode::Check("a".into())));
        assert_eq!(parse(&["--check"]), None);
        assert_eq!(parse(&["--config", "a", "b"]), None);
        assert_eq!(parse(&["a"]), None);
    }
}
