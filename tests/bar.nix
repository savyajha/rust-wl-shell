# NixOS VM test: the bar on niri, on a virtio-gpu (no virgl: Mesa renders in software),
# through the home-manager module, checked by its pixels and its AT-SPI tree.
#
# alice is logged in on tty1 by getty's autologin; her login shell starts niri-session,
# which runs niri on its tty backend as her user unit and starts graphical-session.target,
# which starts the shell's unit. niri's background is plain grey, so the bar's tint shows
# in the pixels. Two outputs: Virtual-1 (1024x768), whose screen is read, and Virtual-2 (800x600) to
# its right, switched off and on.
#
# Behaviours asserted:
#   1. the module's settings (not the defaults) pass `--check` and reach the shell; it
#      starts with the module's unit, logs its start-up marks, and turns blur on
#   2. the bar is 28 px high, as configured, flush with the top, and tints what is under
#      it dark
#   3. one dot per workspace of the output; a click on a dot switches to that workspace
#   4. the app name follows focus: a desktop file's Name, else the app id; none on an
#      empty workspace
#   5. the clock shows the VM's time in the configured format, `%H:%M %d/%m`
#   6. the AT-SPI tree has the bar, its workspace buttons and its labels
#   7. an output switched off loses its bar, and gets one again when switched on
#   8. niri's IPC closing ends the shell with a failure (a fake niri socket)
#   9. the power profile (a fake power-profiles-daemon): a click on the item opens its
#      popover with the three profiles; a refused choice leaves the rows on the daemon's
#      profile; a click on Power Saver sets it on the daemon, closes the popover, and the
#      item follows
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

{
  pkgs,
  homeManager,
  module,
}:

let
  inherit (pkgs) lib;
  user = "alice";
  uid = 1000;
  runtimeDir = "/run/user/${toString uid}";

  # niri refuses software EGL renderers on its tty backend (they can crash importing other
  # GPUs' dmabufs, which this VM has none of), and llvmpipe is all the VM has; llvmpipe has
  # no render node either, so its GPU is filed under the primary render node instead.
  niri = pkgs.niri.overrideAttrs (old: {
    postPatch = (old.postPatch or "") + ''
      substituteInPlace src/backend/tty.rs \
        --replace-fail '!egl_device.is_software()' 'true' \
        --replace-fail '.unwrap_or(node);' '.unwrap_or(self.primary_render_node);'
    '';
    doCheck = false;
  });

  niriConfig = ''
    hotkey-overlay { skip-at-startup; }
    animations { off; }
    xwayland-satellite { off; }
    prefer-no-csd
    layout {
        background-color "#808080"
        focus-ring { off; }
        border { off; }
    }
    output "Virtual-1" { scale 1; position x=0 y=0; }
    output "Virtual-2" { scale 1; position x=1024 y=0; }
    input {
        keyboard {
            xkb { layout "us,de"; }
        }
    }
  '';

  fakes = import ./fake-services.nix { inherit pkgs; };

  # An app whose desktop file names it, run as foot with its app id.
  alpha = pkgs.writeTextDir "share/applications/org.test.Alpha.desktop" ''
    [Desktop Entry]
    Type=Application
    Name=Alpha Test
    Exec=foot --app-id org.test.Alpha
  '';

  # The bar's AT-SPI tree, as (role, name) pairs, depth first.
  atspiTree =
    pkgs.writers.writePython3Bin "atspi-tree"
      {
        libraries = [ pkgs.python3Packages.pygobject3 ];
        makeWrapperArgs = [
          "--prefix GI_TYPELIB_PATH : ${
            lib.makeSearchPath "lib/girepository-1.0" [
              pkgs.at-spi2-core
              pkgs.gobject-introspection
            ]
          }"
        ];
      }
      ''
        import json
        import sys
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi, GLib  # noqa: E402


        def walk(node):
            try:
                yield [node.get_role_name(), node.get_name()]
                count = node.get_child_count()
                children = [node.get_child_at_index(i) for i in range(count)]
            except GLib.Error:
                # Gone while walked (an OSD hiding, a popover closing): left out.
                return
            for child in children:
                if child is not None:
                    yield from walk(child)


        desktop = Atspi.get_desktop(0)
        count = desktop.get_child_count()
        apps = [desktop.get_child_at_index(i) for i in range(count)]
        bar = [a for a in apps if a.get_name() == "rust-wl-shell"]
        if sys.argv[1:] == ["--radios"]:
            # Whether each radio button (each row) is checked, in order.
            checked = Atspi.StateType.CHECKED

            def radios(node):
                if node.get_role_name() == "radio button":
                    yield node.get_state_set().contains(checked)
                for i in range(node.get_child_count()):
                    child = node.get_child_at_index(i)
                    if child is not None:
                        yield from radios(child)

            print(json.dumps(list(radios(bar[0]))))
        elif sys.argv[1:] == ["--checked"]:
            # The names of the checked nodes: the radio rows chosen.
            checked = Atspi.StateType.CHECKED

            def nodes(node):
                yield node
                for i in range(node.get_child_count()):
                    child = node.get_child_at_index(i)
                    if child is not None:
                        yield from nodes(child)

            print(json.dumps([
                n.get_name() for n in nodes(bar[0])
                if n.get_state_set().contains(checked)
            ]))
        else:
            print(json.dumps(list(walk(bar[0])) if bar else []))
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

  gpu = builtins.toJSON {
    driver = "virtio-gpu-pci";
    id = "gpu";
    max_outputs = 2;
    outputs = [
      {
        name = "left";
        xres = 1024;
        yres = 768;
      }
      {
        name = "right";
        xres = 800;
        yres = 600;
      }
    ];
  };
in
pkgs.testers.runNixOSTest {
  name = "bar";

  nodes.machine = {
    imports = [ homeManager ];

    users.users.${user} = {
      isNormalUser = true;
      inherit uid;
    };
    services.getty.autologinUser = user;
    environment.loginShellInit = ''
      # niri-session re-runs itself in a login shell, which reads this again.
      if [ "$(tty)" = /dev/tty1 ] && [ -z "$NIRI_STARTED" ]; then
        export NIRI_STARTED=1
        exec niri-session
      fi
    '';
    environment.systemPackages = [
      niri
      pkgs.foot
      alpha
      atspiTree
      pkgs.adwaita-icon-theme
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
    systemd.packages = [ niri ];

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
    hardware.graphics.enable = true;
    fonts.packages = [ pkgs.adwaita-fonts ];
    services.gnome.at-spi2-core.enable = true;
    programs.dconf.enable = true;

    # The shell in /etc/profiles/per-user, for the run with a fake niri.
    home-manager.useUserPackages = true;
    home-manager.users.${user} = {
      imports = [ module ];
      home.stateVersion = "26.05";
      xdg.configFile."niri/config.kdl".text = niriConfig;
      programs.rust-wl-shell = {
        enable = true;
        # Not the defaults, so the bar's height and clock show they reached it.
        settings = {
          height = 28;
          spacing = 8;
          dot-size = 6;
          clock-format = "%H:%M %d/%m";
        };
      };
    };

    virtualisation.memorySize = 2048;
    virtualisation.qemu.options = [
      "-vga none"
      "-device ${lib.escapeShellArg gpu}"
      "-audiodev none,id=sound1"
      "-audiodev none,id=sound2"
      "-device ich9-intel-hda,id=hda1"
      "-device hda-output,bus=hda1.0,audiodev=sound1"
      "-device intel-hda,id=hda2"
      "-device hda-output,bus=hda2.0,audiodev=sound2"
    ];
  };

  testScript = ''
    import json
    import re
    ${builtins.readFile ./screen.py}

    # The tablet's range: both outputs, side by side.
    SCREEN = (1024 + 800, 768)
    HEIGHT = 28
    CLOCK = "%H:%M %d/%m"
    GREY = 0x80
    PREFIX = "sudo -u ${user} XDG_RUNTIME_DIR=${runtimeDir} DBUS_SESSION_BUS_ADDRESS=unix:path=${runtimeDir}/bus "
    LOG = "journalctl --no-pager -o cat _SYSTEMD_USER_UNIT=rust-wl-shell.service"

    def as_user(command):
        return machine.succeed(PREFIX + command)

    def niri(command):
        return as_user(f"sh -c 'NIRI_SOCKET=$(ls ${runtimeDir}/niri*.sock) niri msg {command}'")

    def tree():
        return json.loads(as_user("atspi-tree"))

    def wait_for_tree(check, what):
        with machine.nested(f"waiting for {what} in the AT-SPI tree"):
            try:
                retry(lambda _: check(tree()), 30)
            except Exception:
                print(json.dumps(tree()))
                raise

    def has(role, name):
        return lambda t: [role, name] in t

    def lacks(role, name):
        """The bar is in the tree, without that node."""
        return lambda t: bool(t) and [role, name] not in t

    def keyboard_free():
        """The bars ask for no keyboard (as when no popover is open)."""
        layers = json.loads(niri("--json layers"))
        ours = [layer for layer in layers if layer["namespace"] == "rust-wl-shell"]
        return ours and all(layer["keyboard_interactivity"] == "None" for layer in ours)

    def bars(t):
        return len([role for role, _ in t if role in ("frame", "window")])

    def labels():
        return [name for role, name in tree() if role == "label"]

    def active_workspace():
        workspaces = json.loads(niri("--json workspaces"))
        return next(w["idx"] for w in workspaces if w["output"] == "Virtual-1" and w["is_active"])

    def dot(i):
        """The i-th dot's centre (from 1): after the bar's padding (spacing, 8), each button
        is the dot (6) and a quarter of the spacing either side."""
        return (8 + (i - 1) * 10 + 5, HEIGHT // 2)

    def right_items():
        """The bar's right side, right to left, as (first, last) columns of white: the
        clock, then each item; told apart by the gaps between them (at least 10 px). The
        pointer, which niri draws, is moved away first."""
        move(512, 400)
        time.sleep(0.5)
        shot = screen()
        bright = [
            x for x in range(1023, 600, -1)
            if any(min(pixel(shot, x, y)) > 180 for y in range(4, HEIGHT - 4))
        ]
        items = []
        for x in bright:
            if items and items[-1][0] - x < 10:
                items[-1][0] = x
            else:
                items.append([x, x])
        return items

    def popover_lines():
        """The open popover's lines of white text, top to bottom, as (x, y) centres: rows of
        pixels below the bar with white in them, grouped as the right items are."""
        move(512, 700)
        time.sleep(0.5)
        shot = screen()
        rows = []
        for y in range(HEIGHT, 300):
            xs = [x for x in range(500, 1024) if min(pixel(shot, x, y)) > 180]
            if xs and rows and y - rows[-1][1] < 4:
                top, _, left, right = rows[-1]
                rows[-1] = [top, y, min(left, xs[0]), max(right, xs[-1])]
            elif xs:
                rows.append([y, y, xs[0], xs[-1]])
        return [((left + right) // 2, (top + bottom) // 2) for top, bottom, left, right in rows]

    def slider_trough(y):
        """The open popover's slider at row y: the longest run of light pixels (the track,
        its white part and the knob; not niri's grey around the popover), as (first, last)
        columns."""
        move(512, 700)
        time.sleep(0.5)
        shot = screen()
        runs, run = [], []
        for x in range(500, 1024):
            if min(pixel(shot, x, y)) > 110 and not is_grey(pixel(shot, x, y)):
                run.append(x)
            elif run:
                runs.append(run)
                run = []
        longest = max(runs + [run], key=len)
        return longest[0], longest[-1]

    def drag(start, end):
        """A drag with the left button, from `start` to `end`, in steps."""
        move(*start)
        input_events({"type": "btn", "data": {"down": True, "button": "left"}})
        for i in range(1, 11):
            move(start[0] + (end[0] - start[0]) * i // 10, start[1])
            time.sleep(0.05)
        input_events({"type": "btn", "data": {"down": False, "button": "left"}})

    def volume():
        """The default output's volume in percent, as pactl reads it."""
        out = as_user("pactl get-sink-volume @DEFAULT_SINK@")
        return int(re.findall(r"(\d+)%", out)[0])

    def osd_shown(t):
        """The OSD's value, a label of digits alone, is in the tree."""
        return any(role == "label" and name.isdigit() for role, name in t)

    def click_item(i):
        """A click on the i-th item from the right (the clock is 0)."""
        first, last = right_items()[i]
        click((first + last) // 2, HEIGHT // 2)

    def clock_text():
        return machine.succeed(f"date '+{CLOCK}'").strip()

    def spawn(command):
        niri(f"action spawn -- {command}")

    def is_grey(p):
        return all(abs(c - GREY) <= 2 for c in p)

    def is_tint(p):
        # 0.35 black over grey, blurred or not.
        return all(abs(c - round(GREY * 0.65)) <= 6 for c in p)

    machine.wait_for_unit("multi-user.target")

    with subtest("the shell starts with the module's unit"):
        machine.wait_until_succeeds(f"{LOG} | grep -q 'first frame'", timeout=120)
        journal = machine.succeed(LOG)
        print(journal)
        print(json.dumps(tree()))
        for mark in ["ms: gtk_init", "ms: first frame", "blur on"]:
            assert mark in journal, mark
        unit = as_user("systemctl --user cat rust-wl-shell.service")
        assert "GDK_DEBUG" not in unit and "GSK_RENDERER" not in unit, unit

    with subtest("the bar is 28 px high and tints what is under it"):
        shot = screen()
        column = [pixel(shot, 512, y) for y in range(0, 40)]
        tinted = [y for y, p in enumerate(column) if is_tint(p)]
        assert tinted == list(range(HEIGHT)), column
        assert all(is_grey(p) for p in column[HEIGHT:]), column

    with subtest("a click on a dot switches workspace"):
        spawn("foot")
        wait_for_tree(has("button", "Workspace 2"), "a second workspace's dot")
        assert active_workspace() == 1
        click(*dot(2))
        retry(lambda _: active_workspace() == 2, 10)
        click(*dot(1))
        retry(lambda _: active_workspace() == 1, 10)

    with subtest("the app name follows focus"):
        wait_for_tree(has("label", "Foot"), "Foot")
        spawn("foot --app-id org.test.Alpha")
        wait_for_tree(has("label", "Alpha Test"), "Alpha Test")
        spawn("foot --app-id NoDesktopFile")
        wait_for_tree(has("label", "NoDesktopFile"), "the app id")
        niri("action focus-column-left")
        wait_for_tree(has("label", "Alpha Test"), "Alpha Test again")
        click(*dot(2))
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
        wait_for_tree(has("button", "Volume 30%"), "the volume item at 30 %")
        wait_for_tree(has("label", "30"), "the OSD with 30")
        machine.send_monitor_command(f"screendump {machine.out_dir}/osd.ppm gpu 0")
        wait_for_tree(lambda t: not osd_shown(t), "the OSD hidden after 1.5 s")
        # Its popover: Sound, the slider (after its mute button), Output, and a row per
        # available port of the two hardware sinks, the one playing checked.
        click_item(1)
        wait_for_tree(has("slider", "Volume"), "the volume's popover")
        t = tree()
        outputs = [n for r, n in t if r == "radio button"]
        print(f"the outputs: {outputs}")
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
        wait_for_tree(has("button", "Volume muted"), "the volume item muted")
        wait_for_tree(osd_shown, "the OSD for mute")
        wait_for_tree(lambda t: not osd_shown(t), "the OSD hidden")
        # The server restarted: the item goes, and comes back once it does.
        as_user("systemctl --user restart pipewire-pulse.service")
        machine.wait_until_succeeds(f"test $({LOG} | grep -c 'audio: connected') -ge 2", timeout=30)
        wait_for_tree(has("button", "Volume muted"), "the volume item back")

    with subtest("choosing a power profile sets it"):
        item = "Power profile: Performance"
        wait_for_tree(has("button", item), "the power profile")
        wait_for_tree(lambda t: not [n for _, n in t if n.startswith("Battery")], "no battery")
        # Right of it: the clock, the volume, and the battery while there is one (not here).
        click_item(2)
        wait_for_tree(has("radio button", "Power Saver"), "the profiles' popover")
        machine.send_monitor_command(f"screendump {machine.out_dir}/profiles.ppm gpu 0")
        # The title, then Performance, Balanced and Power Saver.
        lines = popover_lines()
        assert len(lines) == 4, lines
        # Refused first (as polkit may): the popover, reopened, still shows the profile.
        machine.succeed("touch /run/fake-power-profiles-refuse")
        click(*lines[3])
        wait_for_tree(lacks("radio button", "Power Saver"), "the popover closed")
        retry(lambda _: keyboard_free(), 10)
        click_item(2)
        wait_for_tree(has("radio button", "Power Saver"), "the profiles' popover again")
        checked = json.loads(as_user("atspi-tree --checked"))
        assert "Performance" in checked and "Power Saver" not in checked, checked
        machine.succeed("rm /run/fake-power-profiles-refuse")
        click(*lines[3])
        busctl = "busctl get-property org.freedesktop.UPower.PowerProfiles "
        busctl += "/org/freedesktop/UPower/PowerProfiles "
        busctl += "org.freedesktop.UPower.PowerProfiles ActiveProfile"
        machine.wait_until_succeeds(f"{busctl} | grep -q power-saver", timeout=10)
        wait_for_tree(has("button", "Power profile: Power Saver"), "the item following")
        wait_for_tree(lacks("radio button", "Power Saver"), "the popover closed")
        retry(lambda _: keyboard_free(), 10)

    with subtest("the keyboard layout switches"):
        wait_for_tree(has("button", "Keyboard layout: English (US)"), "the layout item")
        wait_for_tree(has("label", "EN"), "the layout's short name")
        click_item(3)
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
        machine.succeed("systemctl kill -s USR2 fake-upower")
        wait_for_tree(lacks("button", "Battery 90%, charging"), "no battery present")

    with subtest("an output switched off loses its bar, and gets it back"):
        wait_for_tree(lambda t: bars(t) == 2, "two bars")
        # The OSD on Virtual-2 (focused), not on Virtual-1: the middle of its bottom.
        def osd_on(head):
            width, _ = shot = screen(head)
            height = len(shot[1]) // 3 // width
            return not is_grey(pixel(shot, width // 2, height - 48 - 22))
        niri("action focus-monitor-right")
        as_user("wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.55")
        wait_for_tree(has("label", "55"), "the OSD with 55")
        assert osd_on(1) and not osd_on(0), "the OSD not on the focused output"
        wait_for_tree(lambda t: not osd_shown(t), "the OSD hidden")
        niri("output Virtual-2 off")
        machine.wait_until_succeeds(f"{LOG} | grep -q 'output Virtual-2 removed'")
        wait_for_tree(lambda t: bars(t) == 1, "one bar")
        # Its output gone, the OSD shows on the one left.
        as_user("wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.65")
        wait_for_tree(has("label", "65"), "the OSD with 65")
        retry(lambda _: osd_on(0), 5)
        machine.send_monitor_command(f"screendump {machine.out_dir}/osd-moved.ppm gpu 0")
        wait_for_tree(lambda t: not osd_shown(t), "the OSD hidden again")
        niri("output Virtual-2 on")
        # GDK may list the output before its name, so the bar finds its workspaces later.
        wait_for_tree(
            lambda t: bars(t) == 2 and t.count(["button", "Workspace 1"]) == 2,
            "two bars again, each with its workspace",
        )

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
