//! The power profile, from power-profiles-daemon, and setting it.

use std::collections::HashMap;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{debug, info, warn};
use zbus::zvariant::{OwnedValue, Value};

use crate::Update;
use crate::bus::{self, Object, Properties};

/// Its name since 0.20 (the older `net.hadess` one is not followed).
const DAEMON: Object = Object {
    service: "org.freedesktop.UPower.PowerProfiles",
    path: "/org/freedesktop/UPower/PowerProfiles",
    interface: "org.freedesktop.UPower.PowerProfiles",
};

/// The daemon's profiles, in this order when offered.
pub const PROFILES: [&str; 3] = ["performance", "balanced", "power-saver"];

#[derive(Clone, Debug, PartialEq)]
pub struct Profiles {
    pub active: String,
    /// Those of `PROFILES` the daemon offers.
    pub offered: Vec<String>,
    /// Why performance is held back, if it is.
    pub degraded: Option<String>,
}

/// Send the profiles (`None`: no daemon) now and on each change, and set the profiles
/// asked for on `requests`.
pub async fn run(
    bus: zbus::Connection,
    updates: UnboundedSender<Update>,
    mut requests: UnboundedReceiver<String>,
) {
    let send = |profiles: Option<Profiles>| {
        debug!("power profiles: {profiles:?}");
        updates.send(Update::Profiles(profiles)).is_ok()
    };
    let set = async {
        while let Some(profile) = requests.recv().await {
            if let Err(e) = set(&bus, &profile).await {
                warn!("setting the power profile to {profile}: {e}");
            }
        }
    };
    tokio::select! {
        followed = bus::follow(&bus, DAEMON, profiles, send) => if let Err(e) = followed {
            info!("no power profiles: {e:#}");
            let _ = updates.send(Update::Profiles(None));
        },
        () = set => {}
    }
}

async fn set(bus: &zbus::Connection, profile: &str) -> zbus::Result<()> {
    let interface = zbus::names::InterfaceName::try_from(DAEMON.interface)?;
    let proxy = DAEMON.proxy(bus).await?;
    Ok(proxy
        .set(interface, "ActiveProfile", Value::from(profile))
        .await?)
}

fn profiles(all: &Properties) -> Option<Profiles> {
    let active = bus::get(all, "ActiveProfile")?;
    let listed: Vec<HashMap<String, OwnedValue>> = bus::get(all, "Profiles")?;
    let listed: Vec<String> = listed
        .iter()
        .filter_map(|profile| bus::get(profile, "Profile"))
        .collect();
    let offered = PROFILES
        .iter()
        .filter(|profile| listed.iter().any(|listed| listed == *profile))
        .map(|profile| profile.to_string())
        .collect();
    let degraded = bus::get::<String>(all, "PerformanceDegraded").filter(|why| !why.is_empty());
    Some(Profiles {
        active,
        offered,
        degraded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    fn properties(active: &str, listed: &[&str], degraded: &str) -> Properties {
        let listed: Vec<HashMap<String, Value>> = listed
            .iter()
            .map(|name| HashMap::from([("Profile".to_string(), Value::from(*name))]))
            .collect();
        let pairs = [
            ("ActiveProfile", Value::from(active)),
            ("Profiles", Value::from(listed)),
            ("PerformanceDegraded", Value::from(degraded)),
        ];
        let owned = pairs
            .into_iter()
            .map(|(name, value)| (name.to_string(), OwnedValue::try_from(value).unwrap()));
        owned.collect()
    }

    #[test]
    fn profiles_come_in_their_order() {
        let all = properties("balanced", &["power-saver", "balanced", "performance"], "");
        let want = Profiles {
            active: "balanced".into(),
            offered: vec![
                "performance".into(),
                "balanced".into(),
                "power-saver".into(),
            ],
            degraded: None,
        };
        assert_eq!(profiles(&all), Some(want));
    }

    #[test]
    fn unknown_profiles_are_left_out_and_degradation_is_kept() {
        let all = properties(
            "power-saver",
            &["power-saver", "turbo", "balanced"],
            "lap-detected",
        );
        let profiles = profiles(&all).unwrap();
        assert_eq!(profiles.offered, ["balanced", "power-saver"]);
        assert_eq!(profiles.degraded.as_deref(), Some("lap-detected"));
    }

    #[test]
    fn missing_properties_are_no_daemon() {
        assert_eq!(profiles(&Properties::new()), None);
    }
}
