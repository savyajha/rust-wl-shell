# The session both VM tests run the shell in: alice logged in on tty1 by getty's autologin,
# her login shell starting niri-session, which runs niri on its tty backend as her user
# unit and starts graphical-session.target, which starts the shell's unit (through the
# home-manager module). niri's background is plain grey, so the bar's tint shows in the
# pixels. Two outputs: Virtual-1 (1024x768), whose screen is read, and Virtual-2 (800x600)
# to its right. With the test script's helpers: the pointer, the screen, the AT-SPI tree.
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
        from gi.repository import Atspi, Gio, GLib  # noqa: E402


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
        elif sys.argv[1:2] in (["--extents"], ["--expanded"]):
            # The first node so named: its place in its window and its size, or
            # whether it is expanded.
            def found(node):
                if node.get_name() == sys.argv[2]:
                    return node
                for i in range(node.get_child_count()):
                    child = node.get_child_at_index(i)
                    hit = child and found(child)
                    if hit:
                        return hit
                return None
            node = found(bar[0])
            if sys.argv[1] == "--expanded":
                states = node.get_state_set()
                print(json.dumps(states.contains(Atspi.StateType.EXPANDED)))
            else:
                box = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                print(json.dumps([box.x, box.y, box.width, box.height]))
        elif sys.argv[1:2] == ["--root-child"]:
            # The shell's root's child at an index, asked on the a11y bus directly (an
            # index past its shown windows, as a client whose count is stale asks).
            session = Gio.bus_get_sync(Gio.BusType.SESSION)
            address = session.call_sync(
                "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus", "GetAddress",
                None, None, Gio.DBusCallFlags.NONE, -1).unpack()[0]
            flags = (Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
                     | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION)
            a11y = Gio.DBusConnection.new_for_address_sync(address, flags)
            try:
                reply = a11y.call_sync(
                    bar[0].app.bus_name, "/org/a11y/atspi/accessible/root",
                    "org.a11y.atspi.Accessible", "GetChildAtIndex",
                    GLib.Variant("(i)", (int(sys.argv[2]),)), None,
                    Gio.DBusCallFlags.NONE, 5000)
                print(json.dumps(reply.unpack()))
            except GLib.Error as e:
                # No window at all: GTK 4.22 leaves the call unanswered.
                print(json.dumps(str(e)))
        elif sys.argv[1:2] in (["--checked"], ["--focused"], ["--do"]):
            checked = Atspi.StateType.CHECKED
            focused = Atspi.StateType.FOCUSED

            def nodes(node):
                yield node
                for i in range(node.get_child_count()):
                    child = node.get_child_at_index(i)
                    if child is not None:
                        yield from nodes(child)

            if sys.argv[1] in ("--checked", "--focused"):
                # The names of the checked nodes (the rows chosen), or of the
                # focused ones.
                state = checked if sys.argv[1] == "--checked" else focused
                print(json.dumps([
                    n.get_name() for n in nodes(bar[0])
                    if n.get_state_set().contains(state)
                ]))
            else:
                # The first action (a click) of the first node so named
                # (and of that role, if given) that has one.
                role = sys.argv[3] if len(sys.argv) > 3 else None
                actions = (
                    n.get_action_iface() for n in nodes(bar[0])
                    if n.get_name() == sys.argv[2]
                    and role in (None, n.get_role_name())
                )
                next(a for a in actions if a and a.get_n_actions()).do_action(0)
        else:
            print(json.dumps(list(walk(bar[0])) if bar else []))
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
{
  inherit user runtimeDir niri;

  # The machine, the shell's config set to `settings`.
  machine = settings: {
    imports = [ homeManager ];

    users.users.${user} = {
      isNormalUser = true;
      inherit uid;
      # NetworkManager's polkit rules let its group change the network.
      extraGroups = [ "networkmanager" ];
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
      atspiTree
      pkgs.adwaita-icon-theme
    ];
    systemd.packages = [ niri ];
    hardware.graphics.enable = true;
    fonts.packages = [ pkgs.adwaita-fonts ];
    services.gnome.at-spi2-core.enable = true;
    programs.dconf.enable = true;

    # The shell in /etc/profiles/per-user, for runs of its own.
    home-manager.useUserPackages = true;
    home-manager.users.${user} = {
      imports = [ module ];
      home.stateVersion = "26.05";
      xdg.configFile."niri/config.kdl".text = niriConfig;
      programs.rust-wl-shell = {
        enable = true;
        inherit settings;
      };
    };

    virtualisation.memorySize = 2048;
    virtualisation.qemu.options = [
      "-vga none"
      "-device ${lib.escapeShellArg gpu}"
    ];
  };

  # The test script's start: the helpers, and the constants they use.
  helpers = ''
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

    def pills():
        """The workspaces' pills, left to right, as [first, last, shown] columns on the
        bar's middle row (faint white, or the accent, blue, for the one shown), and the
        first column of white after them (the app's name). The pointer is moved away
        first, and the animation left to end."""
        move(512, 400)
        time.sleep(0.5)
        shot = screen()
        runs = []
        for x in range(0, 512):
            p = pixel(shot, x, HEIGHT // 2)
            if any(min(pixel(shot, x, y)) > 180 for y in range(4, HEIGHT - 4)):
                return [r for r in runs if r[1] - r[0] >= 4], x
            if p[2] - p[0] > 60:
                kind = True
            elif all(abs(c - 135) <= 15 for c in p):
                # White at 0.3 over the tint.
                kind = False
            else:
                kind = None
            if kind is not None and runs and runs[-1][1:] == [x - 1, kind]:
                runs[-1][1] = x
            elif kind is not None:
                runs.append([x, x, kind])
        return [r for r in runs if r[1] - r[0] >= 4], None

    def which_shown(runs):
        return [r[2] for r in runs]

    def click_pill(run):
        click((run[0] + run[1]) // 2, HEIGHT // 2)

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
        """The open popover's slider at row y: the longest run of pixels neither the
        popover's background nor niri's grey (the track, its accent part and the knob), as
        (first, last) columns."""
        move(512, 700)
        time.sleep(0.5)
        shot = screen()
        runs, run = [], []
        for x in range(500, 1024):
            if not is_popover(pixel(shot, x, y)) and not is_grey(pixel(shot, x, y)):
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

    def act(name, role=""):
        """The first action (a click) of the bar's node named `name` (and of that role),
        through AT-SPI."""
        as_user(f"atspi-tree --do '{name}' '{role}'" if role else f"atspi-tree --do '{name}'")

    def typing_reaches_the_window(word):
        """The keyboard is back with the focused window, a foot running alice's shell."""
        retry(lambda _: keyboard_free(), 10)
        machine.send_chars(f"touch {word}\n")
        machine.wait_for_file(f"/home/${user}/{word}", timeout=10)

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

    def is_popover(p):
        # libadwaita's dark popover background, #36363a (there is no gtk.css here).
        return all(abs(c - b) <= 3 for c, b in zip(p, (0x36, 0x36, 0x3A)))

    def is_tint(p):
        # 0.35 black over grey, blurred or not.
        return all(abs(c - round(GREY * 0.65)) <= 6 for c in p)

  '';
}
