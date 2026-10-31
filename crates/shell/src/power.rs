//! The power menu, after the clock: Lock Screen, Sleep, Restart…, Shut Down… and Log
//! Out…, each shown when logind offers it (logging out always). The three with "…" ask
//! first, in the dialog, and come by themselves when its countdown ends.

use gtk4::{self as gtk, prelude::*};
use shell_core::power::{Action, Power, Request};
use tokio::sync::mpsc::UnboundedSender;

use crate::popover::{Circle, Item, Row};

pub struct PowerItem {
    pub item: Item,
    rows: gtk::Box,
    requests: UnboundedSender<Request>,
}

impl PowerItem {
    pub fn new(requests: UnboundedSender<Request>) -> Self {
        let icon = gtk::Image::from_icon_name("system-shutdown-symbolic");
        let item = Item::new(&icon, "Power");
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        item.content.append(&rows);
        let item = Self {
            item,
            rows,
            requests,
        };
        item.show(None);
        item
    }

    pub fn show(&self, power: Option<&Power>) {
        self.item.button.set_visible(true);
        let mut actions = Vec::new();
        if let Some(power) = power {
            actions.push((Action::Lock, "Lock Screen", "system-lock-screen-symbolic"));
            if power.suspend {
                actions.push((Action::Suspend, "Sleep", "weather-clear-night-symbolic"));
            }
            if power.reboot {
                actions.push((Action::Reboot, "Restart\u{2026}", "system-reboot-symbolic"));
            }
            if power.power_off {
                actions.push((
                    Action::PowerOff,
                    "Shut Down\u{2026}",
                    "system-shutdown-symbolic",
                ));
            }
        }
        actions.push((Action::LogOut, "Log Out\u{2026}", "system-log-out-symbolic"));
        let rows: Vec<Row> = actions
            .iter()
            .map(|(_, label, icon)| Row {
                label: label.to_string(),
                circle: Circle::Icon(icon),
                chosen: false,
                after: None,
            })
            .collect();
        let requests = self.requests.clone();
        self.item.rows(&self.rows, &rows, false, move |i| {
            // Those with "…" are asked about in the dialog, once their blockers are known.
            let request = match actions[i].0 {
                action @ (Action::Lock | Action::Suspend) => Request::Act(action),
                action => Request::Ask(action),
            };
            let _ = requests.send(request);
        });
    }
}
