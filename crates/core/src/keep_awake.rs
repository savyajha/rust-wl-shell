//! Keep awake: a logind inhibitor of idle (`block` mode), held while the toggle is on, so
//! that what acts on idle (the lock, the screens going off, suspend) waits; the lid and
//! the power key still sleep. Off at every start; the inhibitor goes with its fd.

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{info, warn};
use zbus::proxy;
use zbus::zvariant::OwnedFd;

use crate::Update;

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Logind {
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::Result<OwnedFd>;
}

/// Send whether the machine is kept awake (off at first), and take or release the
/// inhibitor as `requests` ask; one refused leaves it off.
pub async fn run(
    bus: zbus::Connection,
    updates: UnboundedSender<Update>,
    mut requests: UnboundedReceiver<bool>,
) {
    let mut held: Option<OwnedFd> = None;
    let _ = updates.send(Update::KeepAwake(false));
    while let Some(on) = requests.recv().await {
        if on && held.is_none() {
            let inhibit = async {
                let logind = LogindProxy::new(&bus).await?;
                logind
                    .inhibit("idle", "rust-wl-shell", "Keep awake", "block")
                    .await
            };
            match inhibit.await {
                Ok(fd) => held = Some(fd),
                Err(e) => warn!("keeping awake: {e}"),
            }
        } else if !on {
            held = None;
        }
        info!("keep awake: {}", held.is_some());
        let _ = updates.send(Update::KeepAwake(held.is_some()));
    }
}
