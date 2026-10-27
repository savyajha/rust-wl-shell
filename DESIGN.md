# Design

rust-wl-shell is a macOS-style bar for niri: a bar on every output with the output's
workspaces and the focused app's name on the left; the keyboard layout, the power
profile, the battery and the clock on the right, each item with its own popover.

## Crates

- `crates/core` (`shell_core`): plain Rust, no GTK. The services (niri, the clock,
  UPower, power-profiles-daemon), the state they produce, and the config. It runs a current-thread tokio runtime on its own
  thread (`shell_core::start`).
- `crates/shell` (binary `rust-wl-shell`): the GTK4 + relm4 UI, on GTK's main loop.

The two talk over tokio channels with data only. The core sends `Update`s, each a whole
snapshot of one service (`Niri`, `Clock`, `Battery`, `Profiles`); the UI sends
`Command`s back (`Niri(FocusWorkspace | SwitchLayout)`, `SetProfile`), which the core
routes to the service's task. The UI awaits the update channel from a GLib future;
tokio's channels need no runtime on the receiving side.

## niri

`niri.rs` opens niri's socket (`NIRI_SOCKET`), asks for the event stream, and folds each
event into niri-ipc's own `EventStreamState` (the crate's version is pinned to niri's,
26.4.0). After each event it derives a `Snapshot`: every workspace (id, index, name,
output, whether it is active on its output), sorted by output and index, the focused
window's app id, and the keyboard layouts' names with the index of the one in use. A snapshot is sent only when it differs from the last one, so most
events (layouts, titles) send nothing. An event the crate cannot parse is logged and
skipped. Each command opens a new connection, sends its action and reads the reply.

When the event stream ends or fails, `niri::run` returns an error, the core's runtime
ends, the update channel closes, and the shell exits with a failure. systemd restarts it
(`Restart=on-failure`) unless the session itself is going away.

## The clock

`clock.rs` formats the local time with chrono and sleeps until the next minute starts.
tokio's timers run on the monotonic clock, which stops during suspend, so the clock also
wakes on logind's `PrepareForSleep` (both edges) and redraws then. The first text is
sent before the system bus is connected; if logind does not answer within 2 s, or the bus
fails, the clock logs a warning and keeps only its minute ticks. An NTP step or a time
zone change while awake shows only at the next minute. The format is validated when the config is loaded, as
chrono panics on an invalid one.

## The system bus

The core connects to the system bus once (`bus.rs`, 2 s at most, else it warns and goes
without), after the clock's first text; the clock, UPower and power profiles share the
connection. Both D-Bus services are read the same way, as the greeter reads UPower
(`bus::follow`): all the object's properties (`GetAll`), again on each
`PropertiesChanged` and each time the service's name changes owner, listening before the
first read. The `GetAll` starts a daemon that is not running through D-Bus activation:
on NixOS neither UPower nor power-profiles-daemon is started otherwise, so the shell
activates them itself. What the service's read makes of them is sent only when it changed; a
service not running is `None`, and the item hides.

- **UPower** (`upower.rs`): the `DisplayDevice` (all batteries as one): `IsPresent`,
  `Percentage`, `State` (charging, discharging, fully charged, or not charging on mains)
  and `TimeToEmpty`/`TimeToFull` when UPower estimates them.
- **power-profiles-daemon** (`power_profiles.rs`): `ActiveProfile`, the `Profiles` it
  offers (kept in the order performance, balanced, power saver) and
  `PerformanceDegraded`, under its name since 0.20 (`org.freedesktop.UPower.PowerProfiles`;
  the older `net.hadess` name is not followed). A choice sets `ActiveProfile` through
  `Properties.Set`.
- **Keyboard layouts** (`xkb.rs`): niri names a layout by its xkb description ("German");
  its short name is xkeyboard-config's `<shortDescription>` for that description in
  `evdev.xml` ("de"), as GNOME shows it, read once with plain text matching (the layout
  list has one element per line and no entities; a variant without a short name has its
  layout's). The file's path is set at build time (`XKB_EVDEV_XML`); without it, or for
  a layout it does not list, the short name is the description's first two letters.

## The bars

`shell.rs` keeps one bar per `gdk::Monitor` in the display's monitor list, and follows
its `items-changed`: a new monitor gets a bar, a removed one's bar is destroyed. Each
update goes to every bar; the latest of each kind is kept for bars added later. The app
id is mapped to its desktop file's `Name` once per niri update with
`gio::DesktopAppInfo` (the id as is, then in lower case), else shown as is, and sent to
the bars when it changes.

`bar.rs` is a relm4 component. Its window is a layer surface on the top layer, anchored
to the top (or bottom) and both sides, with an exclusive zone of its height and no
keyboard focus, namespace `rust-wl-shell`. Its output's name is the monitor's connector,
which is how niri names outputs. GDK can list a new monitor before its connector is
known (seen in the VM test when an output is switched back on), so the bar keeps every
output's workspaces and filters them again when the connector changes. The workspace dots are rebuilt when that output's
workspaces change: one `button.workspace` per workspace, holding a `.dot`, `.active`
when shown, labelled "Workspace N" (or its name) for AT-SPI, sending
`FocusWorkspace` when clicked.

The right side holds the items, then the clock; an item switched off in the config is
never built.

### Items and popovers

`popover.rs` is the component every item is built on: a `GtkMenuButton` (flat, with the
greeter's chip plate while hovered or open) whose `GtkPopover` GTK places under it,
closes on an outside click (so one is open at a time) and on Escape. The bar takes no
keyboard, so that clicks on it leave the focus with the window; while a popover is open
the bar's layer surface asks for the keyboard `OnDemand`, so that Escape reaches it,
and gives it back (`None`) when the popover closes. `OnDemand` rather than `Exclusive`:
if the keyboard were ever left asked for, a click on any window would take it back. A
popover stays open over niri's overview. Radio rows (`radio_rows`) are rebuilt on each
change, with the chosen one set before the handler runs; choosing another sends its
command, closes the popover, and switches the shown row back on: GTK selects a clicked
row at once, and a choice the service refuses (polkit, for one) sends no update, so the
rows keep showing the service's state until an update rebuilds them. Each item has an accessible name that says its state
("Battery 50%, on battery", "Power profile: Balanced").

`items.rs` has the three items:

- the battery: the theme's `battery-level-N[-charging]-symbolic` by tens (charged when
  full on mains), the percentage beside it only with `battery-percentage`; the popover
  has the percentage, the state and the time to empty or full.
- the power profile: `power-profile-<profile>-symbolic`; the popover lists the offered
  profiles, and why performance is limited when it is.
- the keyboard layout: its short name in upper case, as macOS shows it ("EN", "DE"),
  shown only with two layouts or more; the popover lists them, and a choice is niri's
  `SwitchLayout`.

### Look

`style.css` is fixed: a 0.35 black tint, white text, hollow dots and a filled active
one, the app name at weight 600; popovers have the same tint, a 10 px radius and a thin
white outline, with no shadow, so that the surface is the popover's own rectangle. Its sizes are CSS variables (`--height`,
`--font-size`, `--icon-size`, `--spacing`, `--dot-size`), set by one generated rule on
`window.bar` from the config (`main.rs`, `stylesheet`). The font family is the system's
(`gtk-font-name`).

### Blur

`blur.rs` makes our own event queue on GDK's Wayland connection (from gdk4-wayland),
binds `ext_background_effect_manager_v1`, and reads its capabilities with one round
trip: without blur among them, there is none. Each bar's window and each popover asks
for blur behind its whole `wl_surface` when it is mapped (logging `blur on`); the
request takes effect with GTK's next commit. The effect object is destroyed when it is
unmapped. niri blurs popovers' surfaces too; as the region is the whole rectangle, the
blur shows a little past the popover's rounded corners. The
queue is drained on each request; a later change of capabilities is not followed. GTK 4.23.3 and later bind
the protocol themselves (a second object is a protocol error), so there the shell logs
that blur is not supported yet; that path (blur nodes in the snapshot) is to be built
when the system's GTK reaches it.

## Start-up

The unit starts after `graphical-session.target`, which niri reaches once it is ready
(as vibepanel does). In `main()`:

- `GSK_RENDERER` is removed, so GTK uses its default renderer (Vulkan), not the
  session's `gl`.
- `GDK_DEBUG=no-portals` is set (appended to any existing value) before `gtk::init`, so
  GTK does not wait for xdg-desktop-portal, and restored right after, so nothing the
  shell starts inherits it. GTK has started its own threads by then; the change touches
  only that variable's slot in glibc's environment array (see the comment there).
- What does reach the apps the shell will start: `GSK_RENDERER` stays removed, and the
  Nix wrapper's `XDG_DATA_DIRS`, `GIO_EXTRA_MODULES` and `GDK_PIXBUF_MODULE_FILE` are
  inherited. The shell starts nothing yet; the first step that does sets each child's
  environment.
- There is no `GApplication`, so no D-Bus name and no unique-app registration: a plain
  GLib main loop runs the bars.
- `+N ms` marks are logged from `main`: `gtk_init` and `first frame` (the first bar's
  first paint; the handler is disconnected once it has fired). Before `main`, loading
  GTK's shared libraries takes about 21 ms warm on AC and 60 ms on battery, as for any
  GTK binary here.

Measured in a nested niri on the laptop, warm, back to back with the step 0 spike bar
(`SPIKE_NONUNIQUE=1`, `GDK_DEBUG=no-portals`), from `main` to the first paint: on AC
(power profile `performance`) the shell takes 59–69 ms (median 60) and the spike 55–57;
on battery both take about three times as long (the audit's runs: 247 ms against 235 from
the process's start). The power state matters more than the difference between the two,
so start-up numbers are compared only within one run.

Without the portal, GTK 4.22 reads its settings from GSettings (dconf). The font
(`gtk-font-name`), icon and cursor themes come through. The colour scheme does not:
GTK reads it only from the portal's `org.freedesktop.appearance`, so it is
`Unsupported` without it, although dconf has `prefer-dark`; the same holds for contrast.
GTK has no accent colour setting at all (libadwaita reads that from the portal). This
was checked with `gtk4-query-settings` in a nested niri, with and without the portal.
The bar's colours are fixed, so neither matters yet; popovers will need the colour
scheme from GSettings themselves.

## Config

KDL through knuffel; unknown nodes and wrong types are errors with their position, and
`Config::parse` then checks the ranges (height 16–64, each size 1 to the height) and the
clock format. `--check <path>` loads a config and exits, which the home-manager module
runs when the configuration is built.

## The home-manager module

`module.nix` renders `settings` (an attribute set of booleans, integers and strings) as
one KDL node per key, checks it with `--check`, and adds the user unit
`rust-wl-shell.service`: `PartOf` and `After` `graphical-session.target`, wanted by it,
`Restart=on-failure`, and no `Environment=`.

## Tests

Unit tests (`cargo test`):

- niri: a recorded niri 26.04 event stream (`crates/core/testdata/events.jsonl`, from a
  nested niri: two apps opened, focus moved, a workspace switch and back, both closed)
  replayed, with the focused app and active workspace checked after every event; the
  ordering of workspaces across outputs.
- the clock's sleep to the next minute, including a leap second.
- the config: defaults, sizes following the height, every key, unknown keys, invalid
  values.
- the arguments.
- UPower's and power-profiles-daemon's properties to their snapshots; the keyboard
  layouts from niri's events; the items' icon names, short layout names and times.

The NixOS VM test (`tests/bar.nix`, `nix build .#checks.x86_64-linux.bar`) runs niri
itself, on its tty backend on a virtio-gpu without virgl, so Mesa renders with
llvmpipe. niri 26.04 refuses software renderers there, and llvmpipe has no render node;
the test's niri is patched in two places (`tty.rs`) to accept it. alice is logged in on
tty1, her login shell runs `niri-session`, and the shell starts through the home-manager
module's unit. niri's background is plain grey, and it has two keyboard layouts ("us,de").
Fake UPower and power-profiles-daemon services (`tests/fake-services.nix`, dbus-python)
run on the system bus. The test checks:

1. the module's settings, chosen unlike the defaults (`height 28`, `clock-format
   "%H:%M %d/%m"`), pass `--check`; the shell logs its marks and `blur on`; the unit
   sets neither `GDK_DEBUG` nor `GSK_RENDERER`
2. the bar is 28 px high from the top edge, and tints the grey under it
3. a click on a workspace dot switches workspace (read back from `niri msg`)
4. the app name follows focus: a desktop file's Name, else the app id; on an empty
   workspace the tree still has the bar's clock and none of the apps (from the AT-SPI
   tree)
5. the clock shows the VM's time in the configured format
6. the AT-SPI tree has the bar's windows, buttons and labels
7. an output switched off loses its bar and gets it back, with its workspaces, when
   switched on
8. a niri socket that closes after the event-stream reply ends the shell with status 1
9. a click on the power profile item opens its popover with three profiles; a choice
   the daemon refuses leaves the rows on its profile (checked through AT-SPI's
   `checked` state); a click on Power Saver sets it on the daemon, closes the popover,
   and the item follows
10. the layout item shows "EN"; choosing German in its popover switches niri's layout
    and the item shows "DE"
11. no battery without UPower; with it, the battery item, its popover with the time
    left, closed by Escape, after which text typed reaches the focused window (foot)
    again; its icon and name follow a change; hidden when UPower reports no battery
12. after each popover closes (a choice, or Escape), the bar asks for no keyboard again
    (`niri msg layers`)

Items and popover rows are found by their pixels (white on the tint, apart by gaps),
not by AT-SPI's extents, which in this VM are off by about 17 px from where GTK lays
the buttons out.

Screens are read with QEMU's screendump; clicks go through QMP on the USB tablet
(`tests/screen.py`). `nix flake check` also runs `cargo fmt --check` and clippy with
`-D warnings`.
