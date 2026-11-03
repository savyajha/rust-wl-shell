# NixOS VM test: the notification server, its banners and the Notification Centre (the
# history), in the session of session.nix, with notify-send (libnotify) as the apps,
# busctl for what it does not send, and dbus-monitor to see the signals.
#
# Behaviours asserted:
#   1. the shell owns org.freedesktop.Notifications: its server information and
#      capabilities
#   2. a malformed image-data is left out (the banner shown without it, the shell still
#      running)
#   3. a banner at the top right of the focused output, below the bar, as macOS's: opaque
#      in the popovers' colour, its app's icon in its slot (no app name), the summary and
#      the body's markup; three at most, newest on top, their cards 8 px apart; no action,
#      no button; two actions, two buttons (no "Options"); the history in the
#      Notification Centre (the clock opens it), grouped by app, and "Clear all"
#   4. its timeout: 5 s by default; "never" (0) capped (here at 2 s) for a non-critical
#      one; a critical one stays; each leaves for the history (a transient one does not)
#   5. a replaced banner gets its whole time
#   6. the banners hide while the Notification Centre is open, and come back; meanwhile,
#      the shell's AT-SPI root asked for a window past those shown (as a client with a
#      stale count asks) answers, and the shell does not crash (GTK 4.22 answers with its
#      last window, which a hidden banner's window, its accessible unrealized, crashed it)
#   7. replaces_id replaces, in the banner and the history; an app's CloseNotification
#      removes the banner and the history entry (closed, 3)
#   8. a click runs the default action, with an ActivationToken first; one action, its
#      button alone; two around the default, both buttons; with three, the first's
#      button and "Options", a menu of the other two, one of which reaches the app; the
#      buttons under the text, the x at the card's top-right still; the menu under its
#      button, not the banner's width; long
#      labels ellipsized; the history's cards show no actions; all close the
#      notification; the Options menu takes the keyboard: Escape closes it, and typing
#      reaches the window again
#   9. x shows at the end of a banner's summary line (its top-right, inset by its padding), while
#      hovered, and dismisses it (closed, 2), as does its x named after the summary (or
#      the app, with none), through AT-SPI
#  10. a swipe towards its edge dismisses a banner: a drag, and a horizontal scroll
#      (closed, 2); one short of the distance goes back (a wheel's, though it sends no
#      end and the banner slid from under the pointer; a slow drag's), unless flung
#      (closed, 2); one inwards goes back, however far (a scroll) or fast (a flick)
#  11. the Notification Centre: a solid panel down the output's right, below the bar,
#      the full height; a notification arriving while it is open shows in it, which
#      stays open; a click on the desktop or on a window closes it, as do Escape (and it
#      opens again at once after) and the clock again (which does not open it anew);
#      typing reaches the window after each
#  12. Do Not Disturb: its indicator beside the clock only while on; no banners (but
#      critical ones), the history still fills; a transient one closed after its id is
#      sent (notify-send --wait returns); it and the history survive a restart of the
#      shell
#  13. with both outputs off (no bar left), the shell's AT-SPI root asked for windows 0
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
        """The Notification Centre open: its labels."""
        act("Clock", "button")
        wait_for_tree(has("switch", "Do Not Disturb"), "the Notification Centre")
        return [n for r, n in tree() if r == "label"]

    def close_history():
        act("Clock", "button")
        wait_for_tree(lacks("switch", "Do Not Disturb"), "the Notification Centre closed")
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
        def buttons(t):
            return [n for r, n in t if r in ("button", "toggle button")]
        before = buttons(tree())
        as_user(NOTIFY + "-i mail-unread-symbolic 'Lunch?' 'Free at <b>one</b>?'")
        wait_for_tree(banner_shown("Lunch?"), "the banner")
        t = tree()
        # No app name on a banner, as macOS's: its icon instead. No action, no button but
        # its x.
        assert lacks("label", "Mail")(t) and has("label", "Free at one?")(t), t
        new = buttons(t)
        for n in before:
            new.remove(n)
        assert new == ['Dismiss "Lunch?"'], new
        move(100, 700)
        time.sleep(0.5)
        shot = screen()
        # The card: 8 px (the gap) from the right edge and below the bar, 360 wide.
        left, top = 1024 - 8 - 360, HEIGHT + 8
        # The icon's slot, on the left (40 px, after the padding): the theme's mail icon.
        slot = [pixel(shot, x, y) for x in range(left + 14, left + 52) for y in range(HEIGHT + 20, HEIGHT + 58)]
        assert any(min(p) > 150 for p in slot), "no icon in its slot"
        machine.send_monitor_command(f"screendump {machine.out_dir}/banner.ppm gpu 0")
        # Opaque, in libadwaita's dark popover colour (in its padding, clear of the text);
        # the margin beside it clear, niri's grey.
        assert is_popover(pixel(shot, 1024 - 16, top + 20)), "no opaque banner"
        assert is_grey(pixel(shot, 1024 - 4, top + 20)), "the banner at the edge"
        for summary in ["Two", "Three"]:
            as_user(NOTIFY + f"-t 20000 {summary} body")
        # The newest, its two actions two buttons, with no "Options".
        background("notify-send -a Mail -A yes=Yes -A no=No -t 20000 Four body", "/tmp/four")
        wait_for_tree(banner_shown("Four"), "the fourth banner")
        t = tree()
        assert has("button", "Yes")(t) and has("button", "No")(t), t
        assert lacks("toggle button", "Options")(t), "Options for two actions"
        t = tree()
        shown = {n for _, n in t if n in ("Lunch?", "Two", "Three", "Four")}
        assert shown == {"Four", "Three", "Two"}, shown
        machine.send_monitor_command(f"screendump {machine.out_dir}/stack.ppm gpu 0")
        # Stacked from the top: the first gap between banners after the first one.
        move(100, 700)
        time.sleep(0.5)
        shot = screen()
        x = 1024 - 8 - 180
        gap = next(y for y in range(top, 700) if is_grey(pixel(shot, x, y)))
        print(f"the first gap at {gap}")
        assert 50 < gap - top < 120, gap
        # The next card 8 px (the gap) after it, the room for its x overlapping.
        after = next(y for y in range(gap, 700) if not is_grey(pixel(shot, x, y)))
        assert after - gap == 8, (gap, after)
        assert "Lunch?" in history()
        machine.send_monitor_command(f"screendump {machine.out_dir}/history.ppm gpu 0")
        close_history()
        act("Clock", "button")
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
        # Its own time long enough to outlast the checks below, which take up to 2 s each
        # on a slow host (the AT-SPI tree read), and the old one's 4 s would cut it short.
        as_user(NOTIFY + f"-r {nid} -t 10000 Longer body")
        wait_for_tree(banner_shown("Longer"), "the replacement")
        # Past the old banner's 4 s, well before the new one's.
        time.sleep(2)
        assert banner_shown("Longer")(tree()), "cut short by the old timer"
        wait_for_tree(banner_gone("Longer"), "its own time up")

    with subtest("the banners hide while the Notification Centre is open"):
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
        wait_for_tree(banner_shown("Behind"), "the banner back")
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
        t = tree()
        # Its one action a button; its default, the card's click, none.
        assert lacks("toggle button", "Options")(t) and lacks("button", "Open")(t), t
        act("Yes")
        machine.wait_until_succeeds("grep -q yes /tmp/button", timeout=10)
        # Two, around the default: both buttons, no "Options", no default.
        background("notify-send -a Chat -A a=Alpha -A default=Open -A b=Beta --wait -t 20000 Pair body", "/tmp/pair")
        wait_for_tree(has("button", "Beta"), "the pair's buttons")
        t = tree()
        assert has("button", "Alpha")(t) and lacks("button", "Open")(t), t
        assert lacks("toggle button", "Options")(t), t
        act("Alpha")
        machine.wait_until_succeeds("grep -qx a /tmp/pair", timeout=10)
        # Long labels, ellipsized: the banner keeps its width (its left edge 368 px from
        # the right one: 360 and the gap), its summary still there.
        long = "-A mark='Mark as read for everyone here' -A reply='Reply to all of them now'"
        background(f"notify-send -a Chat {long} --wait -t 20000 Long body", "/tmp/long")
        wait_for_tree(has("label", "Long"), "the long labels' banner")
        move(100, 700)
        time.sleep(0.5)
        shot = screen()
        edge = next(x for x in range(400, 1024) if is_popover(pixel(shot, x, HEIGHT + 8 + 30)))
        assert abs(edge - (1024 - 8 - 360)) <= 2, edge
        assert has("button", "Mark as read for everyone here")(tree()), tree()
        act("Reply to all of them now")
        machine.wait_until_succeeds("grep -qx reply /tmp/long", timeout=10)
        # A short label whole, beside a long body (which, wrapping, would take its width):
        # as wide as beside a short body. Another app's "settings" action shows.
        def settings_width(body):
            background(f"notify-send -a Other -A settings=Settings --wait -t 20000 Short '{body}'", "/tmp/short")
            wait_for_tree(has("button", "Settings"), "its Settings button")
            width = json.loads(as_user("atspi-tree --extents Settings"))[2]
            act("Settings")
            wait_for_tree(lacks("button", "Settings"), "acted on")
            return width
        alone = settings_width("hi")
        beside = settings_width("A long body that wraps over three lines at the least, " * 3)
        print(f"Settings: {alone} px beside a short body, {beside} beside a long one")
        assert beside == alone, (alone, beside)
        # Chromium's own "settings", from a Chromium-based browser: left out.
        hint = "-h string:desktop-entry:brave-browser"
        background(f"notify-send -a Brave {hint} -A settings=Settings -A reply=Reply --wait -t 20000 Site body", "/tmp/site")
        wait_for_tree(has("button", "Reply"), "the site's banner")
        assert lacks("button", "Settings")(tree()), "Chromium's Settings shown"
        act("Reply")
        machine.wait_until_succeeds("grep -qx reply /tmp/site", timeout=10)
        # The history's cards show no actions.
        background("notify-send -a Chat -p -A keep=Keep --wait -t 20000 Kept body", "/tmp/kept")
        wait_for_tree(has("button", "Keep"), "its button")
        kept = machine.succeed("head -1 /tmp/kept").strip()
        assert "Kept" in history(), "not in the history"
        assert lacks("button", "Keep")(tree()), "an action in the history"
        close_history()
        as_user(CALL + f"CloseNotification u {kept}")
        # Three: the first's button, and "Options", a menu of the other two.
        background("notify-send -a Chat -A a=Alpha -A b=Beta -A c=Gamma --wait -t 20000 Menu body", "/tmp/menu")
        wait_for_tree(has("toggle button", "Options"), "Options")
        t = tree()
        assert has("button", "Alpha")(t) and lacks("button", "Beta")(t), t
        # The buttons under the text, the x still at the card's top-right (the banner the
        # top one): shown on hover, inset by the border (1 px) and padding (10 px, 12 px).
        right, top = 1024 - 8, HEIGHT + 8
        move(100, 700)
        time.sleep(0.5)
        away = screen()
        move(right - 300, top + 30)
        time.sleep(0.8)
        over = screen()
        corner = [(x, y) for x in range(right - 60, 1024) for y in range(top - 4, top + 40)]
        changed = [(x, y) for x, y in corner if pixel(away, x, y) != pixel(over, x, y)]
        xs, ys = [x for x, _ in changed], [y for _, y in changed]
        assert changed and abs(max(xs) - (right - 14)) <= 1 and abs(min(ys) - (top + 11)) <= 1, changed[:5]
        act("Options", "toggle button")
        wait_for_tree(has("button", "Beta"), "the actions' menu")
        # The menu under its button (the banner the top one), not the banner's width:
        # its pixels below the card overlap the button's columns.
        move(100, 700)
        time.sleep(0.8)
        shot = screen()
        # In the gap below the card (the next banner 8 px under it): the menu, from the
        # Options button's right edge, at the card's right, as narrow as its rows.
        card = 1024 - 8 - 360
        bottom = next(y for y in range(top + 40, 400) if is_grey(pixel(shot, card + 20, y)))
        xs = [x for x in range(500, 1024) if is_popover(pixel(shot, x, bottom + 3))]
        print(f"the card ends at {bottom}; its menu at {min(xs)}..{max(xs)}")
        assert min(xs) > card + 150 and max(xs) - min(xs) < 300, (min(xs), max(xs))
        assert max(xs) <= card + 360, ("past the card", max(xs))
        assert has("button", "Gamma")(tree()), tree()
        # It has the keyboard: Escape closes it, and typing reaches the window again.
        machine.send_key("esc")
        wait_for_tree(lacks("button", "Beta"), "the menu closed by Escape")
        typing_reaches_the_window("after-options")
        act("Options", "toggle button")
        wait_for_tree(has("button", "Beta"), "the actions' menu again")
        machine.send_monitor_command(f"screendump {machine.out_dir}/options.ppm gpu 0")
        act("Beta", "button")
        machine.wait_until_succeeds("grep -qx b /tmp/menu", timeout=10)
        wait_for_tree(banner_gone("Menu"), "closed once acted on")
        retry(lambda _: keyboard_free(), 10)

    with subtest("x shows at the summary line's end while hovered, and dismisses it"):
        nid = as_user(NOTIFY + "-p -t 20000 Hovered body").strip()
        wait_for_tree(banner_shown("Hovered"), "the banner")
        # The card's top-right corner (it ends 8 px from the edge, below the bar).
        right, top = 1024 - 8, HEIGHT + 8
        corner = [(x, y) for x in range(right - 60, 1024) for y in range(top - 4, top + 50)]
        move(100, 700)
        time.sleep(0.5)
        away = screen()
        move(1024 - 8 - 200, HEIGHT + 40)
        time.sleep(0.8)
        over = screen()
        changed = [(x, y) for x, y in corner if pixel(away, x, y) != pixel(over, x, y)]
        assert len(changed) > 20, "no x on hover"
        # Inside the card, inset by its border (1 px) and padding (10 px, 12 px): 20 px
        # across.
        xs, ys = [x for x, _ in changed], [y for _, y in changed]
        print(f"x from {min(xs)},{min(ys)} to {max(xs)},{max(ys)}")
        assert abs(max(xs) - (right - 1 - 12 - 1)) <= 1, xs
        assert abs(min(ys) - (top + 1 + 10)) <= 1, ys
        assert 18 <= max(xs) - min(xs) <= 21 and 18 <= max(ys) - min(ys) <= 21, (xs, ys)
        # A click on it.
        click((min(xs) + max(xs)) // 2, (min(ys) + max(ys)) // 2)
        wait_for_tree(banner_gone("Hovered"), "dismissed by its x")
        assert re.search(rf"NotificationClosed\s+uint32 {nid}\s+uint32 2", signals()), signals()
        move(100, 700)
        # For a screen reader, each x is named after its notification.
        nid = as_user(NOTIFY + "-p -t 20000 Named body").strip()
        wait_for_tree(has("button", 'Dismiss "Named"'), "its x, by name")
        act('Dismiss "Named"')
        wait_for_tree(banner_gone("Named"), "dismissed through AT-SPI")
        assert re.search(rf"NotificationClosed\s+uint32 {nid}\s+uint32 2", signals()), signals()
        # With no summary, after its app.
        as_user(
            "gdbus call --session --dest org.freedesktop.Notifications "
            + "--object-path /org/freedesktop/Notifications "
            + "--method org.freedesktop.Notifications.Notify Unnamed 0 \"\" \"\" body '[]' '{}' 20000"
        )
        wait_for_tree(has("button", 'Dismiss "Unnamed"'), "its x, after the app")
        act('Dismiss "Unnamed"')
        wait_for_tree(lacks("button", 'Dismiss "Unnamed"'), "dismissed")

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
            machine.succeed("ydotool mousemove --wheel -x -1 -y 0")
            time.sleep(0.05)
        wait_for_tree(banner_gone("Scrolled"), "swiped by a scroll")

    with subtest("a swipe short of the distance goes back, unless flung"):
        y = HEIGHT + 8 + 40
        card = 1024 - 8 - 360

        def at_rest():
            """The banner where it shows, and opaque."""
            shot = screen()
            left = next((x for x in range(400, 1024) if is_popover(pixel(shot, x, y))), None)
            return left is not None and abs(left - card) <= 2 and is_popover(pixel(shot, 1024 - 16, y))

        def back_at_rest(what):
            for _ in range(30):
                if at_rest():
                    return
                time.sleep(0.1)
            machine.send_monitor_command(f"screendump {machine.out_dir}/stuck.ppm gpu 0")
            raise Exception(f"{what}: the banner not back")

        def close(nid):
            as_user(CALL + f"CloseNotification u {nid}")

        # Two notches of a sideways wheel (no scroll end), which slide the banner right from
        # under the pointer left still: it goes back, and a scroll on it swipes it again.
        nid = as_user(NOTIFY + "-p -t 20000 Short body").strip()
        wait_for_tree(banner_shown("Short"), "the banner")
        back_at_rest("shown")
        move(card + 20, y)
        for _ in range(2):
            machine.succeed("ydotool mousemove --wheel -x -1 -y 0")
            time.sleep(0.05)
        back_at_rest("a short scroll")
        move(card + 200, y)
        for _ in range(5):
            machine.succeed("ydotool mousemove --wheel -x -1 -y 0")
            time.sleep(0.05)
        wait_for_tree(banner_gone("Short"), "swiped by a scroll after")
        # Inwards, away from its edge, it does not go, however far or fast: a scroll past
        # the distance, and a flick.
        nid = as_user(NOTIFY + "-p -t 20000 Inward body").strip()
        wait_for_tree(banner_shown("Inward"), "the banner")
        move(card + 200, y)
        for _ in range(5):
            machine.succeed("ydotool mousemove --wheel -x 1 -y 0")
            time.sleep(0.05)
        back_at_rest("an inward scroll")
        x = card + 200
        move(x, y)
        input_events({"type": "btn", "data": {"down": True, "button": "left"}})
        for i in range(1, 4):
            time.sleep(0.02)
            move(x - 20 * i, y)
        time.sleep(0.02)
        input_events({"type": "btn", "data": {"down": False, "button": "left"}})
        back_at_rest("an inward flick")
        assert banner_shown("Inward")(tree()), "dismissed inwards"
        close(nid)
        wait_for_tree(banner_gone("Inward"), "closed")
        # A quick flick short of the distance, towards its edge, goes.
        nid = as_user(NOTIFY + "-p -t 20000 Flung body").strip()
        wait_for_tree(banner_shown("Flung"), "the banner")
        x = card + 60
        move(x, y)
        input_events({"type": "btn", "data": {"down": True, "button": "left"}})
        for i in range(1, 4):
            time.sleep(0.02)
            move(x + 20 * i, y)
        time.sleep(0.02)
        input_events({"type": "btn", "data": {"down": False, "button": "left"}})
        wait_for_tree(banner_gone("Flung"), "flung")
        assert re.search(rf"NotificationClosed\s+uint32 {nid}\s+uint32 2", signals()), signals()
        # A slow drag as far, let go after a stop, goes back.
        nid = as_user(NOTIFY + "-p -t 20000 Slow body").strip()
        wait_for_tree(banner_shown("Slow"), "the banner")
        move(x, y)
        input_events({"type": "btn", "data": {"down": True, "button": "left"}})
        for i in range(1, 7):
            time.sleep(0.1)
            move(x + 10 * i, y)
        time.sleep(0.3)
        input_events({"type": "btn", "data": {"down": False, "button": "left"}})
        back_at_rest("a slow drag")
        assert banner_shown("Slow")(tree()), "dismissed"
        close(nid)
        wait_for_tree(banner_gone("Slow"), "closed")

    with subtest("the Notification Centre: down the right, closed by a click outside"):
        nid = as_user(NOTIFY + "-p -t 1 Centre body").strip()
        # The clock: the widest of the bar's right side.
        first, last = max(right_items(), key=lambda run: run[1] - run[0])
        def open_centre():
            click((first + last) // 2, HEIGHT // 2)
            wait_for_tree(has("switch", "Do Not Disturb"), "the Notification Centre")
        def closed(what):
            wait_for_tree(lacks("switch", "Do Not Disturb"), what)
            typing_reaches_the_window("x" + str(time.time_ns()))
        open_centre()
        assert has("label", "Centre")(tree()), "not in the Centre"
        # A notification arriving meanwhile shows in it, which stays open.
        later = as_user(NOTIFY + "-p -t 1 Arrived body").strip()
        wait_for_tree(has("label", "Arrived"), "the new one in the Centre")
        assert has("switch", "Do Not Disturb")(tree()), "closed by an update"
        move(100, 700)
        time.sleep(1)
        shot = screen()
        machine.send_monitor_command(f"screendump {machine.out_dir}/centre.ppm gpu 0")
        # Solid, 380 px wide (the default) and 8 px (the gap) from the right edge, from
        # below the bar to 8 px above the bottom.
        row = [x for x in range(512, 1024) if is_popover(pixel(shot, x, 400))]
        assert abs(min(row) - (1024 - 8 - 380)) <= 2 and abs(max(row) - (1024 - 9)) <= 2, row
        # In its padding, left: below the bar, and down to the bottom.
        x = 1024 - 8 - 380 + 8
        assert is_popover(pixel(shot, x, HEIGHT + 8 + 20)), "not below the bar"
        assert is_popover(pixel(shot, x, 768 - 8 - 20)), "not the full height"
        # A click on the desktop (right of foot, left of the panel) closes it.
        click(570, 600)
        closed("closed by a click on the desktop")
        open_centre()
        click(200, 400)
        closed("closed by a click on a window")
        open_centre()
        machine.send_key("esc")
        wait_for_tree(lacks("switch", "Do Not Disturb"), "closed by Escape")
        # Open again at once (no stale guard from the Escape's close).
        open_centre()
        machine.send_key("esc")
        closed("closed by Escape again")
        # The clock again closes it, and does not open it anew.
        open_centre()
        click((first + last) // 2, HEIGHT // 2)
        time.sleep(1.5)
        closed("closed by the clock")
        as_user(CALL + f"CloseNotification u {nid}")
        as_user(CALL + f"CloseNotification u {later}")

    with subtest("Do Not Disturb, kept with the history over a restart"):
        history()
        assert lacks("image", "Do Not Disturb")(tree()), "the indicator with it off"
        act("Do Not Disturb", "switch")
        machine.wait_until_succeeds("grep -q '\"dnd\":true' /home/${session.user}/.local/state/rust-wl-shell/notifications.json")
        close_history()
        # Beside the clock while it is on, as macOS's Focus moon.
        wait_for_tree(has("image", "Do Not Disturb"), "the Do Not Disturb indicator")
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
        assert has("image", "Do Not Disturb")(tree()), "no indicator after the restart"
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
