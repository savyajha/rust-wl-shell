import time

# Helpers for the VM tests: the pointer, through QMP input-send-event on the USB tablet,
# whose range covers the screen (SCREEN, in pixels, which the test defines), and the
# screen, read with QEMU's screendump of the virtio-gpu `gpu`.

def input_events(*events):
    assert machine.qmp_client is not None
    machine.qmp_client.send("input-send-event", {"events": list(events)})  # type: ignore

def move(x, y):
    """The pointer to (x, y), through the absolute tablet."""
    def scaled(v, size):
        return round(v * 0x7fff / size)
    input_events(
        {"type": "abs", "data": {"axis": "x", "value": scaled(x, SCREEN[0])}},
        {"type": "abs", "data": {"axis": "y", "value": scaled(y, SCREEN[1])}},
    )

def click(x, y):
    """A click at (x, y), moving there from a pixel away: the tablet reports only changes."""
    move(x + 1, y)
    move(x, y)
    for down in [True, False]:
        input_events({"type": "btn", "data": {"down": down, "button": "left"}})
        time.sleep(0.1)

def screen(head=0):
    """An output (head 0: Virtual-1) from QEMU's screendump: its width and its RGB bytes."""
    path = machine.out_dir / "screen.ppm"
    machine.send_monitor_command(f"screendump {path} gpu {head}")
    data = path.read_bytes()
    header = re.match(rb"P6\s+(\d+)\s+(\d+)\s+\d+\s", data)
    assert header is not None
    return int(header.group(1)), data[header.end():]

def pixel(shot, x, y):
    """The pixel at (x, y) of a `screen()`."""
    width, pixels = shot
    i = (y * width + x) * 3
    return tuple(pixels[i:i + 3])
