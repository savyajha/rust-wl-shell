//! The bar's right-side items, each a `popover::Item`: the battery (with the power mode)
//! and the keyboard layout. Each is hidden while it has nothing to show.

use std::rc::Rc;
use std::time::Duration;

use gtk4::{self as gtk, prelude::*};
use shell_core::power_profiles::Profiles;
use shell_core::upower::{Battery, State};
use shell_core::{niri, xkb};
use tokio::sync::mpsc::UnboundedSender;

use crate::blur::Blur;
use crate::popover::{Circle, Item, Row};

pub struct BatteryItem {
    pub item: Item,
    icon: gtk::Image,
    percent: gtk::Label,
    /// The popover's lines: the percentage, the state, the time left.
    lines: [gtk::Label; 3],
    /// After them, as macOS has it in its battery menu; `None` when the config leaves it
    /// out.
    power_mode: Option<PowerMode>,
}

/// The power profiles to choose from: a title, a row per profile, and why performance is
/// limited when it is; hidden without power-profiles-daemon.
struct PowerMode {
    section: gtk::Box,
    rows: gtk::Box,
    degraded: gtk::Label,
    /// The profile to set.
    requests: UnboundedSender<String>,
}

impl BatteryItem {
    pub fn new(
        show_percent: bool,
        profiles: Option<UnboundedSender<String>>,
        blur: &Option<Rc<Blur>>,
    ) -> Self {
        let face = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let (icon, percent) = (gtk::Image::new(), gtk::Label::new(None));
        percent.set_visible(show_percent);
        face.append(&icon);
        face.append(&percent);
        let item = Item::new(&face, "Battery", blur);
        let lines = [(); 3].map(|()| gtk::Label::builder().xalign(0.0).build());
        lines[0].add_css_class("title");
        for line in &lines {
            line.add_css_class("line");
            item.content.append(line);
        }
        let power_mode = profiles.map(|requests| {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let title = gtk::Label::builder()
                .label("Power Mode")
                .xalign(0.0)
                .build();
            title.add_css_class("title");
            let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let degraded = gtk::Label::builder().xalign(0.0).wrap(true).build();
            degraded.add_css_class("line");
            section.append(&title);
            section.append(&rows);
            section.append(&degraded);
            section.add_css_class("section");
            section.set_visible(false);
            item.content.append(&section);
            PowerMode {
                section,
                rows,
                degraded,
                requests,
            }
        });
        Self {
            item,
            icon,
            percent,
            lines,
            power_mode,
        }
    }

    pub fn show(&self, battery: Option<&Battery>) {
        self.item.button.set_visible(battery.is_some());
        let Some(battery) = battery else {
            self.item.popover.popdown();
            return;
        };
        let percent = format!("{}%", battery.percent);
        let state = match battery.state {
            State::Charging => "Charging",
            State::Discharging => "On battery",
            State::FullyCharged => "Fully charged",
            State::NotCharging => "Not charging",
        };
        let time = match (battery.state, battery.time_to_empty, battery.time_to_full) {
            (State::Discharging, Some(left), _) => format!("{} remaining", hours(left)),
            (State::Charging, _, Some(left)) => format!("{} until full", hours(left)),
            _ => String::new(),
        };
        self.icon.set_icon_name(Some(&battery_icon(battery)));
        self.percent.set_label(&percent);
        let name = format!("Battery {percent}, {}", state.to_lowercase());
        let button = &self.item.button;
        button.update_property(&[gtk::accessible::Property::Label(&name)]);
        for (line, text) in self.lines.iter().zip([&percent, state, &time]) {
            line.set_label(text);
            line.set_visible(!text.is_empty());
        }
    }

    /// The power profiles (`None`: no power-profiles-daemon, and no section).
    pub fn show_profiles(&self, profiles: Option<&Profiles>) {
        let Some(mode) = &self.power_mode else {
            return;
        };
        mode.section.set_visible(profiles.is_some());
        let Some(profiles) = profiles else {
            return;
        };
        let rows: Vec<Row> = profiles
            .offered
            .iter()
            .map(|profile| Row {
                label: profile_name(profile),
                circle: Circle::Icon(profile_icon(profile)),
                chosen: *profile == profiles.active,
                after: None,
            })
            .collect();
        let (requests, offered) = (mode.requests.clone(), profiles.offered.clone());
        let choose = move |i: usize| {
            // Fails only once the core has stopped, and the shell with it.
            let _ = requests.send(offered[i].clone());
        };
        self.item.rows(&mode.rows, &rows, false, choose);
        let why = profiles
            .degraded
            .as_deref()
            .map(|why| why.replace('-', " "));
        let why = why.map(|why| format!("Performance is limited: {why}"));
        mode.degraded.set_label(why.as_deref().unwrap_or(""));
        mode.degraded.set_visible(why.is_some());
    }
}

pub struct LayoutItem {
    pub item: Item,
    short: gtk::Label,
    rows: gtk::Box,
    niri: UnboundedSender<niri::Command>,
    shown: (Vec<String>, u8),
}

impl LayoutItem {
    pub fn new(niri: UnboundedSender<niri::Command>, blur: &Option<Rc<Blur>>) -> Self {
        let short = gtk::Label::new(None);
        let item = Item::new(&short, "Keyboard layout", blur);
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        item.content.append(&rows);
        Self {
            item,
            short,
            rows,
            niri,
            shown: (Vec::new(), 0),
        }
    }

    /// niri's `layouts`, `current` among them; shown only with two or more.
    pub fn show(&mut self, layouts: &[String], current: u8) {
        if (layouts, current) == (&self.shown.0[..], self.shown.1) {
            return;
        }
        self.shown = (layouts.to_vec(), current);
        self.item.button.set_visible(layouts.len() > 1);
        let Some(name) = layouts.get(current as usize) else {
            return;
        };
        // Upper case, as macOS shows it: "DE".
        self.short.set_label(&xkb::short_name(name).to_uppercase());
        let label = format!("Keyboard layout: {name}");
        let button = &self.item.button;
        button.update_property(&[gtk::accessible::Property::Label(&label)]);
        let niri = self.niri.clone();
        let choose = move |i: usize| {
            let _ = niri.send(niri::Command::SwitchLayout(i as u8));
        };
        let rows: Vec<Row> = layouts
            .iter()
            .enumerate()
            .map(|(i, layout)| Row {
                label: layout.clone(),
                circle: Circle::Text(xkb::short_name(layout).to_uppercase()),
                chosen: i == current as usize,
                after: None,
            })
            .collect();
        self.item.rows(&self.rows, &rows, false, choose);
    }
}

/// The theme's battery icon: by tens, charging or not; full and on mains, charged.
fn battery_icon(battery: &Battery) -> String {
    let level = (u32::from(battery.percent) + 5) / 10 * 10;
    let on_mains = battery.state != State::Discharging;
    if battery.state == State::FullyCharged || (level == 100 && on_mains) {
        return "battery-level-100-charged-symbolic".into();
    }
    let charging = if battery.state == State::Charging {
        "-charging"
    } else {
        ""
    };
    format!("battery-level-{level}{charging}-symbolic")
}

fn profile_icon(profile: &str) -> &'static str {
    match profile {
        "performance" => "power-profile-performance-symbolic",
        "power-saver" => "power-profile-power-saver-symbolic",
        _ => "power-profile-balanced-symbolic",
    }
}

fn profile_name(profile: &str) -> String {
    match profile {
        "performance" => "Performance",
        "balanced" => "Balanced",
        "power-saver" => "Power Saver",
        other => other,
    }
    .into()
}

/// "1:05", from a duration.
fn hours(time: Duration) -> String {
    let minutes = time.as_secs() / 60;
    format!("{}:{:02}", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn battery(percent: u8, state: State) -> Battery {
        Battery {
            percent,
            state,
            time_to_empty: None,
            time_to_full: None,
        }
    }

    #[test]
    fn battery_icons_are_the_themes() {
        let icon = |percent, state| battery_icon(&battery(percent, state));
        assert_eq!(icon(57, State::Discharging), "battery-level-60-symbolic");
        assert_eq!(
            icon(54, State::Charging),
            "battery-level-50-charging-symbolic"
        );
        assert_eq!(icon(3, State::Discharging), "battery-level-0-symbolic");
        assert_eq!(
            icon(98, State::Charging),
            "battery-level-100-charged-symbolic"
        );
        assert_eq!(icon(97, State::Discharging), "battery-level-100-symbolic");
        assert_eq!(
            icon(80, State::FullyCharged),
            "battery-level-100-charged-symbolic"
        );
        assert_eq!(icon(40, State::NotCharging), "battery-level-40-symbolic");
    }

    #[test]
    fn names_and_times() {
        assert_eq!(profile_name("power-saver"), "Power Saver");
        assert_eq!(hours(Duration::from_secs(3900)), "1:05");
        assert_eq!(hours(Duration::from_secs(59)), "0:00");
    }
}
