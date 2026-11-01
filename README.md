# rust-wl-shell

A macOS-style top bar for the [niri](https://github.com/YaLTeR/niri) Wayland compositor,
in GTK4 and [relm4](https://relm4.org).

> **Early work in progress.** It shows a bar on every output: the output's workspaces as
> pills and the focused app's name on the left; the keyboard layout, the brightness, the
> volume, VPN, Bluetooth, the network, the battery (with the power mode) and the clock
> on the right, each item opening its own popover (as GNOME apps' menus), the bar over
> the compositor's blur. More
> items, notifications and the tray are to come.

> **Please read before using**
>
> - This is a personal project, built for my own desktop. It is shared in case it is
>   useful to others, with no promise of support or stability.
> - It was written with the help of an AI assistant (Anthropic's Claude). Every change is
>   reviewed by me, and the behaviour is covered by the tests described in DESIGN.md, but
>   you should review it yourself before relying on it.
> - It works only with niri: workspaces and the focused window come from niri's IPC.

## Running it

```sh
rust-wl-shell                          # with the defaults
rust-wl-shell --config /path/to/config.kdl
rust-wl-shell --check /path/to/config.kdl
```

`--check` validates a config and exits. The shell needs `NIRI_SOCKET`, which niri sets
for its session, and exits with a failure if niri's IPC closes.

## Configuration

The config is [KDL](https://kdl.dev); every key is optional, and unknown keys and invalid
values are errors.

```kdl
height 24
clock-format "%a %-d %b %H:%M"
position "top"
workspaces true
app-name true
battery true
battery-percentage false
power-profile true
keyboard-layout true
volume true
brightness true
osd true
backlight "amdgpu_bl1"
```

| Key | Default | |
|---|---|---|
| `height` | `24` | The bar's height in logical pixels, 16 to 64; a minimum, as text too large for it makes the bar taller. |
| `font-size` | 13 at 24 | The text's size in pixels. |
| `icon-size` | 16 at 24 | Icons' size in pixels. |
| `spacing` | 8 at 24 | The space between items and at the bar's ends, in pixels. |
| `pill-size` | the icon size | A workspace's pill's height and width, at most the height. |
| `active-pill-width` | 11/8 of the pill | The shown workspace's pill's width. |
| `pill-gap` | 3/4 of the spacing | The space between workspaces' pills. |
| `workspace-animation` | `200` | A workspace switch's animation, in ms. |
| `popover-radius` | `15` | The popovers' corners' radius (libadwaita's). |
| `popover-gap` | `6` | The space between the bar and its popovers. |
| `panel-radius` | `14` | The banners' corners' radius (and their blur's). |
| `history-height` | `480` | The notifications' history's height before it scrolls. |
| `osd-margin` | `48` | The OSD's distance from the output's bottom edge. |
| `osd-timeout` | `1500` | How long the OSD shows after the last change, in ms. |
| `notification-gap` | `8` | The banners' distance from the output's edges and from each other. |
| `notification-width` | `360` | The banners' width. |
| `swipe-distance` | `100` | How far a notification is swiped before it is dismissed. |
| `clock-format` | `"%a %-d %b %H:%M"` | The clock, as a [chrono strftime](https://docs.rs/chrono/latest/chrono/format/strftime/index.html) format (`Mon 27 Oct 09:41`). |
| `position` | `"top"` | `"top"`, or `"bottom"` to run it next to another bar at the top. |
| `workspaces` | `true` | Show the output's workspaces: a pill (of the icon size) for each with windows and for the one shown, which is wider and filled with the accent; a click switches to it. |
| `app-name` | `true` | Show the focused window's app name. |
| `battery` | `true` | Show the battery (UPower's), when there is one. |
| `battery-percentage` | `false` | Show the battery's percentage next to its icon. |
| `power-profile` | `true` | Show the power mode (power-profiles-daemon's) in the battery's popover, when the daemon runs. |
| `keyboard-layout` | `true` | Show the keyboard layout (niri's), when there are two or more. |
| `volume` | `true` | Show the volume (the default output's, through PipeWire's pulse server or PulseAudio). |
| `brightness` | `true` | Show the backlight's brightness, when there is a backlight. |
| `osd` | `true` | Show the volume or brightness on screen when it changes from outside the shell. |
| `network` | `true` | Show the network (NetworkManager's): Wi-Fi, or a wired connection. |
| `vpn` | `true` | Show NetworkManager's VPN and WireGuard connections, when there are any. |
| `secret-agent` | `true` | Be NetworkManager's secret agent, answering its own requests for passwords in the dialog; off, another agent (a panel's) answers them, and the network items stay. |
| `bluetooth` | `true` | Show Bluetooth (BlueZ's), when there is an adapter. |
| `night-light` | `true` | Show the night light toggle, when its unit exists. |
| `night-light-unit` | `"wlsunset.service"` | The user unit the night light starts and stops (wlsunset, as home-manager's `services.wlsunset` runs it). |
| `keep-awake` | `true` | Show the keep awake toggle: while on, a logind idle inhibitor. |
| `power` | `true` | Show the power menu after the clock. |
| `power-countdown` | `60` | The seconds a restart, a shut down or a log out waits in its dialog before it happens. |
| `niri-blur-rule` | `false` | Leave the blur behind the banners and the OSD to a niri `layer-rule` (below), whose corners are smooth; without the rule they have none. |
| `notifications` | `true` | Be the notification server (`org.freedesktop.Notifications`); off, another one (a panel's) keeps the name, and the clock has no popover. |
| `notification-position` | `"top-right"` | Where banners show on the focused output: `"top-left"`, `"top"`, `"top-right"`, `"bottom-left"`, `"bottom"` or `"bottom-right"`. |
| `notification-max-timeout` | `10` | The longest, in seconds, a banner asked never to expire stays; only critical ones stay until dismissed. |
| `backlight` | the first | The backlight device to follow, by its name in `/sys/class/backlight`. |

The three sizes follow the height unless set, in macOS's proportions at 24 px; each must be
at most the height. The keys hold only what CSS cannot reach (the bar's height, which is
its exclusive zone, layer-shell margins, the sizes the shell animates, the radii its blur
follows, and timings); the rest of the look is in the stylesheet (below). The clock is redrawn on each minute and after a resume from sleep.
`dot-size` is gone: the workspaces' pills follow `icon-size`, and a config that sets it
fails `--check`.

The power menu is the rightmost item, after the clock: Lock Screen, Sleep, Restart…, Shut
Down… and Log Out… (each of the first four only when logind offers it). Restart, Shut
Down and Log Out ask first and happen by themselves after `power-countdown` seconds; the
dialog lists the apps that block them (logind's block inhibitors) and logind's error if
it refuses. Logging out quits niri.

The items on the right, right to left after the clock: the battery, the network,
Bluetooth, VPN, the volume, keep awake, the night light, the brightness, the keyboard
layout. Each is a symbolic icon
from the icon theme (the layout is its short name, as GNOME has it, in upper case: "EN",
"DE") and opens a popover under it: the battery's percentage, state and time left, and
the power modes to choose from; the Wi-Fi switch, the known networks in range and the
others; Bluetooth's switch and the paired devices, to connect or disconnect (pairing is
left to Settings); the VPN connections, to switch on or off; the volume's slider (its
speaker mutes) and the outputs (each sound card's available ports) to choose from; the
brightness's slider; the layouts to choose from. A click outside or Escape closes it.
The brightness is set through logind, which needs no permission. The night light and
keep awake are click toggles with no popover, their icons telling their state (the moon,
struck through when off; a cup, steaming when on), as VPN's does. While Do Not Disturb
is on, its icon shows beside the clock. The night
light starts and stops a user unit (wlsunset by default), whose state it shows, so
`systemctl --user stop wlsunset` turns it off too; off stays off until it is turned on
or the next login. Keep awake holds a logind inhibitor of idle, so that the idle lock,
the screens going off and suspend on idle wait (the lid, the power key and a lock asked
for still work); it is off whenever the shell starts.

A secured network asks for its password in a dialog in the middle of the focused output
(an enterprise network for a username too, and in "Options" its method and the domain
its server's certificate must be in). The shell is NetworkManager's secret agent (unless
`secret-agent` is off), so NetworkManager's own requests (a saved network whose password
changed, a VPN's password) open the same dialog. It looks as GNOME apps' dialogs do
(libadwaita's alert dialog, its rules ported to `adwaita.css`, loaded after the built-in
stylesheet), in the GTK theme's colours (`@dialog_bg_color`, `@dialog_fg_color`, the
accent, `@error_color`, as the theme or a gtk.css such as matugen's defines them, else
libadwaita's dark ones, from `fallback.css`, below the theme): opaque, with no
blur behind it, whatever `niri-blur-rule` says, so a niri rule for its namespace
(`rust-wl-shell-dialog`) is not needed. Its colours are variables too (`--dialog-bg`,
`--dialog-fg`, `--dialog-accent-bg` and the others in `window { }`). Changing the network needs the user to be allowed to by
NetworkManager's polkit rules (on NixOS, in the `networkmanager` group). With NM's iwd
backend, an enterprise network's server is checked against the system's CA bundle,
which iwd reads only if it holds plain certificates: on NixOS, set
`security.pki.useCompatibleBundle = true`.

Notifications show as banners on the focused output (top right by default), at most
three, newest nearest the corner, each for its app's timeout or 5 s (paused while the
pointer is over it); one that asks never to expire still leaves after
`notification-max-timeout`, unless it is critical. A click runs its default action, its
buttons their actions, × or a swipe sideways (a drag, or two fingers on a touchpad)
dismisses it. The clock opens the history, grouped by app, with × and "Clear all", and a
Do Not Disturb switch (no banners but critical ones); both are kept in
`$XDG_STATE_HOME/rust-wl-shell/notifications.json` across restarts.

The OSD, a pill near the bottom of the focused output, shows the volume or brightness
for 1.5 s when it changes from outside the shell (the keys, wpctl, brightnessctl), not
when the shell's own slider moves it.

The text is in the system font (GTK's `gtk-font-name`, from GSettings). The look is a
translucent dark tint, white text, and the compositor's blur behind it, which needs
niri's `ext-background-effect-v1` (a `layer-rule` for the namespace `rust-wl-shell` can
set its options).

### Stylesheet

The built-in stylesheet, [`style.css`](crates/shell/src/style.css), defines every colour,
spacing, radius and width once, as a variable in its `window { }` rule, and its rules use
them. A stylesheet of your own, `$XDG_CONFIG_HOME/rust-wl-shell/style.css`
(`~/.config/rust-wl-shell/style.css`), is loaded after it, at GTK's user priority, and
again whenever it changes (replaced as a symlink too). Overriding a variable is enough:

```css
window {
  --accent: #e01b24;
  --tint: rgba(0, 0, 0, 0.5);
  --radius-s: 4px;
}
```

A file GTK cannot parse in full is left out, its errors logged, and the built-in look
stays; one with only warnings (an empty declaration, `;;`) is applied, its warnings
logged. The accent defaults to GTK's (`@accent_bg_color`, which a gtk.css can set).

Do not override `--panel-radius` or `--radius-pill` in the stylesheet: the shell's blur
follows the config's radius, not the CSS, so its corners would no longer match. Set the
banners' radius with the config's `panel-radius`, and leave `--radius-pill` round.

The shell watches the name `style.css` in its directory: it sees the file written,
replaced, or renamed over (home-manager's symlink), but not an edit to the file a
symlink there points to elsewhere.

The shell's blur is clipped in whole logical pixels, so at a fractional scale its rounded
corners show steps past the drawn ones. niri can round the blur itself, but only when
the surface asks for none (it does not round a region a surface sets). With
`niri-blur-rule true`, the banners and the OSD ask for none, and these rules in niri's
config blur them with smooth corners (the bar keeps the shell's blur; the popovers and
the dialog are opaque, with no blur):

```kdl
layer-rule {
    match namespace="^rust-wl-shell-notification$"
    geometry-corner-radius 14
    background-effect { blur true; }
}
layer-rule {
    match namespace="^rust-wl-shell-osd$"
    geometry-corner-radius 999
    background-effect { blur true; }
}
```

The banners' radius is the config's default `panel-radius` (14).

## home-manager

The flake has a home-manager module, `homeManagerModules.default`:

```nix
{
  imports = [ inputs.rust-wl-shell.homeManagerModules.default ];
  programs.rust-wl-shell = {
    enable = true;
    settings = {
      clock-format = "%A, %-d %b %H:%M";
      position = "bottom";
    };
  };
}
```

`style` holds a stylesheet (CSS lines), written to `~/.config/rust-wl-shell/style.css`:

```nix
programs.rust-wl-shell.style = ''
  window { --accent: #e01b24; }
'';
```

`settings` holds the config's keys; the config is checked with `--check` when the
configuration is built, so a typo fails the build. The module adds a user unit,
`rust-wl-shell.service`, started with `graphical-session.target` (after niri is ready)
and restarted 3 s after it fails.

## Building and testing

```sh
nix build                      # the package
nix develop -c cargo test      # the unit tests
nix flake check -L             # fmt, clippy, and the NixOS VM tests (KVM)
```

See DESIGN.md for how it works and what the tests check.

## License

MIT (LICENSE), but for `crates/shell/src/adwaita.css`: the dialog's rules, ported from
libadwaita 1.9.3 and modified, are LGPL-2.1-or-later (LICENSES/LGPL-2.1-or-later.txt).
