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
pub mod keep_awake;
pub mod network;
pub mod night_light;
pub mod niri;
pub mod notifications;
pub mod power;
pub mod power_profiles;
pub mod upower;
pub mod xkb;

use std::thread;
use std::time::Duration;

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
    /// Whether the night light's unit is active; `None`: no such unit.
    NightLight(Option<bool>),
    /// Whether the idle inhibitor is held.
    KeepAwake(bool),
    Notifications(notifications::Notifications),
    /// What logind offers; `None`: no logind.
    Power(Option<power::Power>),
    /// An action to ask about, and its block inhibitors, as "app: reason".
    Asked(power::Action, Vec<String>),
    /// A sleep starts, or the session locks: what the dialog asks is called off.
    Interrupted,
    /// An action done, or logind's error.
    Acted(power::Action, Option<String>),
}

/// The core's side of the channels.
pub struct Core {
    /// Closes when the core stops, which it does only when niri's IPC fails.
    pub updates: UnboundedReceiver<Update>,
    pub senders: Senders,
}

/// Where the shell sends the core its requests: one for each service, cloned to each bar.
#[derive(Clone)]
pub struct Senders {
    pub niri: UnboundedSender<niri::Command>,
    /// The power profile to set: one of `power_profiles::PROFILES`.
    pub profiles: UnboundedSender<String>,
    pub audio: audio::Requests,
    /// The backlight's level to set, in percent.
    pub brightness: UnboundedSender<u8>,
    pub network: UnboundedSender<network::Request>,
    pub answers: UnboundedSender<agent::Answer>,
    pub bluetooth: UnboundedSender<bluetooth::Request>,
    /// The night light (its unit) to switch on or off.
    pub night_light: UnboundedSender<bool>,
    /// Whether to keep awake.
    pub keep_awake: UnboundedSender<bool>,
    pub notifications: UnboundedSender<notifications::Request>,
    pub power: UnboundedSender<power::Request>,
}

/// Start the core's thread, and the audio's, with the services the config's items need
/// (the others take their channels' requests nowhere).
pub fn start(config: &config::Config) -> Core {
    let (clock_format, backlight) = (config.clock_format.clone(), config.backlight.clone());
    let network_on = config.network || config.vpn;
    let agent_on = network_on && config.secret_agent;
    let bluetooth_on = config.bluetooth;
    let night_light_unit = config.night_light.then(|| config.night_light_unit.clone());
    let keep_awake_on = config.keep_awake;
    let power_on = config.power;
    let notifications_cap = config
        .notifications
        .then(|| Duration::from_secs(config.notification_max_timeout.into()));
    let (update_tx, updates) = mpsc::unbounded_channel();
    let (niri, niri_rx) = mpsc::unbounded_channel();
    let (profiles, profile_rx) = mpsc::unbounded_channel();
    let (brightness, brightness_rx) = mpsc::unbounded_channel();
    let (network, network_rx) = mpsc::unbounded_channel();
    let (answers, answer_rx) = mpsc::unbounded_channel();
    let (bluetooth, bluetooth_rx) = mpsc::unbounded_channel();
    let (night_light, night_light_rx) = mpsc::unbounded_channel();
    let (keep_awake, keep_awake_rx) = mpsc::unbounded_channel();
    let (notifications, notifications_rx) = mpsc::unbounded_channel();
    let (power, power_rx) = mpsc::unbounded_channel();
    let niri_commands = niri.clone();
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
                if let Some(cap) = notifications_cap {
                    let served = notifications::run(cap, update_tx.clone(), notifications_rx);
                    tokio::spawn(served);
                }
                if let Some(unit) = night_light_unit {
                    tokio::spawn(night_light::run(unit, update_tx.clone(), night_light_rx));
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
                    // Logging out takes no system bus.
                    if power_on {
                        let served =
                            power::run(bus.clone(), updates.clone(), niri_commands, power_rx);
                        tokio::spawn(served);
                    }
                    let Some(bus) = bus else {
                        return;
                    };
                    tokio::spawn(upower::run(bus.clone(), updates.clone()));
                    if agent_on {
                        tokio::spawn(agent::run(bus.clone(), updates.clone(), answer_rx));
                    }
                    if keep_awake_on {
                        tokio::spawn(keep_awake::run(bus.clone(), updates.clone(), keep_awake_rx));
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
        senders: Senders {
            niri,
            profiles,
            audio,
            brightness,
            network,
            answers,
            bluetooth,
            night_light,
            keep_awake,
            notifications,
            power,
        },
    }
}
