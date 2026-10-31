# NixOS VM test: the bar on niri, on a virtio-gpu (no virgl: Mesa renders in software),
# through the home-manager module, checked by its pixels and its AT-SPI tree, in the
# session of session.nix; Virtual-2 is switched off and on.
#
# Behaviours asserted:
#   1. the module's settings (not the defaults) pass `--check` and reach the shell; it
#      starts with the module's unit, logs its start-up marks, and turns blur on
#   2. the bar is 28 px high, as configured, flush with the top, and tints what is under
#      it dark
#   3. the workspaces as pills: one for each workspace of the output with windows and for
#      the one shown, which is wider and blue (the accent); a click on a pill switches to
#      its workspace, and once the switch has animated, the app's name after the pills
#      has not moved; a workspace left empty loses its pill
#   4. the app name follows focus: a desktop file's Name, else the app id; none on an
#      empty workspace
#   5. the clock shows the VM's time in the configured format, `%H:%M %d/%m`
#   6. the AT-SPI tree has the bar, its workspace buttons and its labels
#   7. an output switched off loses its bar, and gets one again when switched on
#   8. niri's IPC closing ends the shell with a failure (a fake niri socket)
#   9. the power mode (a fake power-profiles-daemon), in the battery's popover, with no
#      item of its own: the popover shows the three profiles; a refused choice leaves the
#      rows on the daemon's profile; a click on Power Saver sets it on the daemon and
#      closes the popover, which shows it chosen when opened again; the rows go while
#      the daemon is stopped, and come back when it starts again; with no battery,
#      there is no power mode at all
#  10. the keyboard layout: niri has two, the item shows "EN"; choosing German in its
#      popover switches niri to it, and the item shows "DE" (xkeyboard-config's short
#      name); after each popover closes, the bar asks for no keyboard again
#  11. the battery: none without UPower; shown when (a fake) UPower starts, its popover
#      with the time left, closed by Escape, after which typing reaches the window again;
#      its icon and name follow a change; hidden when UPower reports no battery
#  12. the volume (PipeWire, two emulated sound cards and a virtual sink): no OSD at
#      start-up; an outside change moves the item and shows the OSD, which hides; the
#      popover lists the hardware sinks' available ports only, the one playing checked;
#      its speaker icon mutes and unmutes; a slider drag sets the volume with no OSD;
#      choosing the other output makes it the default and its row the checked one, with
#      no OSD; an outside mute shows the OSD; the server restarted, the item comes back.
#      No backlight in the VM: no brightness item.
#  13. the OSD shows on the focused output; with that output switched off, on another
#  15. no wlsunset unit: no night light item
#  16. a user stylesheet (~/.config/rust-wl-shell/style.css) setting the tint's variable
#      tints the bar red; rewritten, blue; one that does not parse is logged and leaves
#      the built-in tint, the shell running; one with only a warning (`;;`) is applied,
#      the warning logged; a symlink renamed over it (as home-manager
#      does) is followed; removed, the built-in tint again
#  14. a popover opens 6 px (popover-gap) from the bar, with no arrow, opaque (its pixels
#      the same over stripes and over grey), its outline unbroken along its bar-facing edge, its button lit while it is open and not
#      after; from a bottom bar (a second shell) alike, upward

{
  pkgs,
  homeManager,
  module,
}:

let
  session = import ./session.nix { inherit pkgs homeManager module; };
  inherit (session) user runtimeDir;

  fakes = import ./fake-services.nix { inherit pkgs; };

  # An app whose desktop file names it, run as foot with its app id.
  alpha = pkgs.writeTextDir "share/applications/org.test.Alpha.desktop" ''
    [Desktop Entry]
    Type=Application
    Name=Alpha Test
    Exec=foot --app-id org.test.Alpha
  '';

  # Stripes, 8 px white and 8 px black, for a background a blur smooths (a PNG made here).
  stripes = pkgs.runCommand "stripes.png" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    python3 - <<'PY'
    import struct, zlib
    row = b"\0" + (b"\xff" * 24 + b"\0" * 24)
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 16, 16, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(row * 16)) + chunk(b"IEND", b"")
    open("${placeholder "out"}", "wb").write(png)
    PY
  '';

  # niri's socket for a second shell: it answers the event-stream request, then closes.
  fakeNiri = pkgs.writers.writePython3 "fake-niri" { } ''
    import socket
    import sys

    server = socket.socket(socket.AF_UNIX)
    server.bind(sys.argv[1])
    server.listen(1)
    connection, _ = server.accept()
    connection.makefile().readline()
    connection.sendall(b'{"Ok":"Handled"}\n')
    connection.close()
  '';
in
pkgs.testers.runNixOSTest {
  name = "bar";

  nodes.machine = {
    imports = [
      # Not the defaults, so the bar's height and clock show they reached it.
      (session.machine {
        height = 28;
        spacing = 8;
        clock-format = "%H:%M %d/%m";
        # Off, so the items' places stay as the test counts them; the night light is on,
        # but hidden: the VM has no wlsunset unit.
        keep-awake = false;
        power = false;
      })
    ];
    environment.systemPackages = [
      alpha
      # pactl, to read the volume and the default output.
      pkgs.pulseaudio
    ];

    # Audio: PipeWire with pipewire-pulse, on two emulated sound cards (hardware outputs)
    # and a null sink (a virtual one, which the volume item leaves out).
    services.pipewire = {
      enable = true;
      pulse.enable = true;
      extraConfig.pipewire."10-virtual-sink" = {
        "context.objects" = [
          {
            factory = "adapter";
            args = {
              "factory.name" = "support.null-audio-sink";
              "node.name" = "virtual_sink";
              "node.description" = "Virtual Sink";
              "media.class" = "Audio/Sink";
              # As EasyEffects' sink is: pipewire-pulse then leaves out its HARDWARE flag.
              "node.virtual" = true;
            };
          }
        ];
      };
    };

    # The fakes: power-profiles-daemon from the start, UPower started by the test.
    services.dbus.packages = [ fakes.policy ];
    systemd.services.fake-power-profiles = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        Type = "dbus";
        BusName = "org.freedesktop.UPower.PowerProfiles";
        ExecStart = fakes.profiles;
      };
    };
    systemd.services.fake-upower.serviceConfig = {
      Type = "dbus";
      BusName = "org.freedesktop.UPower";
      ExecStart = fakes.upower;
    };
    virtualisation.qemu.options = [
      "-audiodev none,id=sound1"
      "-audiodev none,id=sound2"
      "-device ich9-intel-hda,id=hda1"
      "-device hda-output,bus=hda1.0,audiodev=sound1"
      "-device intel-hda,id=hda2"
      "-device hda-output,bus=hda2.0,audiodev=sound2"
    ];
  };

  testScript = ''
    ${session.helpers}
    def volume():
        """The default output's volume in percent, as pactl reads it."""
        out = as_user("pactl get-sink-volume @DEFAULT_SINK@")
        return int(re.findall(r"(\d+)%", out)[0])

    def glimpse(check, what):
        """`check` true of the tree at some point in 10 s, the tree read again and again
        (not once a second): for the OSD, shown for 1.5 s."""
        with machine.nested(f"glimpsing {what} in the AT-SPI tree"):
            end = time.time() + 10
            while time.time() < end:
                if check(tree()):
                    return
            raise AssertionError(f"no {what}")

    def osd_shown(t):
        """The OSD's value, a label of digits alone, is in the tree."""
        return any(role == "label" and name.isdigit() for role, name in t)

    machine.wait_for_unit("multi-user.target")

    with subtest("the shell starts with the module's unit"):
        machine.wait_until_succeeds(f"{LOG} | grep -q 'first frame'", timeout=120)
        journal = machine.succeed(LOG)
        print(journal)
        print(json.dumps(tree()))
        for mark in ["ms: gtk_init", "ms: first frame", "blur on"]:
            assert mark in journal, mark
        assert "no night light: no unit wlsunset.service" in journal, journal
        assert not [n for _, n in tree() if n.startswith("Night light")], tree()
        unit = as_user("systemctl --user cat rust-wl-shell.service")
        assert "GDK_DEBUG" not in unit and "GSK_RENDERER" not in unit, unit

    with subtest("the bar is 28 px high and tints what is under it"):
        shot = screen()
        column = [pixel(shot, 512, y) for y in range(0, 40)]
        tinted = [y for y, p in enumerate(column) if is_tint(p)]
        assert tinted == list(range(HEIGHT)), column
        assert all(is_grey(p) for p in column[HEIGHT:]), column

    with subtest("a user stylesheet restyles the bar, and is followed"):
        css = "/home/${user}/.config/rust-wl-shell/style.css"
        as_user("mkdir -p /home/${user}/.config/rust-wl-shell")

        def write(text, path=css):
            machine.succeed(f"cat > {path}.new <<'EOF'\n{text}\nEOF\nmv {path}.new {path}")

        def bar_is(check, what):
            column = lambda: [pixel(screen(), 512, y) for y in range(2, HEIGHT - 2)]
            with machine.nested(f"waiting for the bar {what}"):
                retry(lambda _: all(check(p) for p in column()), 30)

        write("window { --tint: rgba(200, 0, 0, 0.5); }")
        bar_is(lambda p: p[0] > p[2] + 60, "red")
        write("window { --tint: rgba(0, 0, 200, 0.5); }")
        bar_is(lambda p: p[2] > p[0] + 60, "blue")
        write("window { --tint: rgba(0, 200, 0, 0.5); } @@@ {")
        bar_is(is_tint, "tinted as built in, the broken stylesheet left out")
        machine.wait_until_succeeds(f"{LOG} | grep -q 'the built-in look stays'", timeout=10)
        restarts = as_user("systemctl --user show -p NRestarts --value rust-wl-shell").strip()
        assert restarts == "0", f"the shell restarted {restarts} times"
        # A warning (an empty declaration) is logged; the rest stays applied.
        write("window { --tint: rgba(0, 0, 200, 0.5);; }")
        bar_is(lambda p: p[2] > p[0] + 60, "blue, despite a warning")
        machine.wait_until_succeeds(f"{LOG} | grep -q 'Empty declaration'", timeout=10)
        # As home-manager replaces it: a symlink renamed over it.
        write("window { --tint: rgba(200, 0, 0, 0.5); }", "/home/${user}/linked.css")
        machine.succeed(f"ln -s /home/${user}/linked.css {css}.link && mv -T {css}.link {css}")
        bar_is(lambda p: p[0] > p[2] + 60, "red, from the symlink")
        machine.succeed(f"rm {css}")
        bar_is(is_tint, "tinted as built in again")

    with subtest("the workspaces: a pill for each with windows, and the one shown"):
        # The shown one alone, empty: one pill, blue.
        wait_for_tree(has("button", "Workspace 1"), "the first workspace's pill")
        runs, _ = pills()
        assert which_shown(runs) == [True], runs
        spawn("foot")
        wait_for_tree(has("label", "Foot"), "Foot")
        # The empty workspace 2: a pill only while shown, and wider then.
        assert lacks("button", "Workspace 2")(tree())
        niri("action focus-workspace 2")
        wait_for_tree(has("button", "Workspace 2"), "the second workspace's pill")
        runs, _ = pills()
        assert which_shown(runs) == [False, True], runs
        assert runs[1][1] - runs[1][0] >= runs[0][1] - runs[0][0] + 5, runs
        spawn("foot")
        wait_for_tree(has("label", "Foot"), "Foot on workspace 2")
        # A click switches; the app's name after the pills stays where it was.
        runs, app = pills()
        click_pill(runs[0])
        retry(lambda _: active_workspace() == 1, 10)
        after, app_after = pills()
        assert which_shown(after) == [True, False], after
        assert app_after == app and after[-1][1] == runs[-1][1], (runs, after, app, app_after)
        click_pill(after[1])
        retry(lambda _: active_workspace() == 2, 10)
        again, app_again = pills()
        assert again == runs and app_again == app, (runs, again, app, app_again)
        # Its window closed and left, workspace 2 is gone, and its pill with it.
        niri("action close-window")
        niri("action focus-workspace 1")
        wait_for_tree(lacks("button", "Workspace 2"), "no second workspace's pill")
        runs, _ = pills()
        assert which_shown(runs) == [True], runs

    with subtest("the app name follows focus"):
        wait_for_tree(has("label", "Foot"), "Foot")
        spawn("foot --app-id org.test.Alpha")
        wait_for_tree(has("label", "Alpha Test"), "Alpha Test")
        spawn("foot --app-id NoDesktopFile")
        wait_for_tree(has("label", "NoDesktopFile"), "the app id")
        niri("action focus-column-left")
        wait_for_tree(has("label", "Alpha Test"), "Alpha Test again")
        niri("action focus-workspace 2")
        def no_app(t):
            names = {name for role, name in t if role == "label"}
            apps = {"Foot", "Alpha Test", "NoDesktopFile"}
            return clock_text() in names and not apps & names
        wait_for_tree(no_app, "the bar, with no app on the empty workspace")
        # Kept as an artifact: the screen on the empty workspace.
        machine.send_monitor_command(f"screendump {machine.out_dir}/apps.ppm gpu 0")

    with subtest("the clock shows the VM's time"):
        retry(lambda _: clock_text() in labels(), 5)

    with subtest("the volume: the icon follows, the slider sets it, the OSD shows"):
        wait_for_tree(lambda t: any(n.startswith("Volume ") for _, n in t), "the volume")
        t = tree()
        assert not [n for _, n in t if n.startswith("Brightness")], "a backlight in the VM?"
        assert not osd_shown(t), "an OSD at start-up"
        # From outside the shell: the item follows, and the OSD shows, then hides.
        as_user("wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.30")
        # The OSD first: it shows for 1.5 s only, the item stays.
        glimpse(has("label", "30"), "the OSD with 30")
        wait_for_tree(has("button", "Volume 30%"), "the volume item at 30 %")
        machine.send_monitor_command(f"screendump {machine.out_dir}/osd.ppm gpu 0")
        wait_for_tree(lambda t: not osd_shown(t), "the OSD hidden after 1.5 s")
        # Its popover: Sound, the slider (after its mute button), Output, and a row per
        # available port of the two hardware sinks, the one playing checked.
        click_item(1)
        wait_for_tree(has("slider", "Volume"), "the volume's popover")
        t = tree()
        outputs = [n for r, n in t if r == "radio button"]
        print(f"the outputs: {outputs}")
        # Their ports ("Line Out") are generic: named by their cards, both "Built-in Audio".
        assert outputs == ["Built-in Audio", "Built-in Audio 2"], outputs
        assert len(outputs) >= 2 and not [o for o in outputs if "Virtual" in o], outputs
        # The emulated cards' ports share their names, card and all: told apart by place.
        radios = json.loads(as_user("atspi-tree --radios"))
        assert radios.count(True) == 1, radios
        machine.send_monitor_command(f"screendump {machine.out_dir}/volume.ppm gpu 0")
        lines = popover_lines()
        assert len(lines) == 3 + len(outputs), lines
        # The speaker's icon mutes, and unmutes: a click on the slider row's first white.
        shot = screen()
        y = lines[1][1]
        icon = next(x for x in range(500, 1024) if min(pixel(shot, x, y)) > 180)
        wait_for_tree(has("button", "Mute"), "the mute button")
        click(icon + 6, y)
        retry(lambda _: "yes" in as_user("pactl get-sink-mute @DEFAULT_SINK@"), 10)
        wait_for_tree(has("button", "Unmute"), "the unmute button")
        assert not osd_shown(tree()), "an OSD for the user's own mute"
        click(icon + 6, y)
        retry(lambda _: "no" in as_user("pactl get-sink-mute @DEFAULT_SINK@"), 10)
        # A drag on the slider, to about 80 %: it sets the volume, and shows no OSD.
        first, last = slider_trough(lines[1][1])
        width = last - first
        drag((first + width // 5, lines[1][1]), (first + width * 4 // 5, lines[1][1]))
        retry(lambda _: 70 <= volume() <= 90, 10)
        for _ in range(4):
            assert not osd_shown(tree()), "an OSD for the user's own slider"
            time.sleep(0.5)
        # An output on the other card, made the default: the last row (the first is
        # playing, as the first card's sink is the default).
        default = as_user("pactl get-default-sink").strip()
        assert radios[0], radios
        click(*lines[-1])
        retry(lambda _: as_user("pactl get-default-sink").strip() != default, 10)
        retry(lambda _: keyboard_free(), 10)
        # A switch of output is no change of volume: no OSD.
        for _ in range(4):
            assert not osd_shown(tree()), "an OSD for a switch of output"
            time.sleep(0.5)
        # Opened again, the popover checks the row chosen, and only that one.
        click_item(1)
        wait_for_tree(has("slider", "Volume"), "the volume's popover again")
        radios = json.loads(as_user("atspi-tree --radios"))
        assert radios[-1] and not radios[0], radios
        machine.send_key("esc")
        wait_for_tree(lacks("slider", "Volume"), "the popover closed")
        retry(lambda _: keyboard_free(), 10)
        # Mute from outside: the item says so, and the OSD shows.
        as_user("wpctl set-mute @DEFAULT_AUDIO_SINK@ 1")
        # The OSD first: it shows for 1.5 s only, the item stays.
        glimpse(osd_shown, "the OSD for mute")
        wait_for_tree(has("button", "Volume muted"), "the volume item muted")
        wait_for_tree(lambda t: not osd_shown(t), "the OSD hidden")
        # The server restarted: the item goes, and comes back once it does.
        as_user("systemctl --user restart pipewire-pulse.service")
        machine.wait_until_succeeds(f"test $({LOG} | grep -c 'audio: connected') -ge 2", timeout=30)
        wait_for_tree(has("button", "Volume muted"), "the volume item back")

    with subtest("the keyboard layout switches"):
        wait_for_tree(has("button", "Keyboard layout: English (US)"), "the layout item")
        wait_for_tree(has("label", "EN"), "the layout's short name")
        # Right of it: the clock and the volume (no battery yet, and no power mode item).
        assert not [n for _, n in tree() if n.startswith("Power profile")], "a power item"
        click_item(2)
        wait_for_tree(has("radio button", "German"), "the layouts' popover")
        machine.send_monitor_command(f"screendump {machine.out_dir}/layouts.ppm gpu 0")
        # English (US), then German.
        lines = popover_lines()
        assert len(lines) == 2, lines
        click(*lines[1])
        def switched(_):
            return json.loads(niri("--json keyboard-layouts"))["current_idx"] == 1
        retry(switched, 10)
        # xkeyboard-config's short name for German, in upper case.
        wait_for_tree(has("label", "DE"), "the other layout's short name")
        retry(lambda _: keyboard_free(), 10)

    with subtest("the battery follows UPower"):
        battery = "Battery 50%, on battery"
        # Back to the workspace with the apps, so that the keyboard has a window to return to.
        niri("action focus-workspace 1")
        t = tree()
        assert t and not [n for _, n in t if n.startswith("Battery")], t
        machine.succeed("systemctl start fake-upower")
        wait_for_tree(has("button", battery), "the battery")
        click_item(1)
        wait_for_tree(has("label", "1:30 remaining"), "the battery's popover")
        machine.send_monitor_command(f"screendump {machine.out_dir}/battery.ppm gpu 0")
        machine.send_key("esc")
        wait_for_tree(lacks("label", "1:30 remaining"), "the popover closed by Escape")
        retry(lambda _: keyboard_free(), 10)
        # The keyboard is back with the focused window (a foot running alice's shell). The
        # layout is German now: no characters that it moves (y, z, punctuation).
        machine.send_chars("touch focusback\n")
        machine.wait_for_file("/home/${user}/focusback", timeout=10)
        first, last = right_items()[1]
        def icon():
            shot = screen()
            return [pixel(shot, x, y) for x in range(first, last + 1) for y in range(HEIGHT)]
        before = icon()
        machine.succeed("systemctl kill -s USR1 fake-upower")
        wait_for_tree(has("button", "Battery 90%, charging"), "the battery charging")
        assert icon() != before, "the battery's icon did not change"

    with subtest("choosing a power mode in the battery's popover sets it"):
        # Back on the empty workspace: no window's text under the popover.
        niri("action focus-workspace 2")
        click_item(1)
        wait_for_tree(has("radio button", "Power Saver"), "the power modes")
        machine.send_monitor_command(f"screendump {machine.out_dir}/power.ppm gpu 0")
        # 90 %, Charging, the time to full, Power Mode, Performance, Balanced, Power Saver.
        lines = popover_lines()
        assert len(lines) == 7, lines
        # Refused first (as polkit may): the popover, reopened, still shows the profile.
        machine.succeed("touch /run/fake-power-profiles-refuse")
        click(*lines[-1])
        wait_for_tree(lacks("radio button", "Power Saver"), "the popover closed")
        retry(lambda _: keyboard_free(), 10)
        click_item(1)
        wait_for_tree(has("radio button", "Power Saver"), "the power modes again")
        checked = json.loads(as_user("atspi-tree --checked"))
        assert "Performance" in checked and "Power Saver" not in checked, checked
        machine.succeed("rm /run/fake-power-profiles-refuse")
        click(*lines[-1])
        busctl = "busctl get-property org.freedesktop.UPower.PowerProfiles "
        busctl += "/org/freedesktop/UPower/PowerProfiles "
        busctl += "org.freedesktop.UPower.PowerProfiles ActiveProfile"
        machine.wait_until_succeeds(f"{busctl} | grep -q power-saver", timeout=10)
        wait_for_tree(lacks("radio button", "Power Saver"), "the popover closed")
        retry(lambda _: keyboard_free(), 10)
        click_item(1)
        wait_for_tree(has("radio button", "Power Saver"), "the power modes, chosen")
        checked = json.loads(as_user("atspi-tree --checked"))
        assert "Power Saver" in checked and "Performance" not in checked, checked
        # The daemon going and coming back, with the popover open: the rows follow it.
        machine.succeed("systemctl stop fake-power-profiles")
        wait_for_tree(lacks("radio button", "Power Saver"), "the power modes gone")
        assert has("label", "Charging")(tree()), "the popover closed"
        machine.succeed("systemctl start fake-power-profiles")
        wait_for_tree(has("radio button", "Power Saver"), "the power modes back")
        machine.send_key("esc")
        retry(lambda _: keyboard_free(), 10)
        # No battery: no battery item, and no power mode at all.
        machine.succeed("systemctl kill -s USR2 fake-upower")
        wait_for_tree(lacks("button", "Battery 90%, charging"), "no battery present")
        assert not [n for _, n in tree() if n in ("Power Saver", "Performance")], tree()

    with subtest("an output switched off loses its bar, and gets it back"):
        wait_for_tree(lambda t: bars(t) == 2, "two bars")
        # The OSD on Virtual-2 (focused), not on Virtual-1: the middle of its bottom.
        def osd_on(head):
            width, _ = shot = screen(head)
            height = len(shot[1]) // 3 // width
            return not is_grey(pixel(shot, width // 2, height - 48 - 22))
        niri("action focus-monitor-right")
        as_user("wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.55")
        glimpse(has("label", "55"), "the OSD with 55")
        assert osd_on(1) and not osd_on(0), "the OSD not on the focused output"
        wait_for_tree(lambda t: not osd_shown(t), "the OSD hidden")
        niri("output Virtual-2 off")
        machine.wait_until_succeeds(f"{LOG} | grep -q 'output Virtual-2 removed'")
        wait_for_tree(lambda t: bars(t) == 1, "one bar")
        # Its output gone, the OSD shows on the one left.
        as_user("wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.65")
        glimpse(has("label", "65"), "the OSD with 65")
        retry(lambda _: osd_on(0), 5)
        machine.send_monitor_command(f"screendump {machine.out_dir}/osd-moved.ppm gpu 0")
        wait_for_tree(lambda t: not osd_shown(t), "the OSD hidden again")
        niri("output Virtual-2 on")
        # GDK may list the output before its name, so the bar finds its workspaces later.
        wait_for_tree(
            lambda t: bars(t) == 2 and t.count(["button", "Workspace 1"]) == 2,
            "two bars again, each with its workspace",
        )

    def gap_and_outline(shot, x, ys):
        """Column `x` over `ys`, from the bar outward: niri's grey for the gap (6 px), one
        row of outline (darker than the grey), then the popover's opaque background."""
        column = [pixel(shot, x, y) for y in ys]
        edge = column[6]
        ok = all(is_grey(p) for p in column[:6]) and max(edge) < GREY - 8
        return ok and all(is_popover(p) for p in column[7:]), column

    def outlined_from_bar(shot, x, ys):
        """The popover a gap from the bar, outlined across its bar-facing edge (under the
        button, and 20 px either side)."""
        for dx in (-20, 0, 20):
            ok, column = gap_and_outline(shot, x + dx, ys)
            assert ok, (dx, column)

    def lit(shot, first, y):
        """The item's button lit: its plate, 3 px before its first white, lighter than the
        bar's tint."""
        return not is_tint(pixel(shot, first - 3, y))

    with subtest("a popover opens a gap from the bar, opaque, outlined, its button lit"):
        # A striped background (niri's grey would hide a blur).
        spawn("${pkgs.swaybg}/bin/swaybg -i ${stripes} -m tile")
        time.sleep(2)
        first, last = right_items()[1]
        x = (first + last) // 2
        before = screen()
        click_item(1)
        time.sleep(1)
        move(100, 700)
        time.sleep(0.5)
        after = screen()
        machine.send_monitor_command(f"screendump {machine.out_dir}/popover.ppm gpu 0")
        machine.send_key("esc")
        retry(lambda _: keyboard_free(), 10)
        # Opaque, as GNOME apps' popovers: inside it, near its bar-facing edge, its own
        # background over the stripes, and the same pixels over niri's grey (below).
        inside = [(x + dx, HEIGHT + 12) for dx in range(-6, 7)]
        assert len({pixel(before, *p) for p in inside}) > 1, "no stripes under it"
        over_stripes = [pixel(after, *p) for p in inside]
        assert all(is_popover(p) for p in over_stripes), over_stripes
        machine.succeed("pkill swaybg")
        # On niri's grey: the gap, the outline, and the button lit while it is open only.
        time.sleep(1)
        click_item(1)
        time.sleep(1)
        move(100, 700)
        time.sleep(0.5)
        shot = screen()
        assert [pixel(shot, *p) for p in inside] == over_stripes, "not opaque"
        outlined_from_bar(shot, x, range(HEIGHT, HEIGHT + 12))
        assert lit(shot, first, HEIGHT // 2), "not lit while open"
        machine.send_key("esc")
        retry(lambda _: keyboard_free(), 10)
        time.sleep(0.5)
        assert not lit(screen(), first, HEIGHT // 2), "lit once closed"

    with subtest("from a bottom bar, the popover opens upward, a gap from it, outlined"):
        machine.succeed("printf 'position \"bottom\"\\npower false\\n' > /tmp/bottom.kdl")
        machine.succeed(
            PREFIX + "sh -c 'WAYLAND_DISPLAY=$(basename $(ls ${runtimeDir}/wayland-?)) "
            + "NIRI_SOCKET=$(ls ${runtimeDir}/niri*.sock) "
            + "/etc/profiles/per-user/${user}/bin/rust-wl-shell --config /tmp/bottom.kdl "
            + ">/tmp/bottom.log 2>&1 &'"
        )
        # At the default height, 24 px.
        bottom_height = 24
        top = 768 - bottom_height
        def bottom_items():
            move(512, 400)
            time.sleep(0.5)
            shot = screen()
            bright = [
                x for x in range(1023, 600, -1)
                if any(min(pixel(shot, x, y)) > 180 for y in range(top + 4, 768 - 4))
            ]
            return bright
        retry(lambda _: len(bottom_items()) > 20, 30)
        # The clock's first white, then the item before it: the first white after a gap.
        bright = bottom_items()
        gap = next(i for i in range(1, len(bright)) if bright[i - 1] - bright[i] >= 10)
        last = bright[gap]
        first = next(
            (bright[i - 1] for i in range(gap + 1, len(bright)) if bright[i - 1] - bright[i] >= 10),
            bright[-1],
        )
        x = (first + last) // 2
        click(x, top + bottom_height // 2)
        time.sleep(1)
        move(100, 300)
        time.sleep(0.5)
        shot = screen()
        machine.send_monitor_command(f"screendump {machine.out_dir}/popover-bottom.ppm gpu 0")
        outlined_from_bar(shot, x, range(top - 1, top - 13, -1))
        assert lit(shot, first, top + bottom_height // 2), "not lit while open"
        machine.send_key("esc")
        time.sleep(1)
        assert not lit(screen(), first, top + bottom_height // 2), "lit once closed"
        machine.succeed("pkill -f bottom.kdl")

    with subtest("niri's IPC closing ends the shell"):
        machine.succeed(f"{PREFIX} ${fakeNiri} ${runtimeDir}/fake-niri.sock >/dev/null 2>&1 &")
        machine.wait_until_succeeds("test -S ${runtimeDir}/fake-niri.sock")
        status, out = machine.execute(
            PREFIX + "sh -c 'WAYLAND_DISPLAY=$(basename $(ls ${runtimeDir}/wayland-?)) "
            + "NIRI_SOCKET=${runtimeDir}/fake-niri.sock /etc/profiles/per-user/${user}/bin/rust-wl-shell 2>&1'",
            timeout=30,
        )
        print(out)
        assert status == 1, status
        assert "niri closed its event stream" in out, out
  '';
}
