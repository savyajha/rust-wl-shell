# rust-wl-shell

A macOS-style top bar for the [niri](https://github.com/YaLTeR/niri) Wayland compositor,
in GTK4 and [relm4](https://relm4.org).

> **Early work in progress.** It shows a bar on every output: the output's workspaces as
> dots and the focused app's name on the left; the keyboard layout, the power profile,
> the battery and the clock on the right, each item opening its own popover, all over
> the compositor's blur. More items, notifications and the tray are to come.

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
| `dot-size` | 6 at 24 | A workspace dot's diameter in pixels. |
| `clock-format` | `"%a %-d %b %H:%M"` | The clock, as a [chrono strftime](https://docs.rs/chrono/latest/chrono/format/strftime/index.html) format (`Mon 27 Oct 09:41`). |
| `position` | `"top"` | `"top"`, or `"bottom"` to run it next to another bar at the top. |
| `workspaces` | `true` | Show the output's workspaces. |
| `app-name` | `true` | Show the focused window's app name. |
| `battery` | `true` | Show the battery (UPower's), when there is one. |
| `battery-percentage` | `false` | Show the battery's percentage next to its icon. |
| `power-profile` | `true` | Show the power profile (power-profiles-daemon's), when the daemon runs. |
| `keyboard-layout` | `true` | Show the keyboard layout (niri's), when there are two or more. |
| `volume` | `true` | Show the volume (the default output's, through PipeWire's pulse server or PulseAudio). |
| `brightness` | `true` | Show the backlight's brightness, when there is a backlight. |
| `osd` | `true` | Show the volume or brightness on screen when it changes from outside the shell. |
| `backlight` | the first | The backlight device to follow, by its name in `/sys/class/backlight`. |

The four sizes follow the height unless set, in macOS's proportions at 24 px; each must be
at most the height. The clock is redrawn on each minute and after a resume from sleep.

The items on the right, right to left after the clock: the battery, the volume, the
brightness, the power profile, the keyboard layout. Each is a symbolic icon from the icon theme (the layout is its short
name, as GNOME has it, in upper case: "EN", "DE") and opens a popover under it: the battery's percentage, state and time
left; the volume's slider, a mute check and the outputs to choose from; the brightness's
slider; the power profiles to choose from; the layouts to choose from. A click outside or
Escape closes it. The brightness is set through logind, which needs no permission.

The OSD, a pill near the bottom of the focused output, shows the volume or brightness
for 1.5 s when it changes from outside the shell (the keys, wpctl, brightnessctl), not
when the shell's own slider moves it.

The text is in the system font (GTK's `gtk-font-name`, from GSettings). The look is fixed
for now: a translucent dark tint, white text, and the compositor's blur behind it, which
needs niri's `ext-background-effect-v1` (a `layer-rule` for the namespace
`rust-wl-shell` can set its options).

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

`settings` holds the config's keys; the config is checked with `--check` when the
configuration is built, so a typo fails the build. The module adds a user unit,
`rust-wl-shell.service`, started with `graphical-session.target` (after niri is ready)
and restarted 3 s after it fails.

## Building and testing

```sh
nix build                      # the package
nix develop -c cargo test      # the unit tests
nix flake check -L             # fmt, clippy, and the NixOS VM test (KVM)
```

See DESIGN.md for how it works and what the tests check.

## License

MIT.
