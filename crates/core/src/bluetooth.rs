//! Bluetooth, from BlueZ on the system bus (zbus directly, not `bluer`, which would bring a
//! second D-Bus library): the first adapter's power and its paired devices, with their
//! battery when BlueZ knows it; switching the adapter, connecting and disconnecting. BlueZ's
//! objects are read whole again on any signal it sends, and when it starts or stops.

use std::time::Duration;

use futures_lite::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time;
use tracing::{debug, info, warn};
use zbus::fdo::{self, ManagedObjects};
use zbus::message::Type;
use zbus::names::InterfaceName;
use zbus::zvariant::{OwnedObjectPath, Value};
use zbus::{MatchRule, MessageStream};

use crate::Update;
use crate::bus::{self, Properties};

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const BATTERY: &str = "org.bluez.Battery1";
/// Signals coming together are read once, after this.
const SETTLE: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq)]
pub struct Bluetooth {
    pub powered: bool,
    /// The paired devices, by name.
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// Its object, which requests name.
    pub path: String,
    pub name: String,
    /// BlueZ's icon name for its kind, as "audio-headphones".
    pub icon: Option<String>,
    pub connected: bool,
    /// In percent, when BlueZ reports it.
    pub battery: Option<u8>,
}

pub enum Request {
    Power(bool),
    Connect(String),
    Disconnect(String),
}

/// Send Bluetooth (`None`: no BlueZ, or no adapter) now and on each change, and carry out
/// the requests.
pub async fn run(
    bus: zbus::Connection,
    updates: UnboundedSender<Update>,
    mut requests: UnboundedReceiver<Request>,
) {
    let serve = async {
        while let Some(request) = requests.recv().await {
            let bus = bus.clone();
            tokio::spawn(async move {
                if let Err(e) = carry_out(&bus, request).await {
                    warn!("bluetooth: {e}");
                }
            });
        }
    };
    tokio::select! {
        followed = follow(&bus, &updates) => if let Err(e) = followed {
            info!("no bluetooth: {e}");
            let _ = updates.send(Update::Bluetooth(None));
        },
        () = serve => {}
    }
}

async fn follow(bus: &zbus::Connection, updates: &UnboundedSender<Update>) -> zbus::Result<()> {
    // Listening before the first read, so that no change is missed between them.
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(BLUEZ)?
        .build();
    let mut signals = MessageStream::for_match_rule(rule, bus, None).await?;
    let mut owners = fdo::DBusProxy::new(bus)
        .await?
        .receive_name_owner_changed_with_args(&[(0, BLUEZ)])
        .await?;
    let objects = objects(bus).await?;
    let mut shown = None;
    loop {
        let read = objects.get_managed_objects().await.ok();
        let bluetooth = read.as_ref().and_then(bluetooth);
        if shown.as_ref() != Some(&bluetooth) {
            debug!("bluetooth: {bluetooth:?}");
            if updates.send(Update::Bluetooth(bluetooth.clone())).is_err() {
                return Ok(());
            }
            shown = Some(bluetooth);
        }
        tokio::select! {
            signal = signals.next() => if signal.is_none() { return Ok(()) },
            owner = owners.next() => if owner.is_none() { return Ok(()) },
        }
        let settle = time::sleep(SETTLE);
        tokio::pin!(settle);
        loop {
            tokio::select! {
                () = &mut settle => break,
                signal = signals.next() => if signal.is_none() { return Ok(()) },
            }
        }
    }
}

/// An object's properties on `interface`, if it has that interface.
fn interface<'a>(
    objects: &'a ManagedObjects,
    path: &str,
    interface: &str,
) -> Option<&'a Properties> {
    let interfaces = objects.get(&OwnedObjectPath::try_from(path).ok()?)?;
    let (_, properties) = interfaces
        .iter()
        .find(|(name, _)| name.as_str() == interface)?;
    Some(properties)
}

/// The first adapter's path.
fn adapter(objects: &ManagedObjects) -> Option<String> {
    let mut paths: Vec<String> = objects.keys().map(|p| p.to_string()).collect();
    paths.sort();
    paths
        .into_iter()
        .find(|p| interface(objects, p, ADAPTER).is_some())
}

/// The first adapter and its paired devices, from BlueZ's objects.
fn bluetooth(objects: &ManagedObjects) -> Option<Bluetooth> {
    let adapter = adapter(objects)?;
    let powered = bus::get(interface(objects, &adapter, ADAPTER)?, "Powered");
    let mut devices: Vec<Device> = objects
        .keys()
        .filter(|p| p.as_str().starts_with(&format!("{adapter}/")))
        .filter_map(|p| {
            let device = interface(objects, p, DEVICE)?;
            if !bus::get::<bool>(device, "Paired")? {
                return None;
            }
            let battery = interface(objects, p, BATTERY).and_then(|b| bus::get(b, "Percentage"));
            Some(Device {
                path: p.to_string(),
                name: bus::get(device, "Alias").or_else(|| bus::get(device, "Name"))?,
                icon: bus::get(device, "Icon"),
                connected: bus::get(device, "Connected").unwrap_or(false),
                battery,
            })
        })
        .collect();
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Some(Bluetooth {
        powered: powered.unwrap_or(false),
        devices,
    })
}

async fn objects(bus: &zbus::Connection) -> zbus::Result<fdo::ObjectManagerProxy<'static>> {
    fdo::ObjectManagerProxy::builder(bus)
        .destination(BLUEZ)?
        .path("/")?
        .build()
        .await
}

async fn carry_out(bus: &zbus::Connection, request: Request) -> zbus::Result<()> {
    let (path, method) = match request {
        Request::Power(on) => {
            let Some(adapter) = adapter(&objects(bus).await?.get_managed_objects().await?) else {
                return Ok(());
            };
            let properties = fdo::PropertiesProxy::builder(bus)
                .destination(BLUEZ)?
                .path(adapter)?
                .build()
                .await?;
            let interface = InterfaceName::from_static_str_unchecked(ADAPTER);
            return Ok(properties
                .set(interface, "Powered", Value::from(on))
                .await?);
        }
        Request::Connect(path) => (path, "Connect"),
        Request::Disconnect(path) => (path, "Disconnect"),
    };
    bus.call_method(Some(BLUEZ), path.as_str(), Some(DEVICE), method, &())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zbus::names::OwnedInterfaceName;
    use zbus::zvariant::OwnedValue;

    fn object(
        interfaces: &[(&str, Vec<(&str, Value)>)],
    ) -> HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>> {
        interfaces
            .iter()
            .map(|(name, properties)| {
                let properties = properties
                    .iter()
                    .map(|(k, v)| (k.to_string(), OwnedValue::try_from(v.clone()).unwrap()));
                (
                    OwnedInterfaceName::try_from(*name).unwrap(),
                    properties.collect(),
                )
            })
            .collect()
    }

    fn path(p: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(p).unwrap()
    }

    #[test]
    fn the_adapter_and_its_paired_devices() {
        let headphones = vec![
            ("Alias", Value::from("WH-1000XM4")),
            ("Icon", Value::from("audio-headphones")),
            ("Paired", Value::from(true)),
            ("Connected", Value::from(true)),
        ];
        let mouse = vec![
            ("Alias", Value::from("Mouse")),
            ("Paired", Value::from(true)),
            ("Connected", Value::from(false)),
        ];
        let stranger = vec![
            ("Alias", Value::from("Phone")),
            ("Paired", Value::from(false)),
        ];
        let objects: ManagedObjects = HashMap::from([
            (
                path("/org/bluez"),
                object(&[("org.bluez.AgentManager1", vec![])]),
            ),
            (
                path("/org/bluez/hci0"),
                object(&[(ADAPTER, vec![("Powered", Value::from(true))])]),
            ),
            (
                path("/org/bluez/hci0/dev_1"),
                object(&[
                    (DEVICE, headphones),
                    (BATTERY, vec![("Percentage", Value::from(80u8))]),
                ]),
            ),
            (path("/org/bluez/hci0/dev_2"), object(&[(DEVICE, mouse)])),
            (path("/org/bluez/hci0/dev_3"), object(&[(DEVICE, stranger)])),
        ]);
        let bluetooth = bluetooth(&objects).unwrap();
        assert!(bluetooth.powered);
        let devices: Vec<_> = bluetooth
            .devices
            .iter()
            .map(|d| (d.name.as_str(), d.connected, d.battery))
            .collect();
        assert_eq!(
            devices,
            [("Mouse", false, None), ("WH-1000XM4", true, Some(80))]
        );
        assert_eq!(
            bluetooth.devices[1].icon.as_deref(),
            Some("audio-headphones")
        );
        assert_eq!(bluetooth.devices[1].path, "/org/bluez/hci0/dev_1");
    }

    #[test]
    fn no_adapter_is_no_bluetooth() {
        let objects: ManagedObjects = HashMap::from([(path("/org/bluez"), object(&[]))]);
        assert_eq!(bluetooth(&objects), None);
    }
}
