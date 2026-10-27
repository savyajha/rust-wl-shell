# Fake system services for the VM test, in dbus-python (as the greeter's tests have them):
# UPower's DisplayDevice and power-profiles-daemon, with as much of each as the shell reads.
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

  policy = pkgs.writeTextDir "share/dbus-1/system.d/fake-services.conf" ''
    <!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-BUS Bus Configuration 1.0//EN"
      "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
    <busconfig>
      <policy user="root">
        <allow own="org.freedesktop.UPower"/>
        <allow own="org.freedesktop.UPower.PowerProfiles"/>
      </policy>
      <policy context="default">
        <allow send_destination="org.freedesktop.UPower"/>
        <allow send_destination="org.freedesktop.UPower.PowerProfiles"/>
      </policy>
    </busconfig>
  '';
}
