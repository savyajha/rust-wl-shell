//! The screen's backlight: read from sysfs, followed through the kernel's notification on
//! `actual_brightness` (every write, by anyone, raises it), and set through logind, which
//! needs no permission beyond being the session's user.

use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{info, warn};
use zbus::proxy;

use crate::Update;

const BACKLIGHTS: &str = "/sys/class/backlight";

#[proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1",
    // The caller's session, or else its user's graphical one.
    default_path = "/org/freedesktop/login1/session/auto"
)]
trait LogindSession {
    fn set_brightness(&self, subsystem: &str, name: &str, brightness: u32) -> zbus::Result<()>;
}

/// Send the backlight's level in percent (`None`: no backlight) now and on each change, and
/// set the levels asked for on `requests`. `name` is the device, else the one
/// systemd-backlight and GNOME would pick. After an error it tries again every 5 s,
/// warning once while the error repeats; a device that went away is hidden until it is back.
pub async fn run(
    name: Option<String>,
    bus: Option<zbus::Connection>,
    updates: UnboundedSender<Update>,
    mut requests: UnboundedReceiver<u8>,
) {
    let Some(mut followed) = device(Path::new(BACKLIGHTS), name.as_deref()) else {
        info!("no backlight");
        let _ = updates.send(Update::Backlight(None));
        return;
    };
    let mut last_error = String::new();
    loop {
        // Ok only once the shell's updates are closed.
        let Err(e) = follow(&followed, bus.clone(), &updates, &mut requests).await else {
            return;
        };
        if e.to_string() != last_error {
            warn!("backlight {}: {e}", followed.display());
            last_error = e.to_string();
        }
        // Gone (a driver reloaded): hidden until it is back. Else (EIO with the panel off,
        // say) it stays shown as it was.
        if !followed.exists() && updates.send(Update::Backlight(None)).is_err() {
            return;
        }
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if let Some(found) = device(Path::new(BACKLIGHTS), name.as_deref()) {
                followed = found;
                break;
            }
        }
    }
}

async fn follow(
    device: &Path,
    bus: Option<zbus::Connection>,
    updates: &UnboundedSender<Update>,
    requests: &mut UnboundedReceiver<u8>,
) -> anyhow::Result<()> {
    let max = read(&device.join("max_brightness"))?;
    // The kernel's notification is on actual_brightness; reading it rearms it.
    let actual = File::open(device.join("actual_brightness"))?;
    // SAFETY: a `File` owns its descriptor, open until the `AsyncFd` drops it.
    let notified = unsafe { AsyncFd::register_with_interest(actual, Interest::PRIORITY) };
    let notified = notified.map_err(|e| e.into_parts().1)?;
    let session = match &bus {
        Some(bus) => Some(LogindSessionProxy::new(bus).await?),
        None => None,
    };
    let name = device.file_name().unwrap_or_default().to_string_lossy();
    let mut shown = None;
    loop {
        let mut rearm = [0; 32];
        notified.get_ref().read_at(&mut rearm, 0)?;
        let level = percent(read(&device.join("brightness"))?, max);
        if shown != Some(level) {
            shown = Some(level);
            if updates.send(Update::Backlight(Some(level))).is_err() {
                return Ok(());
            }
        }
        tokio::select! {
            ready = notified.ready(Interest::PRIORITY) => ready?.clear_ready(),
            Some(level) = requests.recv() => match &session {
                Some(session) => {
                    let value = raw(level, max);
                    if let Err(e) = session.set_brightness("backlight", &name, value).await {
                        warn!("setting the brightness through logind: {e}");
                    }
                }
                None => warn!("no system bus to set the brightness through"),
            },
        }
    }
}

/// The backlight named `name` in `dir`, else the one systemd-backlight and GNOME would pick:
/// by its `type`, firmware, then platform, then raw, and then by name.
fn device(dir: &Path, name: Option<&str>) -> Option<PathBuf> {
    if let Some(name) = name {
        let device = dir.join(name);
        return device.exists().then_some(device);
    }
    let rank = |device: &PathBuf| {
        let kind = fs::read_to_string(device.join("type")).unwrap_or_default();
        let rank = ["firmware", "platform", "raw"]
            .iter()
            .position(|k| *k == kind.trim());
        (rank.unwrap_or(3), device.clone())
    };
    let devices = fs::read_dir(dir).ok()?.flatten().map(|e| e.path());
    devices.min_by_key(rank)
}

fn read(path: &Path) -> anyhow::Result<u32> {
    Ok(fs::read_to_string(path)?.trim().parse()?)
}

/// `value` of `max` in linear percent, rounded, as brightnessctl shows it.
fn percent(value: u32, max: u32) -> u8 {
    let max = u64::from(max.max(1));
    ((u64::from(value).min(max) * 100 + max / 2) / max) as u8
}

/// The raw value for `percent` of `max`, at least 1 %: on many panels 0 is backlight off.
fn raw(percent: u8, max: u32) -> u32 {
    (u64::from(max) * u64::from(percent.clamp(1, 100)) / 100) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_linear_percent() {
        assert_eq!(percent(32768, 65535), 50);
        assert_eq!(percent(0, 65535), 0);
        assert_eq!(percent(65535, 65535), 100);
        assert_eq!(percent(70000, 65535), 100);
        assert_eq!(percent(1, 0), 100);
        assert_eq!(raw(50, 65535), 32767);
        assert_eq!(raw(120, 255), 255);
        assert_eq!(raw(0, 65535), 655);
        assert_eq!(percent(raw(37, 65535), 65535), 37);
    }

    #[test]
    fn the_named_device_or_the_first() {
        let dir = std::env::temp_dir().join(format!("backlights-{}", std::process::id()));
        for (name, kind) in [("intel_backlight", "raw"), ("amdgpu_bl1", "raw")] {
            fs::create_dir_all(dir.join(name)).unwrap();
            fs::write(dir.join(name).join("type"), format!("{kind}\n")).unwrap();
            fs::write(dir.join(name).join("max_brightness"), "65535\n").unwrap();
            fs::write(dir.join(name).join("brightness"), "16384\n").unwrap();
        }
        assert_eq!(device(&dir, None), Some(dir.join("amdgpu_bl1")));
        // A firmware backlight comes before raw ones, whatever its name.
        fs::create_dir_all(dir.join("zz_acpi")).unwrap();
        fs::write(dir.join("zz_acpi").join("type"), "firmware\n").unwrap();
        assert_eq!(device(&dir, None), Some(dir.join("zz_acpi")));
        assert_eq!(
            device(&dir, Some("intel_backlight")),
            Some(dir.join("intel_backlight"))
        );
        assert_eq!(device(&dir, Some("acpi_video0")), None);
        let amdgpu = dir.join("amdgpu_bl1");
        let level = percent(
            read(&amdgpu.join("brightness")).unwrap(),
            read(&amdgpu.join("max_brightness")).unwrap(),
        );
        assert_eq!(level, 25);
        assert_eq!(device(Path::new("/nonexistent"), None), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
