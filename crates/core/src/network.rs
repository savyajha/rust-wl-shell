//! NetworkManager through nmrs: the primary connection, the Wi-Fi networks in range, the
//! VPN connections, and joining, activating and switching them. nmrs opens its own system
//! bus connection; its event stream (devices, access points, active connections, the
//! Wi-Fi switch, saved connections, NM restarting) drives a fresh read, so nothing polls.

// nmrs's own error type is large; boxing it here would gain nothing.
#![allow(clippy::result_large_err)]

use std::time::Duration;

use futures_lite::StreamExt;
use nmrs::{
    ActiveConnection, ActiveConnectionState, ConnectionError, EapMethod, EapOptions,
    NetworkManager, Phase2, SettingsSummary, WifiSecurity, builders,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time;
use tracing::{debug, info, warn};

use crate::Update;
use crate::bus;

/// The system's CA bundle: NM's iwd backend refuses `system-ca-certs`, so the bundle is
/// named as the CA certificate (iwd reads it only if it holds plain certificates).
const CA_BUNDLE: &str = "file:///etc/ssl/certs/ca-certificates.crt";
/// How long iwd takes to load the profile NM writes for an enterprise network, before NM
/// can activate it (measured: at once fails, 1 s works).
const IWD_LOADS: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    pub wifi_enabled: bool,
    /// Whether there is a Wi-Fi device at all.
    pub wifi_present: bool,
    pub primary: Primary,
    /// The active wired connection's name.
    pub wired: Option<String>,
    /// The known networks in range (the current one first), then the others by signal.
    pub networks: Vec<WifiNetwork>,
    pub vpns: Vec<Vpn>,
}

/// What the bar's icon shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Primary {
    Wired,
    /// Its signal, 0 to 100, and its SSID (from the active connection: its access point
    /// may be missing from the last scan's).
    Wifi(u8, String),
    Connecting,
    Offline,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// The strongest access point's, 0 to 100.
    pub strength: u8,
    pub security: Security,
    /// Its saved connection's UUID, when it has one.
    pub known: Option<String>,
    pub current: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Security {
    Open,
    Wpa2,
    Wpa3,
    Enterprise,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vpn {
    pub uuid: String,
    pub name: String,
    pub state: VpnState,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VpnState {
    Off,
    Connecting,
    On,
}

pub enum Request {
    /// When the Wi-Fi popover opens.
    Scan,
    Wifi(bool),
    /// A saved connection: a known network, or a VPN to switch on.
    Activate(String),
    /// A VPN to switch off.
    Deactivate(String),
    Join {
        ssid: String,
        secret: Secret,
    },
}

/// What joining a network takes. Neither it nor `Request` is `Debug`, so they are never
/// logged.
pub enum Secret {
    Open,
    Password(String),
    Eap(Eap),
}

pub struct Eap {
    pub identity: String,
    pub password: String,
    /// PEAP, else TTLS.
    pub peap: bool,
    /// MSCHAPv2, else PAP.
    pub mschapv2: bool,
    /// The domain the server's certificate must be in, if any.
    pub domain: String,
}

/// How a join ended: `error` is why it failed.
#[derive(Clone, Debug, PartialEq)]
pub struct Joined {
    pub ssid: String,
    pub error: Option<String>,
}

/// Send the network (`None`: no NetworkManager) now and on each change, and carry out the
/// requests.
pub async fn run(updates: UnboundedSender<Update>, mut requests: UnboundedReceiver<Request>) {
    let nm = match NetworkManager::new().await {
        Ok(nm) => nm,
        Err(e) => {
            info!("no network: {e}");
            let _ = updates.send(Update::Network(None));
            return;
        }
    };
    let follow = follow(nm.clone(), updates.clone());
    let serve = async {
        while let Some(request) = requests.recv().await {
            tokio::spawn(carry_out(nm.clone(), request, updates.clone()));
        }
    };
    tokio::select! {
        () = follow => {}
        () = serve => {}
    }
}

async fn follow(nm: NetworkManager, updates: UnboundedSender<Update>) {
    let mut events = match nm.network_events().await {
        Ok(events) => events,
        Err(e) => {
            info!("no network: {e}");
            let _ = updates.send(Update::Network(None));
            return;
        }
    };
    let mut shown = None;
    loop {
        // A failed read (NM stopped) shows no network, until NM is back.
        let network = read(&nm).await.inspect_err(|e| debug!("network: {e}")).ok();
        if shown.as_ref() != Some(&network) {
            debug!("network: {network:?}");
            if updates.send(Update::Network(network.clone())).is_err() {
                return;
            }
            shown = Some(network);
        }
        if events.next().await.is_none() {
            return;
        }
        if !bus::settle(&mut events).await {
            return;
        }
    }
}

async fn read(nm: &NetworkManager) -> nmrs::Result<Network> {
    let radio = nm.wifi_state().await?;
    let aps = nm.list_access_points(None).await?;
    let aps = aps.iter().filter(|ap| !ap.is_hidden()).map(|ap| {
        let s = &ap.security;
        let security = match () {
            () if s.is_open() => Security::Open,
            () if s.is_enterprise() => Security::Enterprise,
            () if s.sae && !s.psk => Security::Wpa3,
            () => Security::Wpa2,
        };
        (ap.ssid.clone(), ap.strength, security)
    });
    let saved = nm.list_saved_connections().await?;
    let saved = saved.iter().filter_map(|c| match &c.summary {
        SettingsSummary::Wifi { ssid, .. } => Some(Saved::Wifi(ssid.clone(), c.uuid.clone())),
        SettingsSummary::Vpn { .. } | SettingsSummary::WireGuard { .. } => {
            Some(Saved::Vpn(c.uuid.clone(), c.id.clone()))
        }
        _ => None,
    });
    let active = nm.list_active_connections().await?;
    let active = active.iter().filter_map(|a| {
        let (uuid, kind, state) = match a {
            ActiveConnection::Wifi(w) => {
                let kind = Kind::Wifi(w.strength.unwrap_or(0), w.ssid.clone());
                (&w.uuid, kind, &w.state)
            }
            ActiveConnection::Wired(w) => (&w.uuid, Kind::Wired(w.id.clone()), &w.state),
            ActiveConnection::Vpn(v) => (&v.uuid, Kind::Vpn, &v.state),
            _ => return None,
        };
        let on = match state {
            ActiveConnectionState::Activated => true,
            ActiveConnectionState::Activating => false,
            _ => return None,
        };
        Some(Active {
            uuid: uuid.clone(),
            kind,
            on,
        })
    });
    Ok(network(
        (radio.enabled, radio.present),
        aps.collect(),
        saved.collect(),
        active.collect(),
    ))
}

/// A saved connection: a Wi-Fi one (its SSID and UUID), or a VPN (its UUID and name).
enum Saved {
    Wifi(String, String),
    Vpn(String, String),
}

/// An active connection, `on` once activated (else activating).
struct Active {
    uuid: String,
    kind: Kind,
    on: bool,
}

enum Kind {
    /// Its signal and SSID.
    Wifi(u8, String),
    /// Its name.
    Wired(String),
    Vpn,
}

/// The network from what NM lists: the access points (SSID, signal, security), the saved
/// connections and the active ones.
fn network(
    (wifi_enabled, wifi_present): (bool, bool),
    aps: Vec<(String, u8, Security)>,
    saved: Vec<Saved>,
    active: Vec<Active>,
) -> Network {
    let known = |ssid: &str| {
        saved.iter().find_map(|s| match s {
            Saved::Wifi(saved, uuid) if saved == ssid => Some(uuid.clone()),
            _ => None,
        })
    };
    let is_active = |uuid: &Option<String>| {
        let wifi = |a: &&Active| matches!(a.kind, Kind::Wifi(..));
        active
            .iter()
            .filter(wifi)
            .any(|a| Some(&a.uuid) == uuid.as_ref())
    };
    // One per SSID, its strongest access point's.
    let mut networks: Vec<WifiNetwork> = Vec::new();
    for (ssid, strength, security) in aps {
        match networks.iter_mut().find(|n| n.ssid == ssid) {
            Some(n) => n.strength = n.strength.max(strength),
            None => {
                let known = known(&ssid);
                let current = is_active(&known);
                networks.push(WifiNetwork {
                    ssid,
                    strength,
                    security,
                    known,
                    current,
                });
            }
        }
    }
    networks.sort_by_key(|n| (!n.current, n.known.is_none(), u8::MAX - n.strength));
    let activated = |wanted: fn(&Kind) -> bool| active.iter().find(|a| a.on && wanted(&a.kind));
    let primary = if activated(|k| matches!(k, Kind::Wired(_))).is_some() {
        Primary::Wired
    } else if let Some(Active {
        kind: Kind::Wifi(strength, ssid),
        ..
    }) = activated(|k| matches!(k, Kind::Wifi(..)))
    {
        Primary::Wifi(*strength, ssid.clone())
    } else if active.iter().any(|a| !a.on && !matches!(a.kind, Kind::Vpn)) {
        Primary::Connecting
    } else {
        Primary::Offline
    };
    let wired = active.iter().find_map(|a| match &a.kind {
        Kind::Wired(name) if a.on => Some(name.clone()),
        _ => None,
    });
    let vpns = saved.iter().filter_map(|s| {
        let Saved::Vpn(uuid, name) = s else {
            return None;
        };
        let state = match active.iter().find(|a| a.uuid == *uuid) {
            Some(a) if a.on => VpnState::On,
            Some(_) => VpnState::Connecting,
            None => VpnState::Off,
        };
        Some(Vpn {
            uuid: uuid.clone(),
            name: name.clone(),
            state,
        })
    });
    Network {
        wifi_enabled,
        wifi_present,
        primary,
        wired,
        networks,
        vpns: vpns.collect(),
    }
}

async fn carry_out(nm: NetworkManager, request: Request, updates: UnboundedSender<Update>) {
    let result = match request {
        Request::Scan => nm.scan_networks(None).await,
        Request::Wifi(on) => nm.set_wireless_enabled(on).await,
        Request::Activate(uuid) => activate(&nm, &uuid).await,
        Request::Deactivate(uuid) => nm.disconnect_by_uuid(&uuid).await,
        Request::Join { ssid, secret } => {
            let error = join(&nm, &ssid, secret).await.err().map(|e| match e {
                ConnectionError::AuthFailed => "Wrong password or credentials.".into(),
                e => format!("{e}."),
            });
            let _ = updates.send(Update::Joined(Joined { ssid, error }));
            Ok(())
        }
    };
    if let Err(e) = result {
        warn!("network: {e}");
    }
}

/// A saved connection. A Wi-Fi one whose password was refused is activated again: then NM
/// asks the secret agent for a new one (it does not on the first try, with iwd).
async fn activate(nm: &NetworkManager, uuid: &str) -> nmrs::Result<()> {
    match nm.connect_by_uuid(uuid, Default::default()).await {
        Err(ConnectionError::AuthFailed) => nm.connect_by_uuid(uuid, Default::default()).await,
        result => result,
    }
}

async fn join(nm: &NetworkManager, ssid: &str, secret: Secret) -> nmrs::Result<()> {
    let eap = match secret {
        Secret::Open => return nm.connect(ssid, None, WifiSecurity::Open).await,
        Secret::Password(psk) => return nm.connect(ssid, None, WifiSecurity::WpaPsk { psk }).await,
        Secret::Eap(eap) => eap,
    };
    let mut opts = EapOptions::new(eap.identity, eap.password);
    opts.method = if eap.peap {
        EapMethod::Peap
    } else {
        EapMethod::Ttls
    };
    opts.phase2 = if eap.mschapv2 {
        Phase2::Mschapv2
    } else {
        Phase2::Pap
    };
    opts.domain_suffix_match = Some(eap.domain).filter(|d| !d.is_empty());
    opts.ca_cert_path = Some(CA_BUNDLE.into());
    // Added, then activated: NM's iwd backend refuses to add and activate at once an
    // enterprise network iwd has no profile for yet.
    let security = WifiSecurity::WpaEap { opts };
    let settings = builders::try_build_wifi_connection(ssid, &security, &Default::default())?;
    let uuid = match settings.get("connection").and_then(|c| c.get("uuid")) {
        Some(zbus::zvariant::Value::Str(uuid)) => uuid.to_string(),
        _ => return Err(ConnectionError::IncompleteBuilder("no uuid".into())),
    };
    nm.add_connection(settings).await?;
    time::sleep(IWD_LOADS).await;
    let activated = nm.connect_by_uuid(&uuid, Default::default()).await;
    if activated.is_err() {
        // Not kept: a mistyped password is not a known network.
        nm.delete_saved_connection(&uuid).await?;
    }
    activated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(ssid: &str, strength: u8, security: Security) -> (String, u8, Security) {
        (ssid.into(), strength, security)
    }

    fn wifi(uuid: &str, ssid: &str) -> Saved {
        Saved::Wifi(ssid.into(), uuid.into())
    }

    fn active(uuid: &str, kind: Kind, on: bool) -> Active {
        Active {
            uuid: uuid.into(),
            kind,
            on,
        }
    }

    /// Each network's SSID, strength, known and current.
    fn listed(network: &Network) -> Vec<(&str, u8, bool, bool)> {
        let n = network.networks.iter();
        n.map(|n| (n.ssid.as_str(), n.strength, n.known.is_some(), n.current))
            .collect()
    }

    #[test]
    fn known_networks_first_the_current_one_on_top() {
        let aps = vec![
            ap("Cafe", 80, Security::Open),
            ap("Home", 40, Security::Wpa2),
            ap("Home", 60, Security::Wpa2),
            ap("Office", 30, Security::Enterprise),
            ap("Neighbour", 90, Security::Wpa3),
            ap("Library", 50, Security::Open),
        ];
        let saved = vec![wifi("h", "Home"), wifi("o", "Office"), wifi("x", "Away")];
        let active = vec![active("h", Kind::Wifi(60, "Home".into()), true)];
        let network = network((true, true), aps, saved, active);
        let want = [
            ("Home", 60, true, true),
            ("Office", 30, true, false),
            ("Neighbour", 90, false, false),
            ("Cafe", 80, false, false),
            ("Library", 50, false, false),
        ];
        assert_eq!(listed(&network), want);
        assert_eq!(network.primary, Primary::Wifi(60, "Home".into()));
        assert_eq!(network.networks[0].known.as_deref(), Some("h"));
        assert_eq!(network.networks[2].security, Security::Wpa3);
    }

    #[test]
    fn the_primary_connection() {
        let primary = |active: Vec<Active>| network((true, true), vec![], vec![], active).primary;
        let wired = || active("w", Kind::Wired("Ethernet".into()), true);
        let wifi = |on| active("h", Kind::Wifi(70, "Home".into()), on);
        let vpn = |on| active("v", Kind::Vpn, on);
        assert_eq!(primary(vec![wifi(true), wired()]), Primary::Wired);
        assert_eq!(
            primary(vec![wifi(true), vpn(true)]),
            Primary::Wifi(70, "Home".into())
        );
        assert_eq!(primary(vec![wifi(false)]), Primary::Connecting);
        // A VPN connecting is not the network connecting.
        assert_eq!(primary(vec![vpn(false)]), Primary::Offline);
        assert_eq!(primary(vec![]), Primary::Offline);
        let network = network((false, true), vec![], vec![], vec![wired()]);
        assert_eq!(network.wired.as_deref(), Some("Ethernet"));
        assert!(!network.wifi_enabled);
    }

    #[test]
    fn vpns_and_their_states() {
        let saved = vec![
            Saved::Vpn("a".into(), "Work".into()),
            Saved::Vpn("b".into(), "Home WireGuard".into()),
            Saved::Vpn("c".into(), "Spare".into()),
            wifi("h", "Home"),
        ];
        let active = vec![active("a", Kind::Vpn, true), active("b", Kind::Vpn, false)];
        let vpns = network((true, true), vec![], saved, active).vpns;
        let states: Vec<_> = vpns.iter().map(|v| (v.name.as_str(), v.state)).collect();
        let want = [
            ("Work", VpnState::On),
            ("Home WireGuard", VpnState::Connecting),
            ("Spare", VpnState::Off),
        ];
        assert_eq!(states, want);
    }
}
