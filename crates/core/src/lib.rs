//! The shell's core: its services and their state, with no GTK. It runs on a tokio runtime
//! on its own thread, sends each service's state to the UI as a whole snapshot, and takes
//! the UI's commands.

pub mod agent;
pub mod audio;
pub mod backlight;
pub mod bluetooth;
mod bus;
pub mod clock;
pub mod config;
pub mod network;
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
    /// `None`: no NetworkManager.
    Network(Option<network::Network>),
    /// A join the UI asked for has ended.
    Joined(network::Joined),
    /// NetworkManager asks for a secret, through the agent.
    Prompt(agent::Prompt),
    /// NetworkManager cancelled that prompt.
    PromptClosed(u64),
    /// `None`: no BlueZ, or no adapter.
    Bluetooth(Option<bluetooth::Bluetooth>),
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
    pub network: UnboundedSender<network::Request>,
    pub answers: UnboundedSender<agent::Answer>,
    pub bluetooth: UnboundedSender<bluetooth::Request>,
}

/// Start the core's thread, and the audio's, with the services the config's items need
/// (the others take their channels' requests nowhere).
pub fn start(config: &config::Config) -> Core {
    let (clock_format, backlight) = (config.clock_format.clone(), config.backlight.clone());
    let network_on = config.network || config.vpn;
    let agent_on = network_on && config.secret_agent;
    let bluetooth_on = config.bluetooth;
    let (update_tx, updates) = mpsc::unbounded_channel();
    let (niri, niri_rx) = mpsc::unbounded_channel();
    let (profiles, profile_rx) = mpsc::unbounded_channel();
    let (brightness, brightness_rx) = mpsc::unbounded_channel();
    let (network, network_rx) = mpsc::unbounded_channel();
    let (answers, answer_rx) = mpsc::unbounded_channel();
    let (bluetooth, bluetooth_rx) = mpsc::unbounded_channel();
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
                if network_on {
                    tokio::spawn(network::run(update_tx.clone(), network_rx));
                }
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
                    if agent_on {
                        tokio::spawn(agent::run(bus.clone(), updates.clone(), answer_rx));
                    }
                    if bluetooth_on {
                        tokio::spawn(bluetooth::run(bus.clone(), updates.clone(), bluetooth_rx));
                    }
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
        network,
        answers,
        bluetooth,
    }
}
