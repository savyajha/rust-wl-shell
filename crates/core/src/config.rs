use std::fs;
use std::path::Path;

use anyhow::{Context, ensure};
use chrono::format::{Item, StrftimeItems};

/// macOS's menu bar clock: `Mon 27 Oct 09:41`.
const DEFAULT_CLOCK_FORMAT: &str = "%a %-d %b %H:%M";
const DEFAULT_HEIGHT: u32 = 24;
const HEIGHTS: std::ops::RangeInclusive<u32> = 16..=64;

#[derive(knuffel::Decode, Clone, Debug, PartialEq)]
pub struct Config {
    /// The bar's height in logical pixels; the sizes left unset follow it. A minimum: a
    /// font too large for it makes the bar taller.
    #[knuffel(child, unwrap(argument), default = DEFAULT_HEIGHT)]
    pub height: u32,
    #[knuffel(child, unwrap(argument))]
    pub font_size: Option<u32>,
    #[knuffel(child, unwrap(argument))]
    pub icon_size: Option<u32>,
    #[knuffel(child, unwrap(argument))]
    pub spacing: Option<u32>,
    /// A strftime format, as chrono reads it.
    #[knuffel(child, unwrap(argument), default = DEFAULT_CLOCK_FORMAT.into())]
    pub clock_format: String,
    #[knuffel(child, unwrap(argument), default)]
    pub position: Position,
    #[knuffel(child, unwrap(argument), default = true)]
    pub workspaces: bool,
    #[knuffel(child, unwrap(argument), default = true)]
    pub app_name: bool,
    #[knuffel(child, unwrap(argument), default = true)]
    pub battery: bool,
    /// The battery's percentage next to its icon (macOS leaves it off).
    #[knuffel(child, unwrap(argument), default = false)]
    pub battery_percentage: bool,
    /// The power modes in the battery's popover.
    #[knuffel(child, unwrap(argument), default = true)]
    pub power_profile: bool,
    /// Shown only with two layouts or more.
    #[knuffel(child, unwrap(argument), default = true)]
    pub keyboard_layout: bool,
    #[knuffel(child, unwrap(argument), default = true)]
    pub volume: bool,
    #[knuffel(child, unwrap(argument), default = true)]
    pub brightness: bool,
    /// The level shown on screen when volume or brightness change from outside the shell.
    #[knuffel(child, unwrap(argument), default = true)]
    pub osd: bool,
    /// The network (Wi-Fi or wired), from NetworkManager.
    #[knuffel(child, unwrap(argument), default = true)]
    pub network: bool,
    /// Shown only when NetworkManager has VPN connections.
    #[knuffel(child, unwrap(argument), default = true)]
    pub vpn: bool,
    /// The shell answers NetworkManager's own requests for secrets (off: another agent
    /// does).
    #[knuffel(child, unwrap(argument), default = true)]
    pub secret_agent: bool,
    #[knuffel(child, unwrap(argument), default = true)]
    pub bluetooth: bool,
    /// The night light: switching the user unit `night-light-unit` on and off.
    #[knuffel(child, unwrap(argument), default = true)]
    pub night_light: bool,
    #[knuffel(child, unwrap(argument), default = "wlsunset.service".into())]
    pub night_light_unit: String,
    /// The notification server; off, the shell leaves the name to another.
    #[knuffel(child, unwrap(argument), default = true)]
    pub notifications: bool,
    /// Where banners show on the focused output.
    #[knuffel(child, unwrap(argument), default)]
    pub notification_position: Corner,
    /// The longest, in seconds, a banner that asks to stay ("never expires") stays; only
    /// critical ones stay until dismissed.
    #[knuffel(child, unwrap(argument), default = 10)]
    pub notification_max_timeout: u32,
    /// The dialog, the banners and the OSD left to a niri `layer-rule` for their blur (its
    /// corners smooth), rather than blurred by the shell (README).
    #[knuffel(child, unwrap(argument), default = false)]
    pub niri_blur_rule: bool,
    /// The power menu, after the clock.
    #[knuffel(child, unwrap(argument), default = true)]
    pub power: bool,
    /// The seconds a restart, shut down or log out waits in its dialog before it acts.
    #[knuffel(child, unwrap(argument), default = 60)]
    pub power_countdown: u32,
    /// Keeping the machine awake: holding an idle inhibitor.
    #[knuffel(child, unwrap(argument), default = true)]
    pub keep_awake: bool,
    /// The backlight device (in /sys/class/backlight), else the first.
    #[knuffel(child, unwrap(argument))]
    pub backlight: Option<String>,
}

#[derive(knuffel::DecodeScalar, Clone, Copy, Debug, Default, PartialEq)]
pub enum Position {
    #[default]
    Top,
    /// Only while it runs next to another bar at the top.
    Bottom,
}

#[derive(knuffel::DecodeScalar, Clone, Copy, Debug, Default, PartialEq)]
pub enum Corner {
    TopLeft,
    Top,
    #[default]
    TopRight,
    BottomLeft,
    Bottom,
    BottomRight,
}

/// The bar's sizes in logical pixels, after the defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sizes {
    pub height: u32,
    pub font: u32,
    pub icon: u32,
    pub spacing: u32,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&path.display().to_string(), &text)
            .with_context(|| format!("invalid config {}", path.display()))
    }

    /// Parse and validate KDL `text`; `name` labels it in errors.
    pub fn parse(name: &str, text: &str) -> anyhow::Result<Self> {
        let config: Self = knuffel::parse(name, text).map_err(render)?;
        ensure!(
            HEIGHTS.contains(&config.height),
            "height must be {} to {}",
            HEIGHTS.start(),
            HEIGHTS.end()
        );
        let sizes = config.sizes();
        for (key, size) in [
            ("font-size", sizes.font),
            ("icon-size", sizes.icon),
            ("spacing", sizes.spacing),
        ] {
            ensure!(
                (1..=config.height).contains(&size),
                "{key} must be 1 to the height ({})",
                config.height
            );
        }
        let path = config
            .backlight
            .as_deref()
            .is_some_and(|name| name.contains('/'));
        ensure!(
            !path,
            "backlight is a device's name in /sys/class/backlight, not a path"
        );
        let invalid = StrftimeItems::new(&config.clock_format).any(|item| item == Item::Error);
        ensure!(!invalid, "clock-format is not a valid strftime format");
        Ok(config)
    }

    /// The sizes: those set, else macOS's proportions at 24 px (text 13, icons 16, spacing
    /// 8) scaled to the height.
    pub fn sizes(&self) -> Sizes {
        let scaled = |at_24: u32| (self.height * at_24 + 12) / 24;
        Sizes {
            height: self.height,
            font: self.font_size.unwrap_or_else(|| scaled(13)),
            icon: self.icon_size.unwrap_or_else(|| scaled(16)),
            spacing: self.spacing.unwrap_or_else(|| scaled(8)),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::parse("defaults", "").expect("the defaults are valid")
    }
}

fn render(err: knuffel::Error) -> anyhow::Error {
    let mut text = String::new();
    let _ = miette::NarratableReportHandler::new().render_report(&mut text, &err);
    anyhow::anyhow!("{}", text.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_err(text: &str) -> String {
        format!("{:#}", Config::parse("test.kdl", text).unwrap_err())
    }

    #[test]
    fn empty_config_has_the_defaults() {
        let config = Config::parse("test.kdl", "").unwrap();
        assert_eq!(config.clock_format, "%a %-d %b %H:%M");
        assert_eq!(config.position, Position::Top);
        assert!(config.workspaces && config.app_name);
        assert!(config.battery && config.power_profile && config.keyboard_layout);
        assert!(!config.battery_percentage);
        assert!(config.volume && config.brightness && config.osd);
        assert_eq!(config.backlight, None);
        let sizes = Sizes {
            height: 24,
            font: 13,
            icon: 16,
            spacing: 8,
        };
        assert_eq!(config.sizes(), sizes);
    }

    #[test]
    fn sizes_follow_the_height_unless_set() {
        let config = Config::parse("test.kdl", "height 30\nicon-size 20\n").unwrap();
        let sizes = Sizes {
            height: 30,
            font: 16,
            icon: 20,
            spacing: 10,
        };
        assert_eq!(config.sizes(), sizes);
    }

    #[test]
    fn every_key_parses() {
        let text = r#"
            height 28
            font-size 14
            icon-size 18
            spacing 6
            clock-format "%A, %-d %b %H:%M"
            position "bottom"
            workspaces false
            app-name false
            battery false
            battery-percentage true
            power-profile false
            keyboard-layout false
            volume false
            brightness false
            osd false
            network false
            vpn false
            secret-agent false
            bluetooth false
            night-light false
            night-light-unit "gammastep.service"
            keep-awake false
            notifications false
            notification-position "bottom-left"
            notification-max-timeout 20
            power false
            power-countdown 5
            niri-blur-rule true
            backlight "amdgpu_bl1"
        "#;
        let config = Config::parse("test.kdl", text).unwrap();
        assert_eq!(
            config,
            Config {
                height: 28,
                font_size: Some(14),
                icon_size: Some(18),
                spacing: Some(6),
                clock_format: "%A, %-d %b %H:%M".into(),
                position: Position::Bottom,
                workspaces: false,
                app_name: false,
                battery: false,
                battery_percentage: true,
                power_profile: false,
                keyboard_layout: false,
                volume: false,
                brightness: false,
                osd: false,
                network: false,
                vpn: false,
                secret_agent: false,
                bluetooth: false,
                night_light: false,
                night_light_unit: "gammastep.service".into(),
                keep_awake: false,
                notifications: false,
                notification_position: Corner::BottomLeft,
                notification_max_timeout: 20,
                power: false,
                power_countdown: 5,
                niri_blur_rule: true,
                backlight: Some("amdgpu_bl1".into()),
            }
        );
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = parse_err("height 24\nhieght 30\n");
        assert!(err.contains("unexpected node `hieght`"), "{err}");
    }

    #[test]
    fn invalid_values_are_rejected() {
        for (text, want) in [
            ("height 8", "height must be 16 to 64"),
            ("height 65", "height must be 16 to 64"),
            ("height -1", "invalid value"),
            ("font-size 0", "font-size must be 1 to the height (24)"),
            (
                "height 20\nspacing 21",
                "spacing must be 1 to the height (20)",
            ),
            ("position \"left\"", "expected one of `top`, `bottom`"),
            ("workspaces \"no\"", "expected boolean"),
            ("backlight \"../x\"", "not a path"),
            (
                "clock-format \"%Q\"",
                "clock-format is not a valid strftime",
            ),
            (
                "clock-format",
                "additional argument `clock_format` is required",
            ),
        ] {
            let err = parse_err(text);
            assert!(err.contains(want), "{text:?}: {err}");
        }
    }
}
