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
  '';

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
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi  # noqa: E402


        def walk(node):
            yield [node.get_role_name(), node.get_name()]
            for i in range(node.get_child_count()):
                child = node.get_child_at_index(i)
                if child is not None:
                    yield from walk(child)


        desktop = Atspi.get_desktop(0)
        count = desktop.get_child_count()
        apps = [desktop.get_child_at_index(i) for i in range(count)]
        bar = [list(walk(a)) for a in apps if a.get_name() == "rust-wl-shell"]
        print(json.dumps(bar[0] if bar else []))
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
    ];
    systemd.packages = [ niri ];
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
            retry(lambda _: check(tree()), 30)

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
        wait_for_tree(lambda t: ["button", "Workspace 2"] in t, "a second workspace's dot")
        assert active_workspace() == 1
        click(*dot(2))
        retry(lambda _: active_workspace() == 2, 10)
        click(*dot(1))
        retry(lambda _: active_workspace() == 1, 10)

    with subtest("the app name follows focus"):
        wait_for_tree(lambda t: ["label", "Foot"] in t, "Foot")
        spawn("foot --app-id org.test.Alpha")
        wait_for_tree(lambda t: ["label", "Alpha Test"] in t, "Alpha Test")
        spawn("foot --app-id NoDesktopFile")
        wait_for_tree(lambda t: ["label", "NoDesktopFile"] in t, "the app id")
        niri("action focus-column-left")
        wait_for_tree(lambda t: ["label", "Alpha Test"] in t, "Alpha Test again")
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

    with subtest("an output switched off loses its bar, and gets it back"):
        wait_for_tree(lambda t: bars(t) == 2, "two bars")
        niri("output Virtual-2 off")
        machine.wait_until_succeeds(f"{LOG} | grep -q 'output Virtual-2 removed'")
        wait_for_tree(lambda t: bars(t) == 1, "one bar")
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
