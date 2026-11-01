//! The system bus, shared by the services, and the one way they read a D-Bus object: all
//! its properties, again on each change and each time its service starts or stops.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::bail;
use futures_lite::StreamExt;
use tokio::time;
use tracing::warn;
use zbus::fdo;
use zbus::names::InterfaceName;
use zbus::zvariant::OwnedValue;

/// How long the system bus gets to answer before the services go without it.
const TIMEOUT: Duration = Duration::from_secs(2);
/// How long a service's burst of changes (a scan, a device connecting) gets to settle.
const SETTLE: Duration = Duration::from_millis(100);

pub type Properties = HashMap<String, OwnedValue>;

/// The system bus, or none (warned) if it does not answer.
pub async fn system() -> Option<zbus::Connection> {
    let bus = time::timeout(TIMEOUT, zbus::Connection::system()).await;
    let error = match bus {
        Ok(Ok(bus)) => return Some(bus),
        Ok(Err(e)) => e.to_string(),
        Err(_) => "it did not answer".into(),
    };
    warn!("no system bus ({error}): no battery, power profile or resume signal");
    None
}

/// An object's properties on one interface.
#[derive(Clone, Copy)]
pub struct Object {
    pub service: &'static str,
    pub path: &'static str,
    pub interface: &'static str,
}

impl Object {
    pub async fn proxy(
        &self,
        bus: &zbus::Connection,
    ) -> zbus::Result<fdo::PropertiesProxy<'static>> {
        fdo::PropertiesProxy::builder(bus)
            .destination(self.service)?
            .path(self.path)?
            .build()
            .await
    }
}

/// Call `send` with what `read` makes of the object's properties now (`None` while its
/// service is not running), and again whenever that changes, until `send` returns false.
pub async fn follow<T: Clone + PartialEq>(
    bus: &zbus::Connection,
    object: Object,
    read: impl Fn(&Properties) -> Option<T>,
    send: impl Fn(Option<T>) -> bool,
) -> anyhow::Result<()> {
    let properties = object.proxy(bus).await?;
    // Listening before the first read, so that no change is missed between them.
    let mut changes = properties.receive_properties_changed().await?;
    let mut owners = fdo::DBusProxy::new(bus)
        .await?
        .receive_name_owner_changed_with_args(&[(0, object.service)])
        .await?;
    let mut shown = None;
    loop {
        // Starts the service if it is not running but can be (D-Bus activation): nothing
        // else may start UPower or power-profiles-daemon.
        let all = properties.get_all(interface(object)).await.ok();
        let value = all.as_ref().and_then(&read);
        if shown.as_ref() != Some(&value) {
            if !send(value.clone()) {
                return Ok(());
            }
            shown = Some(value);
        }
        tokio::select! {
            change = changes.next() => if change.is_none() { bail!("no more PropertiesChanged") },
            owner = owners.next() => if owner.is_none() { bail!("no more NameOwnerChanged") },
        }
    }
}

/// The property `name` as a `T`, if there is one of that type.
pub fn get<T: TryFrom<OwnedValue>>(all: &Properties, name: &str) -> Option<T> {
    T::try_from(all.get(name)?.try_clone().ok()?).ok()
}

fn interface(object: Object) -> InterfaceName<'static> {
    InterfaceName::from_static_str_unchecked(object.interface)
}

/// Let a burst of changes settle: `SETTLE`, what `stream` sends meanwhile swallowed (the
/// state is read again after). `false` if it ended.
pub async fn settle<S: futures_lite::Stream + Unpin>(stream: &mut S) -> bool {
    let wait = time::sleep(SETTLE);
    tokio::pin!(wait);
    loop {
        tokio::select! {
            () = &mut wait => return true,
            item = stream.next() => if item.is_none() { return false },
        }
    }
}
