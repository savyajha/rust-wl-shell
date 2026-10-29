//! The power menu's actions: locking, sleep, restart and shut down through logind on the
//! system bus (each `interactive`, so polkit may ask), and logging out through niri, whose
//! session ends with it. Before a restart or a shut down, logind's block inhibitors for it,
//! which the dialog lists.

use std::fs;
use std::os::unix::fs::MetadataExt;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{info, warn};
use zbus::proxy;
use zbus::zvariant::OwnedObjectPath;

use crate::{Update, niri};

/// What logind offers (`yes`, or `challenge`: allowed after polkit asks).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Power {
    pub suspend: bool,
    pub reboot: bool,
    pub power_off: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Lock,
    Suspend,
    Reboot,
    PowerOff,
    LogOut,
}

pub enum Request {
    /// To ask for an action: its block inhibitors, for its dialog (`Asked`).
    Ask(Action),
    Act(Action),
}

/// An inhibitor, as logind lists it: what, who, why, mode, uid, pid.
type Inhibitor = (String, String, String, String, u32, u32);

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    fn can_suspend(&self) -> zbus::Result<String>;
    fn can_reboot(&self) -> zbus::Result<String>;
    fn can_power_off(&self) -> zbus::Result<String>;
    fn suspend(&self, interactive: bool) -> zbus::Result<()>;
    fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    fn power_off(&self, interactive: bool) -> zbus::Result<()>;
    fn lock_session(&self, id: &str) -> zbus::Result<()>;
    fn get_user(&self, uid: u32) -> zbus::Result<OwnedObjectPath>;
    fn list_inhibitors(&self) -> zbus::Result<Vec<Inhibitor>>;
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.freedesktop.login1.User",
    default_service = "org.freedesktop.login1"
)]
trait User {
    /// The user's graphical session: (id, object).
    #[zbus(property)]
    fn display(&self) -> zbus::Result<(String, OwnedObjectPath)>;
    #[zbus(property)]
    fn sessions(&self) -> zbus::Result<Vec<(String, OwnedObjectPath)>>;
}

#[proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Session {
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;
}

/// Send what logind offers (`None`: no logind, so only logging out), then carry out the
/// requests, each action's outcome sent (`Acted`, with logind's error if it refused). A
/// sleep starting, or the session locked, interrupts what the dialog asks.
pub async fn run(
    bus: Option<zbus::Connection>,
    updates: UnboundedSender<Update>,
    niri: UnboundedSender<niri::Command>,
    mut requests: UnboundedReceiver<Request>,
) {
    let manager = match &bus {
        Some(bus) => ManagerProxy::new(bus).await.ok(),
        None => None,
    };
    let (Some(bus), Some(manager)) = (bus, manager) else {
        info!("no logind: of the power actions, only logging out");
        let _ = updates.send(Update::Power(None));
        while let Some(request) = requests.recv().await {
            let update = match request {
                Request::Ask(action) => Update::Asked(action, Vec::new()),
                Request::Act(Action::LogOut) => log_out(&niri),
                Request::Act(action) => Update::Acted(action, Some("No logind".into())),
            };
            let _ = updates.send(update);
        }
        return;
    };
    let can = |answer: zbus::Result<String>| answer.is_ok_and(|a| a == "yes" || a == "challenge");
    let offered = Power {
        suspend: can(manager.can_suspend().await),
        reboot: can(manager.can_reboot().await),
        power_off: can(manager.can_power_off().await),
    };
    let _ = updates.send(Update::Power(Some(offered)));
    let session = session(&bus, &manager).await;
    if let Err(e) = &session {
        warn!("power: no session to lock: {e}");
    }
    let (id, session) = session.ok().unzip();
    let mut sleeps = manager.receive_prepare_for_sleep().await.ok();
    let mut locks = match &session {
        Some(session) => Some(session.receive_locked_hint_changed().await),
        None => None,
    };
    let mut sleeping = false;
    loop {
        tokio::select! {
            request = requests.recv() => {
                let Some(request) = request else { return };
                let update = match request {
                    // No dialog while the machine goes to sleep.
                    Request::Ask(_) if sleeping => continue,
                    Request::Ask(action) => {
                        let listed = manager.list_inhibitors().await.unwrap_or_default();
                        Update::Asked(action, blockers(&listed, action))
                    }
                    Request::Act(Action::LogOut) => log_out(&niri),
                    Request::Act(action) => {
                        info!("power: {action:?}");
                        let acted = match (action, &id) {
                            (Action::Lock, Some(id)) => manager.lock_session(id).await,
                            (Action::Lock, None) => Err(zbus::Error::Failure("No session".into())),
                            (Action::Suspend, _) => manager.suspend(true).await,
                            (Action::Reboot, _) => manager.reboot(true).await,
                            _ => manager.power_off(true).await,
                        };
                        let error = acted.err().map(|e| match e {
                            zbus::Error::MethodError(_, Some(text), _) => text,
                            e => e.to_string(),
                        });
                        if let Some(e) = &error {
                            warn!("power: {action:?}: {e}");
                        }
                        Update::Acted(action, error)
                    }
                };
                let _ = updates.send(update);
            }
            Some(sleep) = next(&mut sleeps) => {
                sleeping = sleep.args().is_ok_and(|a| a.start);
                if sleeping {
                    let _ = updates.send(Update::Interrupted);
                }
            }
            Some(locked) = next(&mut locks) => {
                if locked.get().await.unwrap_or(false) {
                    let _ = updates.send(Update::Interrupted);
                }
            }
        }
    }
}

/// The next of `stream`, or never without one.
async fn next<S: futures_lite::Stream + Unpin>(stream: &mut Option<S>) -> Option<S::Item> {
    use futures_lite::StreamExt;
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

fn log_out(niri: &UnboundedSender<niri::Command>) -> Update {
    let _ = niri.send(niri::Command::Quit);
    Update::Acted(Action::LogOut, None)
}

/// The user's graphical session (else the first of theirs): its id, to lock it, and its
/// object, followed. The shell runs as a user unit, outside any session.
async fn session(
    bus: &zbus::Connection,
    manager: &ManagerProxy<'_>,
) -> zbus::Result<(String, SessionProxy<'static>)> {
    let uid = fs::metadata("/proc/self")
        .map_err(|e| zbus::Error::Failure(e.to_string()))?
        .uid();
    let user = UserProxy::builder(bus)
        .path(manager.get_user(uid).await?)?
        .build()
        .await?;
    let display = user.display().await?;
    let (id, path) = match display.0.is_empty() {
        true => user
            .sessions()
            .await?
            .into_iter()
            .next()
            .ok_or(zbus::Error::Failure("No session".into()))?,
        false => display,
    };
    let session = SessionProxy::builder(bus).path(path)?.build().await?;
    Ok((id, session))
}

/// The block inhibitors of `action`, as "app: reason": a shut down's and a restart's are
/// `shutdown`, sleep's `sleep`; delay ones are left out, as logind waits for them anyway.
fn blockers(listed: &[Inhibitor], action: Action) -> Vec<String> {
    let what = match action {
        Action::Suspend => "sleep",
        Action::Reboot | Action::PowerOff => "shutdown",
        Action::Lock | Action::LogOut => return Vec::new(),
    };
    listed
        .iter()
        .filter(|(whats, _, _, mode, _, _)| {
            mode.starts_with("block") && whats.split(':').any(|w| w == what)
        })
        .map(|(_, who, why, _, _, _)| format!("{who}: {why}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_inhibitors_of_an_action() {
        let inhibitor = |what: &str, who: &str, mode: &str| {
            (
                what.to_string(),
                who.to_string(),
                "Busy".to_string(),
                mode.to_string(),
                0,
                0,
            )
        };
        let listed = [
            inhibitor("shutdown:sleep", "Firefox", "block"),
            inhibitor("shutdown", "session-manager", "delay"),
            inhibitor("sleep", "Player", "block"),
            inhibitor("idle", "rust-wl-shell", "block"),
        ];
        assert_eq!(blockers(&listed, Action::PowerOff), ["Firefox: Busy"]);
        assert_eq!(blockers(&listed, Action::Reboot), ["Firefox: Busy"]);
        assert_eq!(
            blockers(&listed, Action::Suspend),
            ["Firefox: Busy", "Player: Busy"]
        );
        assert!(blockers(&listed, Action::LogOut).is_empty());
    }
}
