# NixOS VM test: the power menu, in the session of session.nix, against a fake logind on a
# private system bus (the shell's system bus here; the VM's own logind is never asked to
# power off), with niri's own IPC for logging out.
#
# Behaviours asserted:
#   1. the entries follow logind's answers: "no" leaves one out, "challenge" shows it
#   2. Lock Screen locks alice's graphical session, Sleep suspends, each interactive
#   3. Restart… asks, lists the block inhibitors (not the delay ones), counts down, and
#      restarts at 0
#   4. Shut Down…: Cancel and Escape do nothing; a refusal shows logind's error in the
#      dialog; its button acts at once
#   5. the keyboard is back with the window after the popover and the dialog
#   6. a countdown is called off when its output goes, a sleep starts, or the session
#      locks: nothing reaches logind
#   7. Log Out… ends niri, and the session with it

{
  pkgs,
  homeManager,
  module,
}:

let
  session = import ./session.nix { inherit pkgs homeManager module; };
  inherit (session) user;
  fakes = import ./fake-services.nix { inherit pkgs; };
  socket = "/run/fake-bus/socket";
  busConfig = pkgs.writeText "fake-bus.conf" ''
    <!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
      "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
    <busconfig>
      <type>system</type>
      <listen>unix:path=${socket}</listen>
      <auth>EXTERNAL</auth>
      <policy context="default">
        <allow user="*"/>
        <allow own="*"/>
        <allow send_destination="*"/>
        <allow receive_sender="*"/>
      </policy>
    </busconfig>
  '';
in
pkgs.testers.runNixOSTest {
  name = "power";

  nodes.machine = {
    imports = [
      (session.machine {
        height = 28;
        spacing = 8;
        clock-format = "%H:%M %d/%m";
        power-countdown = 3;
        # Nothing else on the system bus, which is the fake one here.
        network = false;
        vpn = false;
        bluetooth = false;
        keep-awake = false;
        battery = false;
      })
    ];
    systemd.services.fake-bus = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        RuntimeDirectory = "fake-bus";
        RuntimeDirectoryMode = "0755";
        ExecStart = "${pkgs.dbus}/bin/dbus-daemon --nofork --nopidfile --config-file=${busConfig}";
      };
    };
    systemd.services.fake-logind = {
      wantedBy = [ "multi-user.target" ];
      requires = [ "fake-bus.service" ];
      after = [ "fake-bus.service" ];
      environment.FAKE_BUS = "unix:path=${socket}";
      serviceConfig = {
        RuntimeDirectory = "fake-logind";
        RuntimeDirectoryPreserve = true;
        ExecStartPre = "${pkgs.coreutils}/bin/sleep 1";
        ExecStart = fakes.logind;
      };
    };
    home-manager.users.${user}.systemd.user.services.rust-wl-shell.Service.Environment = [
      "DBUS_SYSTEM_BUS_ADDRESS=unix:path=${socket}"
    ];
  };

  testScript = ''
    ${session.helpers}

    def calls():
        return machine.succeed("cat /run/fake-logind/calls 2>/dev/null || true")

    def menu():
        """The power menu open: its rows."""
        act("Power", "toggle button")
        wait_for_tree(has("radio button", "Log Out…"), "the power menu")
        return [n for r, n in tree() if r == "radio button"]

    def dialog_open(t):
        return any(n.endswith("?") for r, n in t if r == "label")

    def frames():
        return int(machine.succeed(f"{LOG} | grep -c 'first frame' || true").strip())

    def restart_shell():
        before = frames()
        as_user("systemctl --user restart rust-wl-shell")
        retry(lambda _: frames() > before, 60)
        wait_for_tree(has("button", "Power"), "the power item again")

    machine.wait_for_unit("fake-logind.service")
    machine.wait_for_unit("multi-user.target")

    with subtest("the entries follow logind's answers"):
        machine.succeed(
            "echo '{\"CanSuspend\": \"yes\", \"CanReboot\": \"challenge\", \"CanPowerOff\": \"no\"}'"
            + " > /run/fake-logind/can.json"
        )
        machine.wait_until_succeeds(f"{LOG} | grep -q 'first frame'", timeout=120)
        spawn("foot")
        wait_for_tree(has("label", "Foot"), "a foot window")
        restart_shell()
        rows = menu()
        assert rows == ["Lock Screen", "Sleep", "Restart…", "Log Out…"], rows
        machine.send_monitor_command(f"screendump {machine.out_dir}/menu.ppm gpu 0")
        act("Power", "toggle button")
        typing_reaches_the_window("after-menu")
        machine.succeed("echo '{}' > /run/fake-logind/can.json")
        restart_shell()
        rows = menu()
        assert "Shut Down…" in rows, rows
        act("Power", "toggle button")

    with subtest("Lock Screen and Sleep act at once"):
        menu()
        act("Lock Screen")
        machine.wait_until_succeeds("grep -q 'LockSession c1' /run/fake-logind/calls", timeout=10)
        menu()
        act("Sleep")
        machine.wait_until_succeeds("grep -q 'Suspend True' /run/fake-logind/calls", timeout=10)
        typing_reaches_the_window("after-sleep")

    with subtest("Restart asks, lists what blocks it, and restarts at 0"):
        inhibitors = (
            '[["shutdown:sleep", "Firefox", "Downloading", "block", 1000, 1],'
            + ' ["shutdown", "session-manager", "Closing", "delay", 1000, 2]]'
        )
        machine.succeed(f"echo '{inhibitors}' > /run/fake-logind/inhibitors.json")
        menu()
        act("Restart…")
        wait_for_tree(has("label", "Restart?"), "the dialog")
        wait_for_tree(lambda t: any("Firefox: Downloading" in n for _, n in t), "the blocker")
        t = tree()
        assert not any("session-manager" in n for _, n in t), t
        machine.send_monitor_command(f"screendump {machine.out_dir}/dialog.ppm gpu 0")
        assert "Reboot" not in calls(), calls()
        machine.wait_until_succeeds("grep -q 'Reboot True' /run/fake-logind/calls", timeout=10)
        wait_for_tree(lambda t: not dialog_open(t), "the dialog closed")
        typing_reaches_the_window("after-restart")

    with subtest("Shut Down: Cancel and Escape do nothing, a refusal shows, the button acts"):
        menu()
        act("Shut Down…")
        wait_for_tree(has("label", "Shut Down?"), "the dialog")
        act("Cancel")
        wait_for_tree(lambda t: not dialog_open(t), "cancelled")
        menu()
        act("Shut Down…")
        wait_for_tree(has("label", "Shut Down?"), "the dialog")
        machine.send_key("esc")
        wait_for_tree(lambda t: not dialog_open(t), "escaped")
        time.sleep(4)
        assert "PowerOff" not in calls(), calls()
        typing_reaches_the_window("after-cancel")
        machine.succeed("touch /run/fake-logind/refuse")
        menu()
        act("Shut Down…")
        wait_for_tree(has("label", "Shut Down?"), "the dialog")
        act("Shut Down", "button")
        wait_for_tree(lambda t: any("Operation inhibited" in n for _, n in t), "the refusal")
        assert dialog_open(tree()), "closed though refused"
        act("Cancel")
        machine.succeed("rm /run/fake-logind/refuse")
        menu()
        act("Shut Down…")
        wait_for_tree(has("label", "Shut Down?"), "the dialog")
        act("Shut Down", "button")
        machine.wait_until_succeeds("grep -q 'PowerOff True' /run/fake-logind/calls", timeout=5)
        typing_reaches_the_window("after-shut-down")

    with subtest("a countdown is called off: its output gone, a sleep, the lock"):
        def counting(what):
            menu()
            act("Shut Down\u2026")
            wait_for_tree(has("label", "Shut Down?"), f"the dialog, before {what}")
        def nothing_after(what):
            wait_for_tree(lambda t: not dialog_open(t), f"called off by {what}")
            time.sleep(4)
            assert calls().count("PowerOff") == 1, calls()
        # The dialog on the focused output, switched off under it.
        niri("action focus-monitor-left")
        counting("the output going")
        niri("output Virtual-1 off")
        nothing_after("its output going")
        # Called off, not gone with a crash (the shell restarted by its unit).
        restarts = as_user("systemctl --user show -p NRestarts --value rust-wl-shell").strip()
        assert restarts == "0", f"the shell restarted {restarts} times"
        niri("output Virtual-1 on")
        wait_for_tree(lambda t: t.count(["button", "Power"]) == 2, "both bars again")
        counting("a sleep")
        machine.succeed("systemctl kill -s USR1 fake-logind")
        nothing_after("a sleep")
        counting("the lock")
        machine.succeed("systemctl kill -s USR2 fake-logind")
        nothing_after("the lock")
        # The output gone and back, foot's window may be elsewhere: the bars, at least,
        # ask for no keyboard.
        retry(lambda _: keyboard_free(), 10)

    with subtest("Log Out ends niri, and the session"):
        menu()
        act("Log Out…")
        wait_for_tree(has("label", "Log Out?"), "the dialog")
        # This niri; getty logs alice in again, with a niri of its own.
        niri_pid = machine.succeed("pgrep -u ${user} -x niri").split()[0]
        act("Log Out", "button")
        machine.wait_until_fails(f"kill -0 {niri_pid}", timeout=20)
  '';
}
