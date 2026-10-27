//! The battery: UPower's DisplayDevice (all batteries as one), as the greeter reads it.

use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info};

use crate::Update;
use crate::bus::{self, Object, Properties};

const DISPLAY_DEVICE: Object = Object {
    service: "org.freedesktop.UPower",
    path: "/org/freedesktop/UPower/devices/DisplayDevice",
    interface: "org.freedesktop.UPower.Device",
};

#[derive(Clone, Debug, PartialEq)]
pub struct Battery {
    /// 0 to 100.
    pub percent: u8,
    pub state: State,
    /// As UPower estimates them, when it does.
    pub time_to_empty: Option<Duration>,
    pub time_to_full: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    Charging,
    Discharging,
    FullyCharged,
    /// On mains power, but not charging (UPower's "pending charge").
    NotCharging,
}

/// Send the battery (`None`: none, or no UPower) now and on each change.
pub async fn run(bus: zbus::Connection, updates: UnboundedSender<Update>) {
    let send = |battery: Option<Battery>| {
        debug!("battery: {battery:?}");
        updates.send(Update::Battery(battery)).is_ok()
    };
    if let Err(e) = bus::follow(&bus, DISPLAY_DEVICE, battery, send).await {
        info!("no battery from UPower: {e:#}");
        let _ = updates.send(Update::Battery(None));
    }
}

/// The battery in the DisplayDevice's properties; one missing, or not present, is none.
fn battery(all: &Properties) -> Option<Battery> {
    if !bus::get::<bool>(all, "IsPresent")? {
        return None;
    }
    let percentage: f64 = bus::get(all, "Percentage")?;
    // UPower's states: 1 charging, 4 fully charged, 5 pending charge; the others (unknown,
    // discharging, empty, pending discharge) are on battery.
    let state = match bus::get::<u32>(all, "State")? {
        1 => State::Charging,
        4 => State::FullyCharged,
        5 => State::NotCharging,
        _ => State::Discharging,
    };
    // In seconds, 0 when unknown.
    let time = |name| {
        let seconds = bus::get::<i64>(all, name).unwrap_or(0);
        (seconds > 0).then(|| Duration::from_secs(seconds as u64))
    };
    Some(Battery {
        percent: percentage.clamp(0.0, 100.0).round() as u8,
        state,
        time_to_empty: time("TimeToEmpty"),
        time_to_full: time("TimeToFull"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{OwnedValue, Value};

    fn properties(pairs: &[(&str, Value)]) -> Properties {
        let owned = pairs.iter().map(|(name, value)| {
            let value = OwnedValue::try_from(value.clone()).unwrap();
            (name.to_string(), value)
        });
        owned.collect()
    }

    #[test]
    fn the_display_device_is_a_battery() {
        let all = properties(&[
            ("IsPresent", true.into()),
            ("Percentage", 49.6.into()),
            ("State", 2u32.into()),
            ("TimeToEmpty", 5400i64.into()),
            ("TimeToFull", 0i64.into()),
        ]);
        let want = Battery {
            percent: 50,
            state: State::Discharging,
            time_to_empty: Some(Duration::from_secs(5400)),
            time_to_full: None,
        };
        assert_eq!(battery(&all), Some(want));
    }

    #[test]
    fn states_and_mains_power() {
        let state = |state: u32| {
            let all = properties(&[
                ("IsPresent", true.into()),
                ("Percentage", 100.0.into()),
                ("State", state.into()),
            ]);
            battery(&all).unwrap()
        };
        assert_eq!(state(1).state, State::Charging);
        assert_eq!(state(4).state, State::FullyCharged);
        assert_eq!(state(5).state, State::NotCharging);
        for on_battery in [0, 2, 3, 6] {
            assert_eq!(state(on_battery).state, State::Discharging, "{on_battery}");
        }
    }

    #[test]
    fn no_battery_when_absent_or_unreadable() {
        let absent = [
            ("IsPresent", false.into()),
            ("Percentage", 50.0.into()),
            ("State", 2u32.into()),
        ];
        assert_eq!(battery(&properties(&absent)), None);
        assert_eq!(battery(&properties(&absent[1..])), None);
        assert_eq!(battery(&properties(&[("IsPresent", 1u32.into())])), None);
    }
}
