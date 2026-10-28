# NixOS VM test: the network, VPN and Bluetooth items, in the session of session.nix, with
# real Wi-Fi: mac80211_hwsim radios, NetworkManager on its iwd backend for the first, and
# hostapd on the others (in a network namespace of their own, with dnsmasq's DHCP, so that
# their traffic goes over the air): "Home" (WPA2-PSK), "Cafe" (open) and "Campus"
# (WPA2-Enterprise, hostapd's own EAP server, PEAP/MSCHAPv2, its certificate signed by a
# CA added to the system's). A WireGuard connection in NM, and a fake BlueZ.
#
# Behaviours asserted:
#   1. the network item follows NM: offline, then each network joined
#   2. the Wi-Fi popover lists the networks in range under "Other Networks", then the
#      known ones above, the current one checked; with many networks the list scrolls,
#      and the popover stays inside the output
#   3. joining the open network takes a click
#   4. joining the WPA2 one asks for its password in the dialog (on the focused output,
#      with the keyboard): a wrong one shows the error in it, the right one joins and closes
#      it
#   5. the enterprise network asks for the username too, and its method and domain (in
#      "Options"), and joins
#   6. a saved network whose password changed: NM asks the shell's secret agent, whose
#      dialog answers with the new one; then, asked again, Cancel answers NM with a
#      cancel; asked again, a join from the popover cancels the prompt for NM at once;
#      then NM cancelling its request (the device disconnected) closes the dialog
#   7. each way the dialog closes (Join, Cancel, Escape, an error then Cancel, NM's
#      cancel) gives the keyboard back: typing reaches the window again
#   8. the Wi-Fi switch turns the radio off and on
#   9. the VPN item lists the WireGuard connection, and switches it on and off
#  10. Bluetooth: the paired devices (the battery's level after one), connect and
#      disconnect, and the adapter's switch
#  11. none of the passwords typed is in the system's or alice's journal

{
  pkgs,
  homeManager,
  module,
}:

let
  inherit (pkgs) lib;
  session = import ./session.nix { inherit pkgs homeManager module; };
  inherit (session) user;
  fakes = import ./fake-services.nix { inherit pkgs; };

  certs = pkgs.runCommand "test-certs" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out && cd $out
    openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.pem -days 3650 \
      -subj "/CN=Test CA"
    openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr \
      -subj "/CN=server.radius.test"
    printf "subjectAltName=DNS:server.radius.test\nextendedKeyUsage=serverAuth\n" > ext
    openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial \
      -out server.pem -days 3650 -extfile ext
  '';
  eapUsers = pkgs.writeText "eap-users" ''
    "alice" PEAP,TTLS
    "alice" MSCHAPV2,TTLS-MSCHAPV2,TTLS-PAP "campus-pass" [2]
  '';
  # Each radio also has seven open networks, "Extra N-M", so that the list overflows.
  ap = radio: text: pkgs.writeText "wlan${radio}.conf" (
    ''
      driver=nl80211
      hw_mode=g
      channel=1
      interface=wlan${radio}
      ${text}
    ''
    + lib.concatMapStrings (m: ''
      bss=wlan${radio}_${toString m}
      ssid=Extra ${radio}-${toString m}
      bssid=02:00:00:00:0${radio}:0${toString m}
    '') (lib.range 1 7)
  );
  aps = pkgs.linkFarm "access-points" [
    {
      name = "home.conf";
      path = ap "1" ''
        ssid=Home
        wpa=2
        wpa_key_mgmt=WPA-PSK
        rsn_pairwise=CCMP
        wpa_passphrase=correct-horse
      '';
    }
    {
      name = "cafe.conf";
      path = ap "2" ''
        ssid=Cafe
      '';
    }
    {
      name = "campus.conf";
      path = ap "3" ''
        ssid=Campus
        wpa=2
        wpa_key_mgmt=WPA-EAP
        rsn_pairwise=CCMP
        ieee8021x=1
        eap_server=1
        eap_user_file=${eapUsers}
        ca_cert=${certs}/ca.pem
        server_cert=${certs}/server.pem
        private_key=${certs}/server.key
      '';
    }
  ];
in
pkgs.testers.runNixOSTest {
  name = "network";

  nodes.machine = {
    imports = [
      (session.machine {
        height = 28;
        spacing = 8;
        dot-size = 6;
        clock-format = "%H:%M %d/%m";
      })
    ];

    boot.kernelModules = [ "mac80211_hwsim" ];
    boot.extraModprobeConfig = "options mac80211_hwsim radios=4";
    networking.networkmanager = {
      enable = true;
      wifi.backend = "iwd";
      # Only the first radio, and no wired connection: the network is Wi-Fi's.
      unmanaged = [
        "wlan1"
        "wlan2"
        "wlan3"
        "eth0"
        "eth1"
      ];
    };
    systemd.services.iwd.serviceConfig.ExecStart = [
      ""
      "${pkgs.iwd}/libexec/iwd --phys phy0"
    ];
    # iwd reads the system's CA bundle only if it holds plain certificates.
    security.pki.useCompatibleBundle = true;
    security.pki.certificateFiles = [ "${certs}/ca.pem" ];
    systemd.services.access-points = {
      wantedBy = [ "multi-user.target" ];
      wants = [ "sys-subsystem-net-devices-wlan3.device" ];
      after = [ "sys-subsystem-net-devices-wlan3.device" ];
      path = [
        pkgs.iproute2
        pkgs.iw
        pkgs.hostapd
        pkgs.dnsmasq
      ];
      # Their configs in /run/ap, where the test changes Home's password.
      script = ''
        ip netns add ap 2>/dev/null || true
        for phy in phy1 phy2 phy3; do iw phy $phy set netns name ap 2>/dev/null || true; done
        mkdir -p /run/ap
        for ap in home cafe campus; do
          [ -e /run/ap/$ap.conf ] || cp ${aps}/$ap.conf /run/ap/
        done
        n="ip netns exec ap"
        $n ip link set lo up
        for i in 1 2 3; do $n ip addr replace 10.$i.0.1/24 dev wlan$i; done
        $n dnsmasq -k --port=0 --bind-interfaces --dhcp-leasefile=/run/ap/leases \
          --interface=wlan1 --interface=wlan2 --interface=wlan3 \
          --dhcp-range=10.1.0.10,10.1.0.50 --dhcp-range=10.2.0.10,10.2.0.50 \
          --dhcp-range=10.3.0.10,10.3.0.50 &
        exec $n hostapd /run/ap/home.conf /run/ap/cafe.conf /run/ap/campus.conf
      '';
    };

    services.dbus.packages = [ fakes.policy ];
    systemd.services.fake-bluez = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        Type = "dbus";
        BusName = "org.bluez";
        ExecStart = fakes.bluez;
      };
    };
  };

  testScript = ''
    ${session.helpers}

    def act(name, role=""):
        """The first action (a click) of the bar's node named `name` (and of that role),
        through AT-SPI."""
        as_user(f"atspi-tree --do '{name}' '{role}'" if role else f"atspi-tree --do '{name}'")

    def dialog_open(t):
        return any(name.endswith("Required") for role, name in t if role == "label")

    def typing_reaches_the_window(word):
        """The keyboard is back with the focused window, a foot running alice's shell."""
        retry(lambda _: keyboard_free(), 10)
        machine.send_chars(f"touch {word}\n")
        machine.wait_for_file(f"/home/${user}/{word}", timeout=10)

    def item(prefix):
        """The item whose name starts so: its toggle (by AT-SPI, as the icons' pixels do
        not tell the items apart) opens or closes its popover."""
        names = [n for r, n in tree() if r == "toggle button" and n.startswith(prefix)]
        act(names[0])

    def wifi_popover():
        item(("Wi-Fi", "Network"))
        wait_for_tree(has("switch", "Wi-Fi"), "the Wi-Fi popover")

    def other_networks():
        """The network's popover, open, with "Other Networks" open too."""
        wifi_popover()
        act("Other Networks")

    def tab_to(name):
        """Tab until the focus is on the node named so (or with a name starting so)."""
        for _ in range(10):
            focused = json.loads(as_user("atspi-tree --focused"))
            print(f"focused: {focused}")
            if any(n.startswith(name) for n in focused):
                return
            machine.send_key("tab")
        raise AssertionError(f"no focus on {name}")

    def type_into_dialog(text):
        machine.send_key("ctrl-a")
        machine.send_chars(text)

    def joined(ssid):
        wait_for_tree(has("button", f"Wi-Fi: {ssid}"), f"{ssid} joined")

    def agent_asks():
        """Home's saved password refused: a click on Home, and NM asks the agent, whose
        dialog opens."""
        wait_for_tree(lambda t: ["button", "Wi-Fi: Home"] not in t, "Home gone")
        wifi_popover()
        act("Home")
        asked = 'The Wi-Fi network \u201cHome\u201d requires a password.'
        wait_for_tree(has("label", asked), "the agent's dialog")

    def home_password(password):
        machine.succeed(
            f"sed -i 's/^wpa_passphrase=.*/wpa_passphrase={password}/' /run/ap/home.conf"
            + " && pkill dnsmasq; systemctl restart access-points"
        )

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("access-points.service")

    with subtest("the network item: offline, and the networks in range"):
        machine.wait_until_succeeds(f"{LOG} | grep -q 'first frame'", timeout=120)
        spawn("foot")
        wait_for_tree(has("label", "Foot"), "a foot window")
        wait_for_tree(has("button", "Network: offline"), "the network item")
        print(machine.succeed(LOG))
        retry(lambda _: "Campus" in machine.succeed("nmcli -t -f SSID device wifi list"), 30)
        other_networks()
        wait_for_tree(has("radio button", "Campus"), "the networks in range")
        rows = [n for r, n in tree() if r == "radio button"]
        assert {"Home", "Cafe", "Campus"} <= set(rows), rows
        machine.send_monitor_command(f"screendump {machine.out_dir}/wifi-others.ppm gpu 0")

    with subtest("many networks: the list scrolls, and the popover fits the output"):
        # The popover still open, with "Other Networks".
        wait_for_tree(has("radio button", "Extra 3-7"), "the extra networks")
        rows = [n for r, n in tree() if r == "radio button"]
        assert len(rows) == 24, rows
        move(100, 700)
        time.sleep(0.5)
        shot = screen()
        machine.send_monitor_command(f"screendump {machine.out_dir}/wifi-many.ppm gpu 0")
        # The output's bottom rows are niri's grey: the popover ends above them.
        assert all(is_grey(pixel(shot, x, 760)) for x in range(500, 1024)), "off the output"
        # A wheel over the list scrolls it: the popover's pixels change.
        move(800, 300)
        time.sleep(0.5)
        shot = screen()
        for _ in range(10):
            input_events({"type": "btn", "data": {"down": True, "button": "wheel-down"}},
                         {"type": "btn", "data": {"down": False, "button": "wheel-down"}})
        time.sleep(0.5)
        scrolled = screen()
        assert scrolled[1] != shot[1], "the list did not scroll"
        item(("Wi-Fi", "Network"))
        typing_reaches_the_window("after-many")

    with subtest("an open network joins with a click"):
        other_networks()
        act("Cafe")
        joined("Cafe")
        typing_reaches_the_window("after-cafe")

    with subtest("a WPA2 network asks for its password; a wrong one shows the error"):
        other_networks()
        act("Home")
        wait_for_tree(has("label", "Wi-Fi Password Required"), "the dialog")
        t = tree()
        assert has("label", 'The Wi-Fi network “Home” requires a WPA2 password.')(t), t
        machine.send_monitor_command(f"screendump {machine.out_dir}/dialog.ppm gpu 0")
        # Typed straight into it: the dialog has the keyboard.
        machine.send_chars("wrong-password\n")
        wait_for_tree(has("label", "Wrong password or credentials."), "the error")
        assert dialog_open(tree()), "the dialog closed on an error"
        type_into_dialog("correct-horse\n")
        joined("Home")
        wait_for_tree(lambda t: not dialog_open(t), "the dialog closed")
        typing_reaches_the_window("after-join")
        # Known now: above "Other Networks", checked.
        wifi_popover()
        checked = json.loads(as_user("atspi-tree --checked"))
        assert "Home" in checked and "Cafe" not in checked, checked
        machine.send_monitor_command(f"screendump {machine.out_dir}/wifi.ppm gpu 0")
        item("Wi-Fi")
        typing_reaches_the_window("after-popover")

    with subtest("the dialog's Cancel and Escape, and an error then Cancel"):
        other_networks()
        act("Campus")
        wait_for_tree(has("label", "Wi-Fi Password Required"), "the dialog")
        act("Cancel")
        wait_for_tree(lambda t: not dialog_open(t), "the dialog cancelled")
        typing_reaches_the_window("after-cancel")
        other_networks()
        act("Campus")
        wait_for_tree(has("label", "Wi-Fi Password Required"), "the dialog")
        machine.send_key("esc")
        wait_for_tree(lambda t: not dialog_open(t), "the dialog escaped")
        typing_reaches_the_window("after-escape")
        other_networks()
        act("Campus")
        wait_for_tree(has("label", "Wi-Fi Password Required"), "the dialog")
        machine.send_chars("alice\tnot-the-password\n")
        wait_for_tree(has("label", "Wrong password or credentials."), "the error")
        act("Cancel")
        wait_for_tree(lambda t: not dialog_open(t), "the dialog cancelled after an error")
        typing_reaches_the_window("after-error")

    with subtest("an enterprise network takes a username, and its domain"):
        other_networks()
        act("Campus")
        wait_for_tree(has("label", "Wi-Fi Password Required"), "the dialog")
        t = tree()
        assert has("label", "The Wi-Fi network “Campus” requires WPA2 enterprise credentials.")(t), t
        # Username, password, the disclosure opened, then the domain: all by the
        # keyboard, the dialog's.
        tab_to("Username")
        machine.send_chars("alice")
        tab_to("Password")
        machine.send_chars("campus-pass")
        tab_to("Options")
        machine.send_key("spc")
        tab_to("Domain")
        machine.send_monitor_command(f"screendump {machine.out_dir}/dialog-eap.ppm gpu 0")
        machine.send_chars("radius.test\n")
        joined("Campus")
        settings = machine.succeed("nmcli -s -g 802-1x connection show Campus")
        print(settings)
        assert "radius.test" in settings and "ca-certificates.crt" in settings, settings
        typing_reaches_the_window("after-campus")

    with subtest("a saved network's changed password: the agent asks"):
        machine.succeed("nmcli connection up Home")
        joined("Home")
        # Not joined again on its own: only on a click.
        machine.succeed("nmcli connection modify Home connection.autoconnect no")
        home_password("new-password-1")
        agent_asks()
        machine.send_chars("new-password-1\n")
        joined("Home")
        typing_reaches_the_window("after-agent")
        # Asked again, and cancelled: NM has its answer at once (else it would wait for
        # the agent, up to 2 min, with the device needing its secret).
        home_password("new-password-2")
        agent_asks()
        act("Cancel")
        wait_for_tree(lambda t: not dialog_open(t), "the dialog cancelled")
        machine.wait_until_fails("nmcli -t -f STATE device | grep -q 'need auth'", timeout=10)
        typing_reaches_the_window("after-agent-cancel")
        # Asked again, then a join from the popover: the prompt is cancelled for NM at once
        # (not left to NM's 2 min).
        agent_asks()
        machine.wait_until_succeeds("nmcli -t -f STATE device | grep -q 'need auth'", timeout=10)
        machine.succeed("nmcli connection delete Campus")
        other_networks()
        wait_for_tree(has("radio button", "Campus"), "Campus unknown again")
        act("Campus")
        asked = "The Wi-Fi network \u201cCampus\u201d requires WPA2 enterprise credentials."
        wait_for_tree(has("label", asked), "the join's dialog")
        machine.wait_until_fails("nmcli -t -f STATE device | grep -q 'need auth'", timeout=10)
        act("Cancel")
        wait_for_tree(lambda t: not dialog_open(t), "the join's dialog cancelled")
        typing_reaches_the_window("after-join-over-prompt")
        # Asked again, then NM cancels (the device is disconnected): the dialog closes.
        agent_asks()
        machine.succeed("nmcli device disconnect wlan0")
        wait_for_tree(lambda t: not dialog_open(t), "the dialog closed by NM")
        typing_reaches_the_window("after-nm-cancel")

    with subtest("the Wi-Fi switch"):
        wifi_popover()
        act("Wi-Fi", "switch")
        machine.wait_until_succeeds("nmcli radio wifi | grep -q disabled", timeout=10)
        wait_for_tree(has("button", "Network: offline"), "offline")
        act("Wi-Fi", "switch")
        machine.wait_until_succeeds("nmcli radio wifi | grep -q enabled", timeout=10)
        item(("Wi-Fi", "Network"))
        typing_reaches_the_window("after-switch")

    with subtest("the VPN item switches WireGuard on and off"):
        machine.succeed(
            "nmcli connection add type wireguard con-name 'Office VPN' ifname wg0"
            + " autoconnect no wireguard.private-key yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk="
            + " ipv4.method manual ipv4.addresses 10.9.0.2/24 ipv6.method disabled"
        )
        wait_for_tree(has("button", "VPN: off"), "the VPN item")
        item("VPN")
        wait_for_tree(has("check box", "Office VPN"), "the VPN popover")
        machine.send_monitor_command(f"screendump {machine.out_dir}/vpn.ppm gpu 0")
        act("Office VPN")
        machine.wait_until_succeeds("nmcli -t -f NAME connection show --active | grep -q 'Office VPN'", timeout=20)
        wait_for_tree(has("button", "VPN: on"), "the VPN on")
        item("VPN")
        wait_for_tree(has("check box", "Office VPN"), "the VPN popover")
        act("Office VPN")
        machine.wait_until_fails("nmcli -t -f NAME connection show --active | grep -q 'Office VPN'", timeout=20)
        wait_for_tree(has("button", "VPN: off"), "the VPN off")
        typing_reaches_the_window("after-vpn")

    with subtest("Bluetooth: the paired devices, connected and disconnected, and the switch"):
        wait_for_tree(has("button", "Bluetooth: on"), "the Bluetooth item")
        item("Bluetooth")
        wait_for_tree(has("check box", "WH-1000XM4"), "the devices")
        t = tree()
        devices = [n for r, n in t if r == "check box"]
        assert devices == ["Mouse", "WH-1000XM4"], devices
        assert has("label", "80%")(t), t
        machine.send_monitor_command(f"screendump {machine.out_dir}/bluetooth.ppm gpu 0")
        act("WH-1000XM4")
        wait_for_tree(has("button", "Bluetooth: connected"), "connected")
        item("Bluetooth")
        wait_for_tree(has("check box", "WH-1000XM4"), "the devices")
        checked = json.loads(as_user("atspi-tree --checked"))
        assert "WH-1000XM4" in checked and "Mouse" not in checked, checked
        act("WH-1000XM4")
        wait_for_tree(has("button", "Bluetooth: on"), "disconnected")
        item("Bluetooth")
        wait_for_tree(has("switch", "Bluetooth"), "the popover")
        act("Bluetooth", "switch")
        wait_for_tree(has("button", "Bluetooth: off"), "the adapter off")
        item("Bluetooth")
        typing_reaches_the_window("after-bluetooth")

    with subtest("no secret in the journals"):
        secrets = ["correct-horse", "wrong-password", "campus-pass", "not-the-password",
                   "new-password-1", "new-password-2"]
        journals = machine.succeed("journalctl --no-pager -o cat") + as_user(
            "journalctl --user --no-pager -o cat"
        )
        found = [s for s in secrets if s in journals]
        assert not found, found
  '';
}
