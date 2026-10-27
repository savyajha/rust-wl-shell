//! The shell's core: its services and their state, with no GTK. It runs on a tokio runtime
//! on its own thread, sends each service's state to the UI as a whole snapshot, and takes
//! the UI's commands.

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
}

/// The core's side of the channels.
pub struct Core {
    /// Closes when the core stops, which it does only when niri's IPC fails.
    pub updates: UnboundedReceiver<Update>,
    pub niri: UnboundedSender<niri::Command>,
    /// The power profile to set: one of `power_profiles::PROFILES`.
    pub profiles: UnboundedSender<String>,
}

/// Start the core's thread.
pub fn start(clock_format: String) -> Core {
    let (update_tx, updates) = mpsc::unbounded_channel();
    let (niri, niri_rx) = mpsc::unbounded_channel();
    let (profiles, profile_rx) = mpsc::unbounded_channel();
    thread::Builder::new()
        .name("core".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("starting tokio");
            // Dropping the runtime ends the other services too, which closes `updates`.
            runtime.block_on(run(clock_format, update_tx, niri_rx, profile_rx));
        })
        .expect("starting the core's thread");
    Core {
        updates,
        niri,
        profiles,
    }
}

async fn run(
    clock_format: String,
    updates: UnboundedSender<Update>,
    niri_rx: UnboundedReceiver<niri::Command>,
    profile_rx: UnboundedReceiver<String>,
) {
    let (bus_tx, bus_rx) = oneshot::channel();
    tokio::spawn(clock::run(clock_format, updates.clone(), bus_rx));
    tokio::spawn(system_services(updates.clone(), bus_tx, profile_rx));
    if let Err(e) = niri::run(updates, niri_rx).await {
        error!("{e:#}");
    }
}

/// The services on the system bus, sharing one connection, which the clock gets too.
async fn system_services(
    updates: UnboundedSender<Update>,
    clock_bus: oneshot::Sender<Option<zbus::Connection>>,
    profile_requests: UnboundedReceiver<String>,
) {
    let bus = bus::system().await;
    let _ = clock_bus.send(bus.clone());
    let Some(bus) = bus else {
        return;
    };
    tokio::spawn(upower::run(bus.clone(), updates.clone()));
    power_profiles::run(bus, updates, profile_requests).await;
}
