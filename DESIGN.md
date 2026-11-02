# Design

rust-wl-shell is a macOS-style bar for niri: a bar on every output with the output's
workspaces and the focused app's name on the left; the keyboard layout, the brightness,
the volume, VPN, Bluetooth, the network, the battery (with the power mode in its
popover) and the clock on the right, each item with its own popover; an on-screen
display (OSD) for volume and brightness; and a password dialog for Wi-Fi and
NetworkManager's requests.

## Crates

- `crates/core` (`shell_core`): plain Rust, no GTK. The services (niri, the clock,
  UPower, power-profiles-daemon, the backlight, the audio, NetworkManager and its secret
  agent, BlueZ), the state they produce, and the config. The network, the agent and
  Bluetooth run only when an item needs them (`network` or `vpn`, `bluetooth`). It runs a current-thread tokio runtime on its own thread
  (`shell_core::start`); the audio has a thread of its own (below).
- `crates/shell` (binary `rust-wl-shell`): the GTK4 + relm4 UI, on GTK's main loop.

The two talk over tokio channels with data only. The core sends `Update`s, each a whole
snapshot of one service (`Niri`, `Clock`, `Battery`, `Profiles`, `Audio`,
`Backlight`, `Network`, `Bluetooth`, `NightLight`, `KeepAwake`, `Notifications`), or an
event for the dialog (`Joined`, `Prompt`,
`PromptClosed`); the UI sends requests back on one sender per service (`Core::niri`,
`profiles`, `audio`, `brightness`, `network`, `answers`, `bluetooth`, `night_light`,
`keep_awake`, `notifications`). The UI awaits the update channel from a GLib future;
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

## Audio

`audio.rs` follows the default output (sink) over the PulseAudio protocol, which
pipewire-pulse serves, as the toolkit ADR has it: the default sink's volume (its
loudest channel, in percent of libpulse's normal volume, as pactl and wpctl show it)
and mute, and the outputs to choose from: one per available port (its availability not
"no") of each hardware sink, as macOS and GNOME list them, so a headphone jack gets its
own output when something is plugged in (cards are followed too, for their jacks); a
sink without ports is one output. A port whose type (libpulse 14's, through
libpulse-binding's `pa_v14` feature) is specific (speaker, headphones, headset, HDMI,
Bluetooth) is named by its description ("Speakers") and gets that kind's icon. A
generic port (analog, line, S/PDIF, unknown), like a USB DAC's "Analog Output", is
named by its card ("Chu2 DSP"), and its icon comes from the card: its form factor
(headphone or headset, speaker) when it has one, else headphones for a USB or Bluetooth
card, else speakers. A Bluetooth sink (bus `bluetooth`) is named by its card, the
device ("AirPods Pro"), whatever its port's type, since bluez5 describes its routes
generically ("Headphones"); the port's type still gives the icon. The card's properties are read from the sink's, where
pipewire-pulse copies them: `device.description` (under plain PulseAudio, the sink's
own), `device.bus`, and `device.form_factor` (PulseAudio's key, else PipeWire's
`device.form-factor`); the owner's DAC has a bus and no form factor. Names two outputs
share get their card's added (unless it is their name already), and " 2", " 3" in
order when they still would. Only hardware sinks are listed: pipewire-pulse flags a sink `HARDWARE` unless its
node is virtual, which leaves out EasyEffects' sink and other filters. Requests set the
default sink's volume (its channels scaled so that the loudest is at the level, at most
100 %, which keeps their balance), its mute, or an output: its port first, unless its
sink already has it, then the default sink.

**libpulse-binding, not the pure-Rust `pulseaudio` crate.** The `pulseaudio` crate
(0.3.2) has an async client for lists, server info and the default sink, but its
public client has no volume, mute or subscription: those would be written against its
protocol layer, over a blocking connection of our own, next to the client's own reactor
thread. libpulse-binding has all four (subscribe, `set_sink_volume_by_name`,
`set_sink_mute_by_name`, `set_default_sink`) with no `unsafe` in our code, at the cost
of the C library and of its main loop, which is not `Send`. The choice was not close.

libpulse's standard main loop runs on a thread of its own, and everything that touches
it stays there, in `Rc`s: the subscription callback marks the state changed, and the
loop then reads the server's default sink and every sink (one round of two queries),
keeps the default sink's channel volumes, and sends the snapshot when it changed.
Requests from the UI go into a channel, with a byte on a socket pair whose other end the
main loop watches as one of its own I/O events, so no thread polls; requests queued
while there was no connection are dropped when one is made, as they may be for a sink
that is no longer the default. The thread holds the update sender weakly, so that the
shell's updates still close when the core stops. Without a server (or when it goes
away) the thread sends `None` and connects again every 2 s, logging each error once
while it repeats.

## The network

`network.rs` reads NetworkManager through nmrs 3.5 (zbus 5 and tokio, as the core). What
was checked before building on it:

- **Its bus connection.** nmrs opens its own system bus connection
  (`NetworkManager::new`), and its secret agent another; neither takes an existing one.
  So the core has three: its own, nmrs's and the agent's.
- **Its events.** `network_events` follows, by D-Bus signals and without polling:
  devices added and removed and their states, NM's state, access points added and
  removed and each one's `Strength`, the active connections list, `WirelessEnabled`
  (and its hardware switch), connectivity, the saved connections, and NM restarting.
  Not followed: an active connection's own state, so a plugin VPN (one without a device)
  that goes from activating to activated is seen only at the next other event. A
  WireGuard connection has a device, whose state is followed. On the laptop, iwd's
  signal updates make an event about every 6 s; a read takes about 12 ms there.
- **iwd.** Measured in the VM test's set-up (NM 1.58 on iwd 3.12, hwsim radios):
  `RequestScan` and joining an open, a WPA2 and an enterprise network all work, and a
  wrong WPA2 password fails as `AuthFailed` after about 8 s. Two things need care. NM's
  iwd backend refuses `system-ca-certs` ("not supported"), so the system's CA store is
  named as the CA certificate, `/etc/ssl/certs/ca-certificates.crt`; and iwd reads that
  file only if it holds plain certificates: NixOS's default bundle also has OpenSSL's
  `TRUSTED CERTIFICATE` blocks, which iwd's ell rejects (the whole file fails to load),
  so it needs `security.pki.useCompatibleBundle = true`. And NM refuses to add and
  activate an enterprise network in one call before iwd has a profile for it, so the
  shell adds it, waits 1 s (iwd loading the profile NM writes; at once fails), then
  activates it.
- **The agent.** nmrs's `agent` module serves NM's `SecretAgent` interface and hands
  each request over as a stream item with a responder, which the core keeps until the
  UI's answer comes back on a channel; NM's cancellations come on a second stream. Its
  request and responder types leave the secrets out of `Debug`, and nmrs logs none at
  the levels the shell shows (info and above). It does not check that a request comes
  from NetworkManager (below).

On each event (those coming within 100 ms read once), the core reads the Wi-Fi switch,
the access points, the saved connections and the active ones, and sends `Network` when
it changed (`None` without NM):

- **the primary connection**, for the icon: an activated wired connection, else an
  activated Wi-Fi one (its signal), else connecting, else offline.
- **the Wi-Fi networks**: one per SSID (hidden ones left out), with the strongest
  access point's signal and its security (open, WPA2, WPA3 when it offers SAE only, or
  enterprise), the saved connection's UUID when it is known, and whether it is the
  current one; known ones first (the current one on top), then the others by signal.
- **the VPNs**: the saved VPN and WireGuard connections, each off, connecting or on.

The requests: a scan (when the popover opens), the Wi-Fi switch, activating a saved
connection (a known network, or a VPN to switch on; a known network whose password is
refused is activated a second time, as NM asks the agent for a new one only then, with
iwd), deactivating one (a VPN), and joining a network. Joining an open or WPA2 network is
nmrs's `connect`, which deletes a new profile that fails; an enterprise one is added,
then activated, and deleted if that fails. A join's end is sent back as `Joined`, with
its error. Each request runs as a task of its own: a join takes seconds.

## The secret agent

`agent.rs` registers the agent (identifier `rust-wl-shell`) once NM is on the bus, and
again whenever NM restarts. A request the user may be asked (`ALLOW_INTERACTION`) for a
Wi-Fi password, an enterprise network's credentials or a VPN's password becomes a
`Prompt`; any other is answered at once with no secrets. The UI's answer goes back to NM
(the password, the username and password, or the VPN secret under the key NM hinted);
no answer is a cancel. NM's `CancelGetSecrets` closes the prompt (`PromptClosed`); nmrs
has answered NM already. So does a prompt left 115 s: nmrs answers NM with no secrets at
120 s and tells no one, so each pending request has a deadline a little before that. With
`secret-agent false` there is no agent (another one, a panel's, answers NM), and joins
from the popover still use the dialog. Secrets are kept only until they are passed on, and never
logged: neither the requests nor the answers implement `Debug`.

Known limits, left as they are:

- One prompt at a time: a second prompt, or a join from the popover, cancels the one
  open (NM is answered at once).
- Cancel while a join from the popover shows "Joining…" closes the dialog, but the
  attempt goes on to its end.
- nmrs warns "SecretResponder dropped without responding" when a responder is dropped
  unanswered, as after its own timeout.
- An enterprise network whose server's certificate is signed by a private CA (one not
  in the system's bundle) cannot be joined: there is no field for a CA certificate.
- nmrs does not check that `GetSecrets` comes from NetworkManager. The system bus's
  policy lets only root call `SecretAgent` methods, so the risk is low; a check would
  need nmrs to take the message header (`#[zbus(header)]`) in its interface.

## Bluetooth

`bluetooth.rs` reads BlueZ with zbus directly (not `bluer`, which would bring the
`dbus` crate beside zbus): its `ObjectManager` objects, again on any signal BlueZ sends
(those within 100 ms read once) and when it starts or stops. `Bluetooth` is the first
adapter's `Powered` and its paired devices, by name: `Alias`, `Icon`, `Connected`, and
`Battery1`'s `Percentage` when BlueZ has it; `None` without BlueZ or an adapter. The
requests set `Powered`, or call a device's `Connect` or `Disconnect`. There is no
pairing. A known limit: every signal from BlueZ (any object's, any property's) means a
new read of all its objects, coalesced over 100 ms.

## The night light and keep awake

- **The night light** (`night_light.rs`) is wlsunset's, run by home-manager as a user
  unit; the shell starts and stops that unit (`night-light-unit`, by default
  `wlsunset.service`) through systemd's `org.freedesktop.systemd1` on the session bus.
  wlsunset has no IPC (only `SIGUSR1`, which cycles modes that cannot be read back), and
  niri resets the gamma as soon as it disconnects, so stopping it is turning the night
  light off. The state is the unit's `ActiveState` (on while active, activating or
  reloading), followed through `PropertiesChanged` (the shell calls the manager's
  `Subscribe`, without which systemd sends none), so a `systemctl --user stop` shows
  too. The unit is loaded by name (`LoadUnit`): one `not-found` hides the item.
- **Keep awake** (`keep_awake.rs`) holds a logind inhibitor while on:
  `Inhibit("idle", "rust-wl-shell", "Keep awake", "block")`, released by closing its
  fd. Idle only: the lid, the power key and an asked-for lock still act. It is off at
  every start; a refused inhibitor is logged and leaves it off. What honours it:
  rust-wl-idle-manager (logind's `BlockInhibited`) and logind's own `IdleAction`. niri's
  ext-idle-notify does not: niri 26.04 does not read logind's inhibitors (its source
  has no `BlockInhibited`), so an idle client of niri's alone (swayidle) does not wait.

## Notifications

`notifications.rs` (core) is the server: `org.freedesktop.Notifications` on a session
bus connection of its own, claimed only with `notifications` on (while another server
has the name, it logs that and serves nothing). Its narrow interface to the rest: the
whole state (`Notifications`: Do Not Disturb, the banners, the history) on each change,
and the UI's requests (`Expired`, `Dismiss`, `Action`, `ClearAll`, `Dnd`).

- **The spec:** `Notify` (with `replaces_id`: the same id, its banner and history entry
  replaced), `CloseNotification`, `GetCapabilities` (`actions`, `body`,
  `body-markup`, `icon-static`, `persistence`, `x-kde-origin-name`; not
  `body-hyperlinks`: Pango's markup has no links, so `<a ...>` and `</a>` are taken out
  of a body, their text kept, as some apps send them anyway)
  and `GetServerInformation`; the signals `NotificationClosed`, `ActionInvoked` and
  `ActivationToken`. Hints read: `urgency`, `transient`, `image-data` (and its older
  names), `image-path`, `x-kde-origin-name` (shown after the app's name). An
  `image-data` is kept only if GTK can take it (8 bits, RGB or RGBA as its alpha says,
  1 to 2048 px a side, rows at least as long as a row, and all its pixels there); else
  it is left out, as GTK would fail on it. Summaries are cut to 1 KiB and bodies to
  4 KiB, on a character's boundary.
- **Banners and the history:** a notification goes to the history (newest first, 100 at
  most; not a transient one) and, unless Do Not Disturb is on and it is not critical, to
  the banners (three at most: past that, the oldest leaves as if its time were up). A
  banner's time: the app's timeout, else 5 s; "never" (0) is `notification-max-timeout`
  for a non-critical one (so a call's notification does not stay), and a critical one
  stays until dismissed, whatever timeout its app gives.
- **Closing:** a banner leaving is not a close, as with GNOME: the notification stays in
  the history, still to be acted on. It is closed (`NotificationClosed`) when it is gone
  from both: dismissed (× or a swipe, on a banner or in the history; reason 2), acted on
  (2, after `ActivationToken` and `ActionInvoked`), cleared (2), closed by its app (3,
  which removes the history entry too), or gone from both without the user (expired,
  1): a transient one's banner gone, a transient one under Do Not Disturb (never
  shown), one pushed out of the history's 100 while not on screen; a banner whose
  window goes (its output gone) before its time is up. Closes that `Notify` itself
  causes are sent just after its reply, so an app has the id before it hears of it.
- **The file:** the history and Do Not Disturb, as JSON in
  `$XDG_STATE_HOME/rust-wl-shell/notifications.json` (written whole on each change,
  through a renamed temporary file), without images or actions: after a restart the
  entries are kept but can no longer be acted on. The last id given is kept too, so
  that ids are not given again after a restart, even with the history empty.

In the UI (`notifications.rs`, `clock.rs`):

- each banner is a window of its own on the overlay layer of the focused output (niri's),
  anchored at the configured corner, `notification-gap` (8 px) from the edges (and below
  a bar, by its exclusive zone), `notification-width` (360 px) wide, opaque in the theme's popover colours
  (`@popover_bg_color`, `@popover_fg_color`, as the popovers) and rounded at `panel-radius`
  (14 px), with no blur; they stack from the corner, newest first, their cards a gap apart. A banner (`notifications::content`) is macOS's: on the
  left the app's icon, full colour, 40 px (its desktop file's, from the `desktop-entry`
  hint through `DesktopAppInfo`, else the icon it sent, as a name, a path or a `file://`
  URI, else `application-x-executable`), or its image (`image-data`, as a texture) with
  the app's icon a 16 px badge on its corner; the summary in bold (two lines at most)
  and the body in Pango markup (as
  it is, if it does not parse), three lines at most; no app name. Two buttons at most, on
  the right (`default`, its click, is none of them): one or two actions other than
  `default` each a button (its label ellipsized past 12 characters, its key with no
  label); three or more, the first one's button and "Options", a menu
  button whose popover (the bar's popovers' look) lists the others, below the banner at the button's right edge, taking the keyboard while open
  (`OnDemand`, as the bar's popovers; Escape closes it). × is a square in the theme's
  red (`@destructive_bg_color`, its glyph `@destructive_fg_color`; libadwaita's in
  `fallback.css`), rounded at 6 px as niri's windows, in the card's top-right corner,
  inside it (an overlay of the card, so within its padding), the card's text and buttons
  keeping its width and a gap clear of it; hovered, its red mixed 15 % towards the
  glyph's colour. Its size, radius, inset, colours and hover are the stylesheet's
  (`--close-size`, `--close-icon-size`, `--close-radius`, `--close-inset`, `--close-gap`,
  `--close-bg`, `--close-fg`, `--close-bg-hover`, `--close-outline`). The window is the
  card alone. × shows while the pointer is over the card (CSS `:hover`; always in the
  tree, for screen readers, named `Dismiss "summary"`, else after the app, else
  `Dismiss`). The history's rows use the same card, without actions, its × in the same
  corner, with the time after the summary, muted ("now", "5m",
  "2h"; as of its opening or last change). Its countdown (`Countdown`) pauses while the pointer
  is over it, or its menu is open (resumed when it closes only if the pointer has left).
  A replaced notification's banner is built again, with its whole time, the old one's
  countdown finished first (so that its menu closing with it does not resume it). The
  `Banners` hold each banner's window until they destroy it (gtk4-layer-shell keeps a
  window whose output goes), and the menu's handlers hold the window weakly. While
  the Notification Centre (the history) is open, the banners are hidden, their countdowns
  still running: their windows are destroyed (a countdown paused by the pointer resumes)
  and
  built again when it closes, and a banner arriving meanwhile gets its window then.
  Not hidden windows: GTK 4.22's AT-SPI root answers `GetChildAtIndex` for an index
  past its shown windows (a client's stale count) with its last window, shown or not,
  and a hidden window's accessible is unrealized, with no path: the shell crashed in
  `g_variant_new` (the VM tests' "clock's popover" timeouts). Banners are its last
  windows, created after the bars, so none may stay hidden; nor may the OSD's and the
  dialog's, which would be last once no bar is left (no output): both build their
  window when shown and destroy it when they hide. The dialog builds its widgets with
  it, from plain state (`View`: its texts, which fields show, the error; never the
  password), rather than moving them to a new window, which GTK warns of
  (`gtk_at_context_set_accessible_role`); it keeps what it asks for and its countdown,
  and its window's unmap (an output gone) still calls a countdown off, and answers
  NetworkManager's prompt at once (cancelled, rather than left to its 2 min).
- a click on a banner runs its `default` action, with an activation token from GDK's
  launch context (xdg-activation, niri's), so the app may raise its window.
- a swipe (`notifications::swipe`): a horizontal `GtkGestureDrag`, or a two-finger
  horizontal scroll (`GtkEventControllerScroll`; a wheel's steps count 40 px). The
  banner follows it by its margin on its anchored side and fades; past `swipe-distance` (100 px) it is
  dismissed, short of it it goes back. The drag's offsets are in the banner's own
  coordinates, which move with it, so each is added to where it was. History rows
  swipe the same way, shifted by their margins.
- known limits: the name is claimed once, at start (another server leaving it later is
  not noticed); the banners stack by their heights as measured on the focused output,
  so a banner shown on another output (the focus moved) can leave a gap; the `resident`
  hint is not honoured (acting on a notification always closes it).
- the clock is a button (with `notifications-disabled-symbolic` before the time
  while Do Not Disturb is on, as macOS's Focus moon, "Do Not Disturb" for screen
  readers) that opens the Notification Centre (`centre.rs`) on its output, as macOS
  Sonoma's: a solid panel down the output's right, below the bar (the bar's exclusive
  zone; it reserves none, overlaying the windows), `notification-centre-width` (380 px)
  wide, `notification-gap` from the edges, in the popovers' colours, outlined as the
  banners (`--centre-bg`, `--centre-fg`, `--centre-outline`, its spacing
  `--centre-padding`, `--centre-spacing`, `--centre-card-gap`). It has "Do Not
  Disturb" (a switch, libadwaita's as the popovers'), then "Notifications" with "Clear
  all", and the history grouped by app (the app with the newest first, `centre::groups`),
  each entry a card as a banner's, with ×, swiped away, a click its default action
  while it still has one (closing it); "No notifications" when empty. It scrolls. The
  list is built again only when the history changes (a banner's countdown, say, leaves
  it), so a card keeps its hover, a swipe and the keyboard. It is
  a layer surface on the top layer (namespace `rust-wl-shell-centre`, for a niri
  shadow rule), its window built on opening and destroyed on closing (GTK 4.22's
  hidden-toplevel crash, as the OSD and the dialog), its contents built again on each
  change while open; it slides in from the right (a `GtkRevealer`, 200 ms; niri 26.04
  animates no layer surface), and out, its window destroyed 200 ms after on a plain
  timer (not the revealer's end, which needs frames an unplugged or locked output does
  not draw). It closes on the clock,
  on Escape, and on a click anywhere else: it takes the keyboard `OnDemand`, which niri
  gives a layer surface as it maps and takes away on a press on anything but it
  (`focus_layer_surface_if_on_demand`), so its window's `is-active` turning false
  closes it (the open window's only: a closing one's unmap turns its own false). A
  click on the clock while it is open first takes its keyboard (closing it), then
  reaches the clock: the clock decides at the press (a capture-phase click gesture
  noting whether it was open), so that click closes it rather than opening it again;
  with no press (a screen reader's click) it goes by the state then. The clock is
  `Expanded` for screen readers while it is open. Opening it closes an open item popover; an item popover
  opening takes its keyboard, closing it; the banners hide while it is open. Per-app
  collapsible stacks are for later. Known: a screen reader opening it from a bar on an
  output niri has not focused gets no keyboard there (niri focuses the active output's
  surfaces), and Escape in niri's overview is niri's.

## The power menu

`power.rs` (core): through logind's manager on the system bus, the menu's answers
(`CanSuspend`, `CanReboot`, `CanPowerOff`; `yes` or `challenge` shows the entry, read at
start) and its actions, each `interactive` so that polkit may ask: Lock Screen is
`LockSession` for the user's graphical session (`GetUser` for the shell's uid, then its
`Display`, else the first of its `Sessions`: the shell is a user unit, outside any
session), Sleep `Suspend`, Restart
`Reboot`, Shut Down `PowerOff`. Log Out is niri's `Quit` (`skip_confirmation`) on its IPC
socket: the session ends with niri. Each action's outcome comes back as `Acted`, with
logind's error if it refused. Before a restart or a shut down (or sleep), `Blockers` lists
logind's **block** inhibitors for it (`shutdown`, `sleep`; a mode starting with
`block`), as "app: reason"; delay ones are left out, as logind waits for them anyway.
Without logind, or without a system bus, only Log Out is offered. The core follows
logind's `PrepareForSleep` and the session's `LockedHint`: a sleep starting, or the
session locked, sends `Interrupted`, which calls off a power dialog.

In the UI (`power.rs`), the item is the theme's `system-shutdown-symbolic`, the rightmost
in the bar; its popover's rows act at once (Lock Screen, Sleep) or open the dialog
(Restart…, Shut Down…, Log Out…), once the core has their blockers (`Asked`). The dialog
(`Dialog::ask_power`, the Wi-Fi password's) shows "Restart?" and "Your computer will restart in 60 seconds." counting down each second
(`power-countdown`), the blockers under it, then Cancel and the action's button (the
accent's, the default; Enter takes it, Escape cancels). At 0, or on the button, the
action is asked for, and the dialog is hidden (and gives the keyboard back) while it
runs, so that polkit's prompt, if it asks, is not under it; it closes when it is done,
or is shown again with logind's refusal in its error line. A refused Lock or Sleep,
asked without a dialog, gets one to say so.

The countdown never acts unseen: it stops when the dialog leaves the screen (unmapped:
its output unplugged), when a sleep starts, and when the session locks. Known
behaviours: a Wi-Fi password prompt from NetworkManager's agent arriving during the
countdown takes the dialog, which calls the power action off; Escape after the action's
button (the request sent, the dialog hidden) does not undo it, as logind has it.

## The backlight

`backlight.rs` reads `/sys/class/backlight/<device>` (the config's `backlight`, a name,
not a path; else the one systemd-backlight and GNOME would pick: by `type`, firmware,
then platform, then raw, then by name): `brightness` of `max_brightness`, in linear percent as brightnessctl
shows it. It follows changes through the kernel's `sysfs_notify` on
`actual_brightness`, which the backlight core raises on every write, by any writer
(brightnessctl, logind, the keys): the file is polled for `POLLPRI` with tokio's
`AsyncFd` and read again to rearm it. This is the same kernel event udev's `change`
uevent is sent with (D2 named udev); reading it from sysfs needs no libudev. A level is
set with logind's `Session.SetBrightness` on `session/auto` (the caller's session, else
its user's graphical one), which needs no permission beyond being the session's user;
never below 1 %, as 0 turns the backlight off on many panels. After an error it tries
again every 5 s, warning once while the error repeats; only a device that went away (a
driver reloaded) hides the item, until it is back.

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
output's workspaces and filters them again when the connector changes. `workspaces.rs` shows the
output's workspaces that have windows (a window with that `workspace_id`, the core's
`occupied`) and the one shown, as vibepanel does: one `button.workspace` each, holding a
`.pill`, `.active` when shown, labelled "Workspace N" (or its name) for AT-SPI, sending
`FocusWorkspace` when clicked. A pill is `pill-size` (the icon size); the shown one is
`active-pill-width` (11/8 as wide: vibepanel's 22 px on 16 on the owner's 30 px bar); a
gap of `pill-gap` (3/4 of the spacing) is split either side of it, inside its button.
These are config keys, not CSS, because the shell animates them. When the workspaces change, every button's width
(pill and gap) moves linearly, over `workspace-animation` (200 ms), from where it is to its new one, on one
clock that a further change starts again (a tick callback on the row): a switch's two
widths change by opposite amounts, and the widths are rounded where each ends
(`whole_pixels`), so the row's width, and what comes after it, stay put. A workspace
added grows from nothing (its gap opening first); one removed keeps its place
(`merge`), takes no clicks, and shrinks to nothing before its button goes. Before the
row is mapped, the widths are set at once.

The right side holds the items, then the clock; an item switched off in the config is
never built.

### Items and popovers

`popover.rs` is the component every item is built on: a `GtkMenuButton` (flat, with the
greeter's chip plate while hovered or open) whose `GtkPopover` GTK places under it,
as macOS's: no arrow, `popover-gap` from the bar (the bar sets each button's direction,
down from a top bar and up from a bottom one, and its popover's offset, which GTK does
not flip, so negative upward), outlined all round, its button keeping its hover plate
while it is open (the inner button is `:checked`). GTK closes it on an
outside click (so one is open at a time) and on Escape. The bar takes no
keyboard, so that clicks on it leave the focus with the window; while a popover is open
the bar's layer surface asks for the keyboard `OnDemand`, so that Escape reaches it,
and gives it back (`None`) when the popover closes. `OnDemand` rather than `Exclusive`:
if the keyboard were ever left asked for, a click on any window would take it back. It
is asked for from the press on the button, before the popover's grab: niri gives a grab
from a layer surface that takes no keyboard (its interactivity, committed, at the grab)
the pointer's alone, and its screenshot UI ends the pointer's grab without closing the
popover (only a keyboard grab is closed when the focus moves), which then stayed open past
any click outside. A press that opens nothing (dragged off the button) gives the keyboard
back once its click is done, else niri would give the bar the keyboard on its next click.
A popover opened from the keyboard or by AT-SPI has no press: it asks for the keyboard
when it shows, after its grab, so its grab is still the pointer's alone (step 9, the
keyboard and screen readers, is to revisit this). Its popover's `xdg_popup` grab is niri's (smithay's
`PopupPointerGrab`): a press on another client's surface, or on none, closes it; a press
on another surface of ours goes to that surface, where GTK's autohide (GDK's
`check_autohide`) closes the popover and keeps the press. So the pointer coming onto a
banner closes an open popover first (`popover::close_open`), and a click on the banner's ×
dismisses it. While the popover holds its grab the banner gets no motion, only the
pointer's enter (seen in nested niri); an enter in the banner's first 500 ms is the banner
showing under a still pointer, which leaves the popover open (but a banner moved under it
as the stack shifts, older than that, still closes it). A popover stays open over niri's overview.

Every item's popover is one width, `--popover-width` (320 px, its contents' `min-width`),
whatever it lists: the rows' labels and the titles ellipsize, with a natural width of one
character (`max-width-chars`), so a long network name never widens it; the Wi-Fi list's
scrolled window (no horizontal scrolling) takes its child's minimum width, and its
scrollbar is GTK's overlay indicator, taking no width.

The rows to choose from (`Item::rows`) look as macOS's: each a flat button with a hover
plate, a round icon of 26 px (for the one chosen, `@accent_bg_color` with
`@accent_fg_color` on it, as matugen's gtk.css defines them; the stylesheet defines both
from GTK's own `@accent_color` and white as defaults, which the user's gtk.css, at a
higher priority, overrides; a faint white otherwise; it holds an icon, or a short text
as "DE"), then its label, with no GTK radio or check indicator. For screen readers each row is a radio
button, checked when chosen. They are rebuilt on each change; a click on another row
sends its choice and closes the popover, and the rows show the choice only once the
service's update says it was made, so a refused one (polkit, for one) changes nothing. A
row may show something after its label (a lock, a battery's level). Each item has an
accessible name that says its state ("Battery 50%, on battery", "Wi-Fi: Home").

`items.rs` has two items, `sliders.rs` two more, `network.rs` two and `bluetooth.rs`
one:

- the battery: the theme's `battery-level-N[-charging]-symbolic` by tens (charged when
  full on mains), the percentage beside it only with `battery-percentage`; the popover
  has the percentage, the state and the time to empty or full, then, as macOS has Low
  Power Mode in its battery menu, the "Power Mode" section (with `power-profile`): the
  offered profiles as rows with their icons, and why performance is limited when it is;
  hidden without power-profiles-daemon. With no battery there is no battery item, and so
  no power mode.
- the keyboard layout: its short name in upper case, as macOS shows it ("EN", "DE"),
  shown only with two layouts or more; the popover lists them, and a choice is niri's
  `SwitchLayout`.
- the volume: `audio-volume-{muted,low,medium,high}-symbolic` (muted at 0 or muted,
  then by thirds); the popover has a "Sound" title, a slider (0–100 %) after the speaker,
  a flat button that mutes or unmutes (its icon says which it is, its accessible name
  "Mute" or "Unmute"), and the "Output" section with the outputs as rows, each with its
  kind's icon, rebuilt only when the outputs change (not on each step of the slider);
  a choice makes that output play. With no outputs the section is hidden.
- the brightness: `display-brightness-symbolic`, hidden with no backlight; the popover
  has a slider.
- the clock (`clock.rs`): the time, and the notifications' history (below).
- the night light and keep awake (`toggles.rs`): click toggles with no popover, flat
  buttons as the items, their state told by their icon, never a fill (only these and
  VPN have an on/off look): `night-light-symbolic`, the theme's moon, or
  `night-light-disabled-symbolic`; keep awake's cups are the shell's own,
  `rust-wl-shell-keep-awake-symbolic` steaming and `rust-wl-shell-keep-awake-off-symbolic`
  empty, installed with it and added to the icon theme's search path at start, as
  Adwaita has no cup; "Night light: on", "Keep awake: off" for screen readers. A click asks for the
  other state, shown once the core says so; hidden until it does.
- the network: the primary connection's icon (`network-wireless-signal-*-symbolic`
  in four levels, `network-wired-symbolic`, `network-wireless-acquiring-symbolic`, or
  offline); hidden without NM. Its popover, as macOS's: a "Wi-Fi" title with a switch,
  a wired connection's row when there is one, the known networks in range (the current
  one chosen), then "Other Networks", a closed `GtkExpander`, by signal. The networks
  scroll under the switch (a `GtkScrolledWindow` at most half the output's height), so a
  long list never takes the popover off the output. Rows show the
  signal and, after the name, a lock for a secured network. A click on a known network
  activates it (on the current one, nothing); on another, an open one joins and a
  secured one opens the dialog.
- VPN: `network-vpn-symbolic`, hidden with no VPN connection; while one is on, a badge on
  its bottom-right corner (a `gtk::Overlay`): a green circle (`--badge-ok`, GNOME's
  `#2ec27e`, fixed, as the theme's success colour may not be green) with a white
  `object-select-symbolic` tick, three eighths of the icon's size plus a ring in the bar's
  tint (`--tint`) as if cut out of the icon (a translucent ring: over the icon's strokes it
  shows grey rather than the backdrop; a true cut-out would need a mask in a widget of our
  own);
  a row per connection, a click switching it on or off ("Connecting…" after it while
  it connects).
- Bluetooth: `bluetooth-active-symbolic` connected, `bluetooth-symbolic` on with nothing
  connected, `bluetooth-disabled-symbolic` off; a "Bluetooth" title
  with a switch, then "Devices": the paired ones with their kind's icon (BlueZ's icon
  names mapped to the theme's symbolic ones), chosen when connected, their battery after
  the name; "No devices" when there are none. A click connects or disconnects.

Rows that switch something on and off (VPNs, devices) are toggles: any number chosen,
a click on a chosen one counts too, and each is a check box for screen readers. A title
with a switch (`popover::Switch`) sends the user's flip, and shows the service's state
only when it comes.

A slider (`popover::Slider`) sends a request for each whole percent the user moves it
to; an update moves it without sending (it remembers the value it shows, and a level
above 100 % stops it at 100 without that counting as a move).

### The password dialog

`dialog.rs`: one for the shell, styled as libadwaita 1.9's AdwAlertDialog in plain GTK
(no libadwaita: its rules for the dialog's widgets are ported to `adwaita.css`, which is
LGPL-2.1-or-later as libadwaita is, scoped to `window.dialog`, with GTK's own theme's borders, shadows and gradients cleared, and its
colours mapped to the theme's named colours, `@dialog_bg_color` and the others, with
libadwaita's dark ones as fallbacks; its outline libadwaita's, white at 7%): an opaque panel, rounded at 18 px, with no blur
(none is asked for, `niri-blur-rule` or not), on the overlay layer, centred
on the focused output (niri's), namespace `rust-wl-shell-dialog`. A bold heading ("Wi-Fi
Password Required"), the centred text ('The Wi-Fi network "X" requires a WPA2
password.'), a `GtkPasswordEntry` with its peek icon, for an enterprise network a
username and an "Options" disclosure (method PEAP or TTLS, inner method MSCHAPv2 or
PAP, the domain the server's certificate must be in), the error line, and Cancel and
Join as equal pill buttons, Join filled with the accent. Enter joins from any entry;
Escape cancels. While open, its surface asks for the keyboard `OnDemand` (niri gives it
the keyboard when it maps, so typing goes straight in), and on every way it closes it
asks for none and is unmapped. A join from the popover keeps it open ("Joining…") until
`Joined`: an error shows in it and Join comes back; success closes it. A prompt from the
agent closes as soon as it is answered, or when NM cancels it. The password entry is
emptied on closing.

### The OSD

`osd.rs` is vibepanel's look and numbers, not its code: one window on the overlay layer
(namespace `rust-wl-shell-osd`), anchored to the bottom `osd-margin` (48 px) above the edge, with no
exclusive zone, no keyboard and an empty input region, on the focused output (niri's
focused workspace's output). It is a frosted pill (padding 10 by 16 px) holding the icon
at 1.5 times `icon-size`, a 200 px level with no knob, and the value, and it hides `osd-timeout` (1.5 s)
after the last showing. `shell.rs` shows it for a change of volume, mute or brightness
between two updates of the same service: not for the first (at start-up, or when the
service comes back), not for a switch of the default output (here or elsewhere, as on
macOS), and not while that item's popover is open on a bar (it asks each bar), where the
user moves the slider. With its output switched off, it shows on the focused one left.

### Look

`style.css` defines every value once, as a variable in its `window { }` rule (colours,
a spacing scale `--space-xxs` to `--space-xxl`, radii `--radius-m` and
`--radius-pill`, and widths), and its rules use only them: a 0.35 black tint, white
text, faint white workspace pills and the shown one in the accent (`--accent`, GTK's
`@accent_bg_color`), the app name at weight 600; the OSD's level a white bar on a faint
track. The popovers look as GNOME apps' menus (macOS's principle: the menu bar's menus
share the apps' menu material), their rules ported from libadwaita to `adwaita.css` with
the dialog's: opaque in the theme's `@popover_bg_color` and `@popover_fg_color`, a 1 px
dark outline, rounded at `popover-radius` (libadwaita's 15 px), with no shadow (so the
surface is the popover's own rectangle) and no blur; rows hover as a menu's items, the
round icons as circular buttons, switches and sliders libadwaita's. A box's
spacing is CSS too: its `border-spacing`, which GTK's box layout adds to its (zero)
`spacing`. Before it, `style.rs` puts one generated `window { }` rule with what the
config sets: `--height`, `--font-size`, `--icon-size`, `--spacing`, `--popover-radius`
and `--panel-radius`. The banners'
and the popovers' radii, which once had blur following them, stay config keys; the bar's height feeds its exclusive zone; layer-shell margins,
the pills' animated sizes and the timings are the config's for the same reason, CSS not
reaching them. The font family is the system's (`gtk-font-name`).

Both go at GTK's application priority, `adwaita.css` appended to `style.css`. Under
them, at GTK's fallback priority (below the theme's), `fallback.css` defines the named
colours they use for a theme that has none: the accent's, as GTK's, and libadwaita's
dark dialog and error colours; a theme or a gtk.css defining any of them wins. The
user's stylesheet,
`$XDG_CONFIG_HOME/rust-wl-shell/style.css`, goes over them at user priority, in a
provider of its own: read at start-up and again on each event of a `GFileMonitor` on it
but `CHANGED` (a write's end comes as `CHANGES_DONE_HINT`; a rename over it, as
home-manager replaces its symlink, as `RENAMED` with `WATCH_MOVES`; a monitor on a file
watches its directory for its name, and polls for a directory that does not exist yet).
A missing file is an empty one. A file with a `parsing-error` (each logged, by line) is
replaced by nothing, so the built-in look stays, rather than the part GTK could parse;
but a warning (`GtkCssParserWarning`: an empty declaration, an unknown media feature)
is logged only.

### Blur

`blur.rs` makes our own event queue on GDK's Wayland connection (from gdk4-wayland),
binds `ext_background_effect_manager_v1`, and reads its capabilities with one round
trip: without blur among them, there is none. Each bar's window and the
OSD asks for blur behind its `wl_surface` when it is mapped (logging `blur on` at info
for bars, at debug for the others, which map on every showing); the request takes effect
with GTK's next commit. A bar's region is all of it; the OSD's is
its surface with its corners rounded (half its height, a pill), two rows of pixels at a time, so that the blur does not
show past the corners. The dialog, the popovers and the banners are opaque, with no
blur. With `niri-blur-rule`, the OSD is given no blur
object: niri 26.04 clips a region a layer surface sets to its rectangles, not to the
rule's `geometry-corner-radius` (`render_for_tile` with `clip_to_geometry` false), so
whole-pixel rows step at a fractional scale; with no region, the rule's blur is rounded
by niri, anti-aliased (the README's rule). The region is set again on each layout of the surface (its size). The effect object is destroyed when it is
unmapped. The queue is drained on each request; a later change of capabilities is not followed. GTK 4.23.3 and later bind
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
- the power menu's block inhibitors for each action (delay ones left out).
- the notifications' store: the timeouts (default, "never" capped, critical, a
  critical one with a timeout); images GTK can take, and malformed ones; links' tags
  taken out, texts cut on a boundary; closed when gone from both (transient under Do
  Not Disturb, pushed out of the history); banners
  newest first, three at most, the oldest leaving, a transient one pushed out closed as
  expired; replacing, an app's close, a banner's time up (kept), "Clear all"; Do Not
  Disturb; the file without banners; the history grouped by app.
- the network from what NM lists: one row per SSID with its strongest signal, known
  networks first and the current one on top, the primary connection (wired over Wi-Fi,
  connecting, a VPN connecting not counting), the VPNs' states; Bluetooth from BlueZ's
  objects: the first adapter, its paired devices only, by name, with the battery.

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
   sets neither `GDK_DEBUG` nor `GSK_RENDERER`; with no wlsunset unit in the VM, there is no
   night light item
2. the bar is 28 px high from the top edge, and tints the grey under it
3. the workspaces' pills (from the pixels and the tree): the shown one alone, blue, at
   first; an empty workspace has one only while shown, and it is wider; with windows on
   two, a click on a pill switches workspace (read back from `niri msg`), and once it
   has animated the pills' row ends and the app's name starts where they did; workspace
   2 left empty and gone, its pill goes
4. the app name follows focus: a desktop file's Name, else the app id; on an empty
   workspace the tree still has the bar's clock and none of the apps (from the AT-SPI
   tree)
5. the clock shows the VM's time in the configured format
6. the AT-SPI tree has the bar's windows, buttons and labels
7. an output switched off loses its bar and gets it back, with its workspaces, when
   switched on
8. a niri socket that closes after the event-stream reply ends the shell with status 1
9. the power mode, in the battery's popover (no item of its own): three profiles; a
   choice the daemon refuses leaves the rows on its profile (checked through AT-SPI's
   `checked` state); a click on Power Saver sets it on the daemon and closes the
   popover, which shows it chosen when opened again; with no battery, no power mode
10. the layout item shows "EN"; choosing German in its popover switches niri's layout
    and the item shows "DE"
11. no battery without UPower; with it, the battery item, its popover with the time
    left, closed by Escape, after which text typed reaches the focused window (foot)
    again; its icon and name follow a change; hidden when UPower reports no battery
12. after each popover closes (a choice, or Escape), the bar asks for no keyboard again
    (`niri msg layers`)
13. the volume (PipeWire with pipewire-pulse, two emulated HDA sound cards, and a
    virtual null sink as EasyEffects' is): no OSD at start-up; `wpctl set-volume` moves
    the item and shows the OSD, which hides after 1.5 s; the popover lists the hardware
    sinks' available ports, not the virtual sink, with only the playing row checked (the
    emulated cards have one generic port each, "Line Out", on cards both named "Built-in
    Audio", so the rows are "Built-in Audio" and "Built-in Audio 2"); the
    speaker before the slider mutes and unmutes (pactl); a drag on its slider sets the
    volume (pactl)
    and shows no OSD; choosing the other output makes it the default and, the popover
    opened again, its row the only one checked, with no OSD;
    `wpctl set-mute` mutes the item and shows the OSD; after a restart of
    pipewire-pulse the item comes back. The VM has no backlight: no brightness item.
14. the OSD shows on the focused output (Virtual-2), not on the other; with Virtual-2
    switched off, it shows on Virtual-1 (pixels of each output's bottom middle)
15. a popover, over a striped background (swaybg), is opaque (near its bar-facing edge,
    libadwaita's dark popover background, the same pixels again over niri's grey); on
    niri's grey, columns under its button and 20 px either side are grey for the 6 px gap,
    then one row of dark outline, then the popover's background; its
    button's plate is lit while it is open and not once it closes
16. from a bottom bar (a second shell with `position "bottom"`), the popover opens upward,
    with the same gap, outline and lit button
17. a user stylesheet setting `--tint` makes the bar red, rewritten blue (each written
    and renamed over the file); one that does not parse leaves the built-in tint, is
    logged, and the shell has not restarted; one with an override and `;;` stays
    applied, GTK's "Empty declaration" warning logged; a symlink renamed over it (as home-manager
    does) is followed; removed, the built-in tint again

The network's VM test (`tests/network.nix`, in the same session, `tests/session.nix`)
has real Wi-Fi: four `mac80211_hwsim` radios, NetworkManager on its iwd backend for
the first (iwd limited to it, eth0 and eth1 unmanaged, alice in the `networkmanager`
group), and hostapd on the other three in a network namespace of their own, with
dnsmasq's DHCP, so that their traffic goes over the air: "Home" (WPA2-PSK), "Cafe"
(open) and "Campus" (WPA2-Enterprise with hostapd's own EAP server, PEAP/MSCHAPv2, its
certificate for `server.radius.test` signed by a CA added to the system's, with the
compatible bundle). BlueZ is a fake (`tests/fake-services.nix`). Items, rows, buttons
and switches are acted on through AT-SPI (their actions, which need no extents); text
is typed with the VM's keyboard, into whatever has the focus. It checks:

1. the network item is offline, then follows each network joined
2. the popover lists the networks in range under "Other Networks", and the known ones
   above them, the current one checked; with 24 networks in range (each radio also has seven open
   ones), the popover ends above the output's bottom and a wheel over it scrolls it
3. the open network joins with a click
4. the WPA2 one opens the dialog (its text checked), which takes the keyboard: a wrong
   password shows "Wrong password or credentials." in it, still open; the right one
   joins and closes it
5. the dialog's Cancel, Escape, and an error then Cancel each close it
6. the enterprise network asks for the username too; with the domain set in "Options"
   (reached with Tab, checked through AT-SPI's focus), it joins, and its saved
   connection has the domain and the CA bundle
7. Home's password changed: a click on Home, and NM asks the shell's agent, whose dialog
   joins with the new one; changed again, the dialog's Cancel answers NM at once (the
   device does not wait in "need auth"); asked again, a join from the popover (an
   unknown network's dialog) cancels the prompt for NM at once; asked again, NM
   cancelling its request (the device disconnected) closes the dialog
8. after each way the dialog closes, and each popover, text typed reaches the focused
   window (foot) again
9. the Wi-Fi switch turns the radio off (`nmcli radio wifi`) and on
10. a WireGuard connection added with nmcli shows the VPN item; its row switches it on
    and off (`nmcli`), and the item says so, its green badge shown only while on (pixels)
11. Bluetooth: the paired devices only, in order, the headphones' 80 %; a click connects
    and disconnects one (the fake's state, the item's name), and the switch turns the
    adapter off
12. none of the passwords typed is in the system's or alice's journal
13. the night light, with a stand-in `wlsunset.service` (a sleep), started with the
    session: the item is on and filled with the accent (pixels); a click stops the unit
    (`systemctl --user is-active`) and empties the item, another starts it; stopped
    with `systemctl --user stop`, the item follows
14. keep awake: off at start; on, `systemd-inhibit --list` has its line (idle, "Keep
    awake", block) and logind's idle action (`IdleAction=lock` after 5 s, alice's
    session's idle hint set) does not come in 10 s; off, it comes ("System idle" in
    logind's journal) and the inhibitor is gone

The notifications' VM test (`tests/notifications.nix`, in the same session) sends with
notify-send (libnotify), calls with busctl, watches the signals with dbus-monitor, and
scrolls sideways with ydotool's uinput mouse (QEMU's tablet sends no horizontal wheel).
`notification-max-timeout` is 2 there. It checks:

1. the shell owns the name: its server information and capabilities (no
   `body-hyperlinks`); a malformed `image-data` (gdbus, 3 bytes for a 100x100 image)
   leaves the banner without it, GTK never handed it, the shell still running
2. a banner at the top right, below the bar, opaque in libadwaita's dark popover colour
   (pixels), with the app's name, the summary
   and the body's markup; four sent, three shown, the newest (with buttons) on top (the
   first gap between banners comes late); 0 actions no button, 1 its button, 2 two
   buttons, 3 the first's and "Options" with the other two
3. the timeouts, each timed alike from before it is sent: "never" capped (2 s) well
   before the default (5 s); a critical one stays; a transient one is not in the
   history, and is closed as expired
4. a replaced banner gets its whole time, the old one's timer gone; the banners hide
   while the Notification Centre is open (no notification layer in `niri msg layers`) and
   come back; replaces_id: one entry, the new one; an app's CloseNotification: the banner and the
   history entry gone, closed with reason 3
5. a click on a banner: notify-send --wait sees "default", after an ActivationToken
   with a token; a button: its key; the history's cards show no actions; two actions
   around the default, two buttons and no default's; long labels ellipsized, the
   banner's width kept
6. a drag sideways dismisses (reason 2, not in the history), a horizontal scroll too
7. the history in the Notification Centre, "Clear all", and the keyboard back with the
   window after each; the Centre a solid panel down the right (380 px, 8 px from the
   edge, below the bar, to 8 px above the bottom), closed by a click on the desktop, on
   a window, by Escape and by the clock (not opened anew by it)
8. Do Not Disturb: no banner, but for a critical one, the history still filling; a
   transient one (`notify-send --wait -e`) returns at once, its close after its id; after
   a restart of the shell's unit the history and Do Not Disturb are still there

The power menu's VM test (`tests/power.nix`, in the same session) gives the shell a
private system bus (its unit's `DBUS_SYSTEM_BUS_ADDRESS`) with a fake logind on it
(`tests/fake-services.nix`), whose answers and inhibitors come from files and which
writes down the calls it gets: the VM's own logind is never asked to power off. The other
system services are off there. `power-countdown` is 3. It checks:

1. the entries follow the answers: "no" leaves Shut Down out, "challenge" shows Restart
2. Lock Screen sends `LockSession` for the user's display session; Sleep `Suspend(true)`
3. Restart… opens the dialog, which lists the block inhibitor (not the delay one), and
   `Reboot(true)` comes at 0, the dialog closing
4. Shut Down…: Cancel, and Escape, then nothing; refused, logind's error shows in the
   dialog, still open; its button sends `PowerOff(true)` at once
5. the keyboard is back with the window after the popover and the dialog
6. a countdown is called off, nothing reaching logind, when its output is switched off
   under it, when the fake sends `PrepareForSleep(true)`, and when its session's
   `LockedHint` turns true
7. Log Out… ends niri (its process gone)

In the bar's test, items and popover rows are found by their pixels (white on the tint, apart by gaps),
not by AT-SPI's extents, which in this VM are off by about 17 px from where GTK lays
the buttons out.

Screens are read with QEMU's screendump; clicks go through QMP on the USB tablet
(`tests/screen.py`). `nix flake check` also runs `cargo fmt --check` and clippy with
`-D warnings`.
