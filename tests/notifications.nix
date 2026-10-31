# NixOS VM test: the notification server, its banners and the clock popover's history, in
# the session of session.nix, with notify-send (libnotify) as the apps, busctl for what it
# does not send, and dbus-monitor to see the signals.
#
# Behaviours asserted:
#   1. the shell owns org.freedesktop.Notifications: its server information and
#      capabilities
#   2. a banner at the top right of the focused output, below the bar, with the app's
#      name, the summary and the body's markup; three at most, newest on top
#   3. its timeout: 5 s by default; "never" (0) capped (here at 2 s) for a non-critical
#      one; a critical one stays; each leaves for the history (a transient one does not)
#   4. replaces_id replaces, in the banner and the history
#   5. an app's CloseNotification removes the banner and the history entry (closed, 3)
#   6. a click runs the default action, with an ActivationToken first; a button its
#      action; both reach the app (notify-send --wait) and close the notification
#   7. a swipe dismisses a banner: a drag, and a horizontal scroll (closed, 2)
#   8. the history in the clock's popover, grouped by app; × and "Clear all"; the
#      keyboard released when it closes
#  10. a malformed image-data is left out (the banner shown without it, the shell still
#      running); a replaced banner gets its whole time; the banners hide while the
#      clock's popover is open, and come back; meanwhile, the shell's AT-SPI root asked
#      for a window past those shown (as a client with a stale count asks) answers, and
#      the shell does not crash (GTK 4.22 answers with its last window, which a hidden
#      banner's window, its accessible unrealized, crashed it)
#   9. Do Not Disturb: no banners (but critical ones), the history still fills; a
#      transient one closed after its id is sent (notify-send --wait returns); it and
#      the history survive a restart of the shell
#  11. with both outputs off (no bar left), the shell's AT-SPI root asked for windows 0
#      and 99 does not crash the shell (it keeps no hidden window: GTK 4.22 answers an
#      index past its shown windows with its last one, which crashed it if hidden)

{
  pkgs,
  homeManager,
  module,
}:

let
  session = import ./session.nix { inherit pkgs homeManager module; };
in
pkgs.testers.runNixOSTest {
  name = "notifications";

  nodes.machine = {
    imports = [
      (session.machine {
        height = 28;
        spacing = 8;
        clock-format = "%H:%M %d/%m";
        notification-max-timeout = 2;
      })
    ];
    # gdbus, for a Notify notify-send cannot send.
    environment.systemPackages = [
      pkgs.libnotify
      pkgs.glib
    ];
    # A sideways scroll, which QEMU's tablet cannot send: ydotool's uinput mouse.
    programs.ydotool.enable = true;
  };

  testScript = ''
    ${session.helpers}
    NOTIFY = "notify-send -a Mail "
    CALL = "busctl --user call org.freedesktop.Notifications /org/freedesktop/Notifications org.freedesktop.Notifications "

    def banner_shown(summary):
        return lambda t: ["label", summary] in t

    def banner_gone(summary):
        return lambda t: bool(t) and ["label", summary] not in t

    def background(command, output):
        """`command`, as a unit of alice's, its output in `output`."""
        as_user(f"systemd-run --user --collect -p StandardOutput=file:{output} {command}")

    def signals():
        return machine.succeed("cat /tmp/signals")

    def history():
        """The clock's popover open: its labels."""
        act("Clock", "toggle button")
        wait_for_tree(has("switch", "Do Not Disturb"), "the clock's popover")
        return [n for r, n in tree() if r == "label"]

    def close_history():
        act("Clock", "toggle button")
        typing_reaches_the_window("x" + str(time.time_ns()))

    machine.wait_for_unit("multi-user.target")

    with subtest("the shell is the notification server"):
        machine.wait_until_succeeds(f"{LOG} | grep -q 'first frame'", timeout=120)
        spawn("foot")
        wait_for_tree(has("label", "Foot"), "a foot window")
        info = as_user(CALL + "GetServerInformation")
        assert "rust-wl-shell" in info, info
        capabilities = as_user(CALL + "GetCapabilities")
        for c in ["actions", "body", "body-markup", "icon-static", "persistence"]:
            assert c in capabilities, capabilities
        # Pango's markup has no links: the server does not offer them.
        assert "body-hyperlinks" not in capabilities, capabilities
        background("dbus-monitor --session type=signal,interface=org.freedesktop.Notifications", "/tmp/signals")

    with subtest("a malformed image is left out, and the shell goes on"):
        # 100x100 RGBA, rows of 400 bytes, but only 3 bytes of pixels.
        hints = "{'image-data': <(100, 100, 400, true, 8, 4, [byte 1, 2, 3])>}"
        as_user(
            "gdbus call --session --dest org.freedesktop.Notifications "
            + "--object-path /org/freedesktop/Notifications "
            + f"--method org.freedesktop.Notifications.Notify Bad 0 \"\" 'Bad image' body '[]' \"{hints}\" 3000"
        )
        wait_for_tree(banner_shown("Bad image"), "the banner, without its image")
        as_user("systemctl --user is-active rust-wl-shell")
        assert machine.succeed(f"{LOG} | grep -c 'first frame'").strip() == "1", "restarted"
        # Nor handed to GTK, which refuses it (a NULL texture, wrapped as if it were one).
        machine.fail(f"{LOG} | grep -q 'gdk_memory_texture_new: assertion'")
        wait_for_tree(banner_gone("Bad image"), "its time up")

    with subtest("banners: top right, newest on top, three at most, with the markup"):
        as_user(NOTIFY + "-i mail-unread-symbolic 'Lunch?' 'Free at <b>one</b>?'")
        wait_for_tree(banner_shown("Lunch?"), "the banner")
        t = tree()
        assert has("label", "Mail")(t) and has("label", "Free at one?")(t), t
        move(100, 700)
        time.sleep(0.5)
        shot = screen()
        machine.send_monitor_command(f"screendump {machine.out_dir}/banner.ppm gpu 0")
        # Below the bar, 8 px from the right edge: tinted, not niri's grey.
        assert not is_grey(pixel(shot, 1024 - 8 - 180, HEIGHT + 30)), "no banner"
        assert is_grey(pixel(shot, 1024 - 4, HEIGHT + 30)), "the banner at the edge"
        for summary in ["Two", "Three"]:
            as_user(NOTIFY + f"-t 20000 {summary} body")
        # The newest, taller with its buttons.
        background("notify-send -a Mail -A yes=Yes -A no=No -t 20000 Four body", "/tmp/four")
        wait_for_tree(banner_shown("Four"), "the fourth banner")
        t = tree()
        shown = {n for _, n in t if n in ("Lunch?", "Two", "Three", "Four")}
        assert shown == {"Four", "Three", "Two"}, shown
        machine.send_monitor_command(f"screendump {machine.out_dir}/stack.ppm gpu 0")
        # On top, the tallest: the first gap between banners comes late.
        move(100, 700)
        time.sleep(0.5)
        shot = screen()
        x = 1024 - 8 - 180
        gap = next(y for y in range(HEIGHT + 10, 700) if is_grey(pixel(shot, x, y)))
        print(f"the first gap at {gap}")
        assert gap - (HEIGHT + 8) > 90, gap
        assert "Lunch?" in history()
        machine.send_monitor_command(f"screendump {machine.out_dir}/history.ppm gpu 0")
        close_history()
        act("Clock", "toggle button")
        wait_for_tree(has("button", "Clear all"), "Clear all")
        act("Clear all")
        wait_for_tree(lambda t: has("label", "No notifications")(t) and banner_gone("Four")(t), "all cleared")
        close_history()

    with subtest("timeouts: 5 s, never capped at 2 s, critical stays, transient not kept"):
        # Each timed alike, from before it is sent to its banner seen gone (the tree lags
        # by a second or two): "never" capped at 2 s, the default 5 s.
        def lifetime(options, summary):
            start = time.time()
            as_user(NOTIFY + f"{options} {summary} body")
            wait_for_tree(banner_shown(summary), summary)
            wait_for_tree(banner_gone(summary), f"{summary} gone")
            return time.time() - start
        never, default = lifetime("-t 0", "Never"), lifetime("", "Default")
        print(f"never {never:.1f} s, default {default:.1f} s")
        assert default >= 5 and default - never >= 2, (never, default)
        as_user(NOTIFY + "-u critical -t 0 Critical body")
        as_user(NOTIFY + "-e -t 1000 Transient body")
        wait_for_tree(banner_shown("Critical"), "critical")
        time.sleep(6)
        t = tree()
        assert banner_shown("Critical")(t), "critical gone"
        assert banner_gone("Transient")(t), t
        labels = history()
        assert "Default" in labels and "Never" in labels and "Transient" not in labels, labels
        close_history()
        # The transient one, gone from both, was closed as expired (1).
        assert re.search(r"NotificationClosed\s+uint32 \d+\s+uint32 1", signals()), signals()

    with subtest("a replaced banner has its whole time, the old one's timer gone"):
        nid = as_user(NOTIFY + "-p -t 4000 Short body").strip()
        wait_for_tree(banner_shown("Short"), "the first")
        time.sleep(3)
        as_user(NOTIFY + f"-r {nid} -t 4000 Longer body")
        wait_for_tree(banner_shown("Longer"), "the replacement")
        # Past the old banner's 4 s, well before the new one's.
        time.sleep(2)
        assert banner_shown("Longer")(tree()), "cut short by the old timer"
        wait_for_tree(banner_gone("Longer"), "its own time up")

    with subtest("the banners hide while the clock's popover is open"):
        def banner_layers():
            layers = json.loads(niri("--json layers"))
            return [l for l in layers if l["namespace"] == "rust-wl-shell-notification"]
        behind = as_user(NOTIFY + "-p -t 30000 Behind body").strip()
        wait_for_tree(banner_shown("Behind"), "the banner")
        assert banner_layers(), "no banner layer"
        history()
        retry(lambda _: not banner_layers(), 10)
        # A client asking the shell's root for a window past those shown (its count
        # stale, the banners just hidden): GTK 4.22 answers with its last window, which
        # must not be a hidden one (the shell crashed on its missing path).
        print(as_user("atspi-tree --root-child 99"))
        restarts = as_user("systemctl --user show -p NRestarts --value rust-wl-shell").strip()
        assert restarts == "0", f"the shell restarted {restarts} times"
        close_history()
        retry(lambda _: bool(banner_layers()), 10)
        as_user(CALL + f"CloseNotification u {behind}")

    with subtest("replaces_id, and an app's CloseNotification"):
        nid = as_user(NOTIFY + "-p -t 20000 Old body").strip()
        wait_for_tree(banner_shown("Old"), "the first")
        as_user(NOTIFY + f"-r {nid} -t 20000 New body")
        wait_for_tree(lambda t: banner_shown("New")(t) and banner_gone("Old")(t), "replaced")
        labels = history()
        # In the history only: the banners are hidden while it is open.
        assert labels.count("New") == 1 and "Old" not in labels, labels
        close_history()
        as_user(CALL + f"CloseNotification u {nid}")
        wait_for_tree(banner_gone("New"), "closed by its app")
        assert "New" not in history()
        close_history()
        assert re.search(rf"NotificationClosed\s+uint32 {nid}\s+uint32 3", signals()), signals()

    with subtest("a click runs the default action, with a token; a button its own"):
        background("notify-send -a Chat -A default=Open -A yes=Yes --wait -t 20000 Click body", "/tmp/clicked")
        wait_for_tree(banner_shown("Click"), "the banner")
        # Its body, not its ×.
        click(1024 - 8 - 300, HEIGHT + 8 + 40)
        machine.wait_until_succeeds("grep -q default /tmp/clicked", timeout=10)
        wait_for_tree(banner_gone("Click"), "closed once acted on")
        assert re.search(r"ActivationToken\s+uint32 \d+\s+string \"\S+\"", signals()), signals()
        background("notify-send -a Chat -A default=Open -A yes=Yes --wait -t 20000 Button body", "/tmp/button")
        wait_for_tree(has("button", "Yes"), "the button")
        act("Yes")
        machine.wait_until_succeeds("grep -q yes /tmp/button", timeout=10)

    with subtest("a swipe dismisses: a drag, and a horizontal scroll"):
        nid = as_user(NOTIFY + "-p -t 20000 Dragged body").strip()
        wait_for_tree(banner_shown("Dragged"), "the banner")
        y = HEIGHT + 8 + 40
        # A drag to the right, kept as a screen halfway.
        x = 1024 - 8 - 300
        move(x, y)
        input_events({"type": "btn", "data": {"down": True, "button": "left"}})
        for i in range(1, 11):
            move(x + 16 * i, y)
            time.sleep(0.05)
            if i == 5:
                time.sleep(0.3)
                machine.send_monitor_command(f"screendump {machine.out_dir}/swipe.ppm gpu 0")
        input_events({"type": "btn", "data": {"down": False, "button": "left"}})
        wait_for_tree(banner_gone("Dragged"), "swiped by a drag")
        assert re.search(rf"NotificationClosed\s+uint32 {nid}\s+uint32 2", signals()), signals()
        assert "Dragged" not in history()
        close_history()
        as_user(NOTIFY + "-t 20000 Scrolled body")
        wait_for_tree(banner_shown("Scrolled"), "the banner")
        move(1024 - 8 - 200, y)
        machine.wait_for_unit("ydotoold.service")
        for _ in range(5):
            machine.succeed("ydotool mousemove --wheel -x 1 -y 0")
            time.sleep(0.05)
        wait_for_tree(banner_gone("Scrolled"), "swiped by a scroll")

    with subtest("Do Not Disturb, kept with the history over a restart"):
        history()
        act("Do Not Disturb", "switch")
        machine.wait_until_succeeds("grep -q '\"dnd\":true' /home/${session.user}/.local/state/rust-wl-shell/notifications.json")
        close_history()
        as_user(NOTIFY + "-t 20000 Quiet body")
        time.sleep(2)
        assert banner_gone("Quiet")(tree()), "a banner with Do Not Disturb"
        # A transient one, neither shown nor kept, is closed once its id is sent: an app
        # waiting for the close gets it.
        start = time.time()
        as_user("timeout 10 " + NOTIFY + "--wait -e Fleeting body")
        assert time.time() - start < 5, time.time() - start
        as_user(NOTIFY + "-u critical Urgent body")
        wait_for_tree(banner_shown("Urgent"), "a critical banner, with Do Not Disturb")
        assert "Quiet" in history()
        close_history()
        as_user("systemctl --user restart rust-wl-shell")
        machine.wait_until_succeeds(f"{LOG} | grep -c 'first frame' | grep -q 2", timeout=60)
        wait_for_tree(lambda t: any(n.startswith("Clock") for _, n in t), "the shell again")
        labels = history()
        assert "Quiet" in labels and "Urgent" in labels, labels
        close_history()
        as_user(NOTIFY + "-t 20000 'Still quiet' body")
        time.sleep(2)
        assert banner_gone("Still quiet")(tree()), "Do Not Disturb forgotten"
        assert "Still quiet" in history()
        close_history()

    with subtest("with no output, the shell's AT-SPI root answers, and the shell runs on"):
        # No bar left: GTK's last window would be a hidden one, if the shell kept any
        # (the OSD's, the dialog's), and its AT-SPI root crashed on that.
        niri("output Virtual-1 off")
        niri("output Virtual-2 off")
        wait_for_tree(lambda t: bool(t) and bars(t) == 0, "no bar")
        for index in (0, 99):
            print(as_user(f"atspi-tree --root-child {index}"))
        restarts = as_user("systemctl --user show -p NRestarts --value rust-wl-shell").strip()
        assert restarts == "0", f"the shell restarted {restarts} times"
        niri("output Virtual-1 on")
        niri("output Virtual-2 on")
        wait_for_tree(lambda t: bars(t) == 2, "two bars again")
  '';
}
