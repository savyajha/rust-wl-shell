//! The night light: a user unit (wlsunset's, by default) started and stopped through
//! systemd on the session bus, on while the unit is active. wlsunset has no IPC of its
//! own, and stopping it is what turns the night light off: niri resets the gamma when it
//! disconnects. No unit of that name: no night light.

use futures_lite::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{info, warn};
use zbus::proxy;
use zbus::zvariant::OwnedObjectPath;

use crate::Update;

#[proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Systemd {
    fn subscribe(&self) -> zbus::Result<()>;
    fn load_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait Unit {
    #[zbus(property)]
    fn active_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn load_state(&self) -> zbus::Result<String>;
}

/// Send whether the night light is on (`None`: no such unit, or no systemd) now and on
/// each change of the unit's `ActiveState`, and switch it as `requests` ask.
pub async fn run(
    unit: String,
    updates: UnboundedSender<Update>,
    requests: UnboundedReceiver<bool>,
) {
    if let Err(e) = follow(&unit, &updates, requests).await {
        info!("no night light: {e}");
        let _ = updates.send(Update::NightLight(None));
    }
}

async fn follow(
    unit: &str,
    updates: &UnboundedSender<Update>,
    mut requests: UnboundedReceiver<bool>,
) -> zbus::Result<()> {
    let bus = zbus::Connection::session().await?;
    let systemd = SystemdProxy::new(&bus).await?;
    // systemd sends units' changes only while someone has subscribed.
    systemd.subscribe().await?;
    let path = systemd.load_unit(unit).await?;
    let proxy = UnitProxy::builder(&bus).path(path)?.build().await?;
    if proxy.load_state().await? == "not-found" {
        info!("no night light: no unit {unit}");
        let _ = updates.send(Update::NightLight(None));
        return Ok(());
    }
    let mut changes = proxy.receive_active_state_changed().await;
    loop {
        let state = proxy.active_state().await?;
        let on = matches!(state.as_str(), "active" | "activating" | "reloading");
        if updates.send(Update::NightLight(Some(on))).is_err() {
            return Ok(());
        }
        tokio::select! {
            change = changes.next() => if change.is_none() { return Ok(()) },
            request = requests.recv() => {
                let Some(on) = request else { return Ok(()) };
                let switched = match on {
                    true => systemd.start_unit(unit, "replace").await,
                    false => systemd.stop_unit(unit, "replace").await,
                };
                if let Err(e) = switched {
                    warn!("switching {unit}: {e}");
                }
            }
        }
    }
}
