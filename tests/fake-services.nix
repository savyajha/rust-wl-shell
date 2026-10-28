# Fake system services for the VM tests, in dbus-python (as the greeter's tests have them):
# UPower's DisplayDevice, power-profiles-daemon and BlueZ, with as much of each as the shell
# reads.
{ pkgs }:

let
  python =
    name: text:
    pkgs.writers.writePython3 name {
      libraries = [
        pkgs.python3Packages.dbus-python
        pkgs.python3Packages.pygobject3
      ];
    } text;

  # The properties object both fakes share: Get, GetAll, PropertiesChanged.
  common = ''
    import dbus
    import dbus.mainloop.glib
    import dbus.service
    from gi.repository import GLib

    PROPERTIES = "org.freedesktop.DBus.Properties"


    class Object(dbus.service.Object):
        def __init__(self, bus, path, interface, properties):
            super().__init__(bus, path)
            self.interface = interface
            self.properties = properties

        @dbus.service.method(PROPERTIES, in_signature="ss", out_signature="v")
        def Get(self, interface, name):
            return self.properties[name]

        @dbus.service.method(PROPERTIES, in_signature="s", out_signature="a{sv}")
        def GetAll(self, interface):
            return self.properties

        @dbus.service.signal(PROPERTIES, signature="sa{sv}as")
        def PropertiesChanged(self, interface, changed, invalidated):
            pass

        def change(self, changed):
            self.properties.update(changed)
            self.PropertiesChanged(self.interface, changed, [])
            return True


    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SystemBus()
  '';
in
{
  # The DisplayDevice: 50 % and discharging, 1:30 left; on SIGUSR1 90 % and charging; on
  # SIGUSR2 not present (UPower on a machine with no battery).
  upower = python "fake-upower" ''
    import signal
    ${common}
    name = dbus.service.BusName("org.freedesktop.UPower", bus)
    device = Object(
        bus,
        "/org/freedesktop/UPower/devices/DisplayDevice",
        "org.freedesktop.UPower.Device",
        {
            "IsPresent": dbus.Boolean(True),
            "Percentage": dbus.Double(50.0),
            "State": dbus.UInt32(2),
            "TimeToEmpty": dbus.Int64(5400),
            "TimeToFull": dbus.Int64(0),
        },
    )
    charging = {
        "Percentage": dbus.Double(90.0),
        "State": dbus.UInt32(1),
        "TimeToFull": dbus.Int64(1800),
    }
    absent = {"IsPresent": dbus.Boolean(False)}
    for number, changed in [(signal.SIGUSR1, charging), (signal.SIGUSR2, absent)]:
        GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, number, device.change, changed)
    GLib.MainLoop().run()
  '';

  # power-profiles-daemon: three profiles, performance active; Set changes it, as the
  # daemon does, unless /run/fake-power-profiles-refuse exists (as polkit may refuse).
  profiles = python "fake-power-profiles" ''
    import os
    ${common}

    class Daemon(Object):
        @dbus.service.method(PROPERTIES, in_signature="ssv")
        def Set(self, interface, name, value):
            if os.path.exists("/run/fake-power-profiles-refuse"):
                raise dbus.exceptions.DBusException(
                    "refused", name="org.freedesktop.DBus.Error.AccessDenied"
                )
            self.change({name: value})


    def profile(name):
        return dbus.Dictionary(
            {"Profile": name, "Driver": "fake"}, signature="sv"
        )


    name = dbus.service.BusName("org.freedesktop.UPower.PowerProfiles", bus)
    daemon = Daemon(
        bus,
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
        {
            "ActiveProfile": dbus.String("performance"),
            "Profiles": dbus.Array(
                [profile(p) for p in ["power-saver", "balanced", "performance"]],
                signature="a{sv}",
            ),
            "PerformanceDegraded": dbus.String(""),
        },
    )
    GLib.MainLoop().run()
  '';

  # BlueZ: an adapter, powered, with two paired devices (headphones with a battery at 80 %,
  # and a mouse), neither connected, and one not paired; Set (Powered), Connect and
  # Disconnect change them, as BlueZ does.
  bluez = python "fake-bluez" ''
    ${common}
    MANAGER = "org.freedesktop.DBus.ObjectManager"
    DEVICE = "org.bluez.Device1"


    class Root(dbus.service.Object):
        @dbus.service.method(MANAGER, out_signature="a{oa{sa{sv}}}")
        def GetManagedObjects(self):
            return {
                o.path: {o.interface: o.properties, **o.extra}
                for o in [adapter] + devices
            }


    class Bluez(Object):
        def __init__(self, path, interface, properties, extra=None):
            super().__init__(bus, path, interface, properties)
            self.path = path
            self.extra = extra or {}

        @dbus.service.method(PROPERTIES, in_signature="ssv")
        def Set(self, interface, name, value):
            self.change({name: value})

        @dbus.service.method(DEVICE)
        def Connect(self):
            self.change({"Connected": dbus.Boolean(True)})

        @dbus.service.method(DEVICE)
        def Disconnect(self):
            self.change({"Connected": dbus.Boolean(False)})


    def device(address, name, icon, paired, extra=None):
        return Bluez(
            "/org/bluez/hci0/dev_00_00_00_00_00_" + address,
            DEVICE,
            {
                "Alias": dbus.String(name),
                "Icon": dbus.String(icon),
                "Paired": dbus.Boolean(paired),
                "Connected": dbus.Boolean(False),
            },
            extra,
        )


    name = dbus.service.BusName("org.bluez", bus)
    root = Root(bus, "/")
    adapter = Bluez(
        "/org/bluez/hci0", "org.bluez.Adapter1", {"Powered": dbus.Boolean(True)}
    )
    battery = {"org.bluez.Battery1": {"Percentage": dbus.Byte(80)}}
    devices = [
        device("01", "WH-1000XM4", "audio-headphones", True, battery),
        device("02", "Mouse", "input-mouse", True),
        device("03", "Phone", "phone", False),
    ]
    GLib.MainLoop().run()
  '';

  policy = pkgs.writeTextDir "share/dbus-1/system.d/fake-services.conf" ''
    <!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
      "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
    <busconfig>
      <policy user="root">
        <allow own="org.freedesktop.UPower"/>
        <allow own="org.freedesktop.UPower.PowerProfiles"/>
        <allow own="org.bluez"/>
      </policy>
      <policy context="default">
        <allow send_destination="org.freedesktop.UPower"/>
        <allow send_destination="org.freedesktop.UPower.PowerProfiles"/>
        <allow send_destination="org.bluez"/>
      </policy>
    </busconfig>
  '';
}
