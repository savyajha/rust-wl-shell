//! The network's items: Wi-Fi (or wired) and VPN, as macOS's menus have them.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};
use shell_core::network::{
    Network, Primary, Request, Secret, Security, Vpn, VpnState, WifiNetwork,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::dialog::Dialog;
use crate::popover::{Circle, Item, Row, Switch, title};

/// The primary connection's icon; its popover has the Wi-Fi switch, a wired connection
/// when there is one, the known networks in range, and the others under "Other Networks".
/// A scan is asked for each time it opens.
pub struct NetworkItem {
    pub item: Item,
    icon: gtk::Image,
    wifi: Switch,
    wired: gtk::Box,
    known: gtk::Box,
    other: gtk::Expander,
    others: gtk::Box,
    shown: RefCell<Option<Network>>,
    requests: UnboundedSender<Request>,
    dialog: Rc<Dialog>,
}

impl NetworkItem {
    pub fn new(requests: UnboundedSender<Request>, dialog: Rc<Dialog>) -> Self {
        let icon = gtk::Image::new();
        let item = Item::new(&icon, "Network");
        let sent = requests.clone();
        let wifi = Switch::new("Wi-Fi", move |on| {
            let _ = sent.send(Request::Wifi(on));
        });
        let [wired, known, others] = [(); 3].map(|()| gtk::Box::new(gtk::Orientation::Vertical, 0));
        let other = gtk::Expander::builder()
            .label("Other Networks")
            .child(&others)
            .build();
        other.add_css_class("section");
        // The networks scroll, under the switch, as macOS's do.
        let networks = gtk::Box::new(gtk::Orientation::Vertical, 0);
        networks.append(&known);
        networks.append(&other);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&networks)
            .build();
        for widget in [
            wired.upcast_ref(),
            wifi.row.upcast_ref(),
            scrolled.upcast_ref::<gtk::Widget>(),
        ] {
            item.content.append(widget);
        }
        let sent = requests.clone();
        item.popover.connect_show(move |popover| {
            let _ = sent.send(Request::Scan);
            // At most half its output's height.
            let surface = popover.root().and_then(|root| root.surface());
            let monitor = surface.and_then(|s| s.display().monitor_at_surface(&s));
            scrolled.set_max_content_height(monitor.map_or(400, |m| m.geometry().height() / 2));
        });
        Self {
            item,
            icon,
            wifi,
            wired,
            known,
            other,
            others,
            shown: RefCell::default(),
            requests,
            dialog,
        }
    }

    pub fn show(&self, network: Option<&Network>) {
        let Some(network) = self.item.state(network) else {
            return;
        };
        let before = self.shown.replace(Some(network.clone()));
        if before.as_ref() == Some(network) {
            return;
        }
        // Each list of rows rebuilt only when what it shows changed (not, say, a signal
        // within its icon's level), so that a row is not replaced under the pointer.
        let before = before.as_ref().map(lists);
        let now = lists(network);
        let (icon, label) = match &network.primary {
            Primary::Wired => ("network-wired-symbolic", "Network: wired".into()),
            Primary::Wifi(strength, ssid) => (signal_icon(*strength), format!("Wi-Fi: {ssid}")),
            Primary::Connecting => (
                "network-wireless-acquiring-symbolic",
                "Network: connecting".into(),
            ),
            Primary::Offline if network.wifi_present => (
                "network-wireless-offline-symbolic",
                "Network: offline".into(),
            ),
            Primary::Offline => ("network-offline-symbolic", "Network: offline".into()),
        };
        self.icon.set_icon_name(Some(icon));
        self.item.set_name(&label);

        self.wifi.row.set_visible(network.wifi_present);
        self.wifi.show(network.wifi_enabled);
        let listed = network.wifi_present && network.wifi_enabled;
        let (known, others): (Vec<&WifiNetwork>, _) =
            network.networks.iter().partition(|n| n.known.is_some());
        self.known.set_visible(listed);
        self.other.set_visible(listed && !others.is_empty());
        let changed = |i: usize| before.as_ref().is_none_or(|b| b[i] != now[i]);
        if changed(0) {
            let wired: Vec<Row> = network
                .wired
                .iter()
                .map(|name| Row {
                    label: name.clone(),
                    circle: Circle::Icon("network-wired-symbolic"),
                    chosen: true,
                    after: None,
                })
                .collect();
            self.item.rows(&self.wired, &wired, false, |_| {});
        }
        if changed(1) {
            let requests = self.requests.clone();
            let uuids: Vec<String> = known.iter().filter_map(|n| n.known.clone()).collect();
            self.item.rows(&self.known, &rows(&known), false, move |i| {
                let _ = requests.send(Request::Activate(uuids[i].clone()));
            });
        }
        if changed(2) {
            let (requests, dialog) = (self.requests.clone(), self.dialog.clone());
            let joined: Vec<(String, Security)> = others
                .iter()
                .map(|n| (n.ssid.clone(), n.security))
                .collect();
            self.item
                .rows(&self.others, &rows(&others), false, move |i| {
                    let (ssid, security) = joined[i].clone();
                    match security {
                        Security::Open => {
                            let secret = Secret::Open;
                            let _ = requests.send(Request::Join { ssid, secret });
                        }
                        _ => dialog.ask_join(ssid, security),
                    }
                });
        }
    }
}

/// A row as it shows and as a click on it acts: its name, icon, whether it is current,
/// its security and its saved connection.
type RowKey = (String, &'static str, bool, Security, Option<String>);

/// What the wired row, the known networks' rows and the others' show, in that order.
fn lists(network: &Network) -> [Vec<RowKey>; 3] {
    let row = |n: &WifiNetwork| {
        let icon = signal_icon(n.strength);
        (n.ssid.clone(), icon, n.current, n.security, n.known.clone())
    };
    let wired = network
        .wired
        .iter()
        .map(|name| (name.clone(), "", true, Security::Open, None));
    let (known, others): (Vec<&WifiNetwork>, _) =
        network.networks.iter().partition(|n| n.known.is_some());
    [
        wired.collect(),
        known.into_iter().map(row).collect(),
        others.into_iter().map(row).collect(),
    ]
}

/// Wi-Fi networks' rows: the signal, chosen when current, and a lock when secured.
fn rows(networks: &[&WifiNetwork]) -> Vec<Row> {
    networks
        .iter()
        .map(|n| Row {
            label: n.ssid.clone(),
            circle: Circle::Icon(signal_icon(n.strength)),
            chosen: n.current,
            after: (n.security != Security::Open)
                .then_some(Circle::Icon("network-wireless-encrypted-symbolic")),
        })
        .collect()
}

/// The theme's Wi-Fi icon for a signal, in four levels.
fn signal_icon(strength: u8) -> &'static str {
    match strength {
        75.. => "network-wireless-signal-excellent-symbolic",
        50.. => "network-wireless-signal-good-symbolic",
        25.. => "network-wireless-signal-ok-symbolic",
        _ => "network-wireless-signal-weak-symbolic",
    }
}

/// The VPN connections: hidden when there are none; a green tick on the icon's corner while
/// one is on. A click on one switches it on or off.
pub struct VpnItem {
    pub item: Item,
    badge: gtk::Image,
    rows: gtk::Box,
    shown: RefCell<Vec<Vpn>>,
    requests: UnboundedSender<Request>,
}

impl VpnItem {
    pub fn new(requests: UnboundedSender<Request>) -> Self {
        let face = gtk::Overlay::builder()
            .child(&gtk::Image::from_icon_name("network-vpn-symbolic"))
            .build();
        let badge = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .css_classes(["vpn-badge"])
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .visible(false)
            .build();
        face.add_overlay(&badge);
        let item = Item::new(&face, "VPN");
        let title = title("VPN");
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        item.content.append(&title);
        item.content.append(&rows);
        Self {
            item,
            badge,
            rows,
            shown: RefCell::default(),
            requests,
        }
    }

    pub fn show(&self, network: Option<&Network>) {
        let vpns = network.map(|n| n.vpns.clone()).unwrap_or_default();
        self.item.state((!vpns.is_empty()).then_some(()));
        if *self.shown.borrow() == vpns {
            return;
        }
        self.shown.replace(vpns.clone());
        let on = vpns.iter().filter(|v| v.state != VpnState::Off).count();
        self.badge.set_visible(on > 0);
        let label = if on > 0 { "VPN: on" } else { "VPN: off" };
        self.item.set_name(label);
        let rows: Vec<Row> = vpns
            .iter()
            .map(|v| Row {
                label: v.name.clone(),
                circle: Circle::Icon("network-vpn-symbolic"),
                chosen: v.state != VpnState::Off,
                after: (v.state == VpnState::Connecting)
                    .then(|| Circle::Text("Connecting…".into())),
            })
            .collect();
        let requests = self.requests.clone();
        self.item.rows(&self.rows, &rows, true, move |i| {
            let vpn = &vpns[i];
            let request = match vpn.state {
                VpnState::Off => Request::Activate(vpn.uuid.clone()),
                _ => Request::Deactivate(vpn.uuid.clone()),
            };
            let _ = requests.send(request);
        });
    }
}
