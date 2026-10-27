//! The shell's core: its services and their state, with no GTK. It runs on a tokio runtime
//! on its own thread, sends each service's state to the UI as a whole snapshot, and takes
//! the UI's commands.

pub mod clock;
pub mod config;
pub mod niri;

use std::thread;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::error;

#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Niri(niri::Snapshot),
    /// The clock's text.
    Clock(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    FocusWorkspace(u64),
}

pub struct Core {
    /// Closes when the core stops, which it does only when niri's IPC fails.
    pub updates: UnboundedReceiver<Update>,
    pub commands: UnboundedSender<Command>,
}

/// Start the core's thread.
pub fn start(clock_format: String) -> Core {
    let (update_tx, updates) = mpsc::unbounded_channel();
    let (commands, command_rx) = mpsc::unbounded_channel();
    thread::Builder::new()
        .name("core".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("starting tokio");
            // Dropping the runtime ends the clock too, which closes `updates`.
            runtime.block_on(async {
                tokio::spawn(clock::run(clock_format, update_tx.clone()));
                if let Err(e) = niri::run(update_tx, command_rx).await {
                    error!("{e:#}");
                }
            });
        })
        .expect("starting the core's thread");
    Core { updates, commands }
}
