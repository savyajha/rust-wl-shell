//! The NetworkManager secret agent, through nmrs: NM's own requests for a secret (a saved
//! network whose password changed, a VPN's password) become prompts for the UI's dialog,
//! and its answers go back to NM; NM cancelling a request closes the prompt. Registered
//! again when NM restarts. Secrets are never logged or kept.

// nmrs's own error type is large; boxing it here would gain nothing.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::time::Duration;

use futures_lite::StreamExt;
use nmrs::agent::{SecretAgent, SecretAgentFlags, SecretRequest, SecretResponder, SecretSetting};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::{self, Instant};
use tracing::{info, warn};
use zbus::fdo;
use zbus::zvariant::OwnedObjectPath;

use crate::Update;

const NM: &str = "org.freedesktop.NetworkManager";
/// A little under nmrs's 120 s: past it nmrs has answered NM with no secrets, and tells
/// no one, so the prompt is closed here.
const ANSWER_WITHIN: Duration = Duration::from_secs(115);

#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
    pub id: u64,
    /// The connection's name.
    pub name: String,
    pub kind: PromptKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PromptKind {
    /// A WPA password.
    Wifi,
    /// An enterprise network's username and password.
    Eap { identity: Option<String> },
    /// A VPN's password.
    Vpn,
}

/// The UI's answer to a prompt: `None` cancels it.
pub type Answer = (u64, Option<Secrets>);

/// Not `Debug`, so never logged.
pub struct Secrets {
    pub identity: String,
    pub password: String,
}

/// A request waiting for its answer.
struct Pending {
    responder: SecretResponder,
    path: OwnedObjectPath,
    kind: PromptKind,
    /// A VPN's secret's key, as NM hints it.
    key: String,
    deadline: Instant,
}

/// Register the agent (once NM is there) and serve it.
pub async fn run(
    bus: zbus::Connection,
    updates: UnboundedSender<Update>,
    mut answers: UnboundedReceiver<Answer>,
) {
    let owners = async {
        let proxy = fdo::DBusProxy::new(&bus).await?;
        proxy.receive_name_owner_changed_with_args(&[(0, NM)]).await
    };
    let mut owners = match owners.await {
        Ok(owners) => owners,
        Err(e) => return warn!("no secret agent: {e}"),
    };
    let register = || {
        SecretAgent::builder()
            .with_identifier("rust-wl-shell")
            .register()
    };
    let (mut handle, mut requests) = loop {
        match register().await {
            Ok(agent) => break agent,
            Err(e) => info!("no secret agent yet: {e}"),
        }
        if owners.next().await.is_none() {
            return;
        }
    };
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut next = 0;
    loop {
        tokio::select! {
            request = requests.next() => {
                let Some(request) = request else { return };
                next += 1;
                if let Some(p) = prompt(request, next, &mut pending).await {
                    let _ = updates.send(Update::Prompt(p));
                }
            }
            cancel = handle.cancellations().next() => {
                let Some(cancel) = cancel else { return };
                // NM has had its answer already: only the dialog is closed.
                pending.retain(|&id, p| {
                    let cancelled = p.path == cancel.connection_path;
                    if cancelled {
                        let _ = updates.send(Update::PromptClosed(id));
                    }
                    !cancelled
                });
            }
            answer = answers.recv() => {
                let Some((id, secrets)) = answer else { return };
                if let Some(p) = pending.remove(&id)
                    && let Err(e) = respond(p, secrets).await
                {
                    warn!("answering NM's request: {e}");
                }
            }
            () = time::sleep_until(next_deadline(&pending)) => {
                // nmrs has answered NM with no secrets by now, and tells no one.
                let now = Instant::now();
                pending.retain(|&id, p| {
                    let late = p.deadline <= now;
                    if late {
                        let _ = updates.send(Update::PromptClosed(id));
                    }
                    !late
                });
            }
            owner = owners.next() => {
                let Some(owner) = owner else { return };
                let back = owner.args().is_ok_and(|a| a.new_owner().is_some());
                if back && let Err(e) = handle.reregister().await {
                    warn!("registering the secret agent again: {e}");
                }
            }
        }
    }
}

/// The prompt for `request` (kept in `pending` under `id`); none for a request the user
/// may not be asked (answered with no secrets) or of a kind the dialog has no fields for.
async fn prompt(
    request: SecretRequest,
    id: u64,
    pending: &mut HashMap<u64, Pending>,
) -> Option<Prompt> {
    let kind = match request.setting {
        SecretSetting::WifiPsk { .. } => Some(PromptKind::Wifi),
        SecretSetting::WifiEap { identity, .. } => Some(PromptKind::Eap { identity }),
        SecretSetting::Vpn { .. } => Some(PromptKind::Vpn),
        _ => None,
    };
    let interactive = request.flags.contains(SecretAgentFlags::ALLOW_INTERACTION);
    let Some(kind) = kind.filter(|_| interactive) else {
        let _ = request.responder.no_secrets().await;
        return None;
    };
    let key = request.hints.first().cloned().unwrap_or("password".into());
    pending.insert(
        id,
        Pending {
            responder: request.responder,
            path: request.connection_path,
            kind: kind.clone(),
            key,
            deadline: Instant::now() + ANSWER_WITHIN,
        },
    );
    Some(Prompt {
        id,
        name: request.connection_id,
        kind,
    })
}

/// The earliest pending deadline, or a day away.
fn next_deadline(pending: &HashMap<u64, Pending>) -> Instant {
    let deadlines = pending.values().map(|p| p.deadline);
    deadlines
        .min()
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400))
}

async fn respond(pending: Pending, secrets: Option<Secrets>) -> nmrs::Result<()> {
    let Some(Secrets { identity, password }) = secrets else {
        return pending.responder.cancel().await;
    };
    let responder = pending.responder;
    match pending.kind {
        PromptKind::Wifi => responder.wifi_psk(password).await,
        PromptKind::Eap { .. } => responder.wifi_eap(Some(identity), password).await,
        PromptKind::Vpn => {
            let secrets = HashMap::from([(pending.key, password)]);
            responder.vpn_secrets(secrets).await
        }
    }
}
