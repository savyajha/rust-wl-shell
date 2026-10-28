//! The shell's core: its services and their state, with no GTK. It runs on a tokio runtime
//! on its own thread, sends each service's state to the UI as a whole snapshot, and takes
//! the UI's commands.

pub mod audio;
pub mod backlight;
mod bus;
pub mod clock;
pub mod config;
pub mod niri;
pub mod power_profiles;
pub mod upower;
pub mod xkb;

use std::thread;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tracing::error;

#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Niri(niri::Snapshot),
    /// The clock's text.
    Clock(String),
    /// `None`: no battery, or no UPower.
    Battery(Option<upower::Battery>),
    /// `None`: no power-profiles-daemon.
    Profiles(Option<power_profiles::Profiles>),
    /// `None`: no audio server, or no default output.
    Audio(Option<audio::Audio>),
    /// The backlight's level in percent; `None`: no backlight.
    Backlight(Option<u8>),
}

/// The core's side of the channels.
pub struct Core {
    /// Closes when the core stops, which it does only when niri's IPC fails.
    pub updates: UnboundedReceiver<Update>,
    pub niri: UnboundedSender<niri::Command>,
    /// The power profile to set: one of `power_profiles::PROFILES`.
    pub profiles: UnboundedSender<String>,
    pub audio: audio::Requests,
    /// The backlight's level to set, in percent.
    pub brightness: UnboundedSender<u8>,
}

/// Start the core's thread, and the audio's; `backlight` names the backlight to follow,
/// else the first.
pub fn start(clock_format: String, backlight: Option<String>) -> Core {
    let (update_tx, updates) = mpsc::unbounded_channel();
    let (niri, niri_rx) = mpsc::unbounded_channel();
    let (profiles, profile_rx) = mpsc::unbounded_channel();
    let (brightness, brightness_rx) = mpsc::unbounded_channel();
    let audio = audio::start(update_tx.downgrade());
    thread::Builder::new()
        .name("core".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("starting tokio");
            // Dropping the runtime ends the other services too, which closes `updates`.
            runtime.block_on(async move {
                let (bus_tx, bus_rx) = oneshot::channel();
                tokio::spawn(clock::run(clock_format, update_tx.clone(), bus_rx));
                let updates = update_tx.clone();
                // The services on the system bus share one connection, which the clock gets
                // too. The backlight is read without it, but set through it.
                tokio::spawn(async move {
                    let bus = bus::system().await;
                    let _ = bus_tx.send(bus.clone());
                    let followed =
                        backlight::run(backlight, bus.clone(), updates.clone(), brightness_rx);
                    tokio::spawn(followed);
                    let Some(bus) = bus else {
                        return;
                    };
                    tokio::spawn(upower::run(bus.clone(), updates.clone()));
                    power_profiles::run(bus, updates, profile_rx).await;
                });
                if let Err(e) = niri::run(update_tx, niri_rx).await {
                    error!("{e:#}");
                }
            });
        })
        .expect("starting the core's thread");
    Core {
        updates,
        niri,
        profiles,
        audio,
        brightness,
    }
}
