//! The clock's text, sent on each minute and again after a resume: tokio's timers stop
//! during suspend, so the minute's sleep ends late.

use std::time::Duration;

use chrono::{Local, Timelike};
use futures_lite::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time;
use tracing::warn;
use zbus::proxy;

use crate::Update;

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LogindManager {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// How long logind gets to answer before the clock goes without its resume signal.
const BUS_TIMEOUT: Duration = Duration::from_secs(2);

/// Send the clock's text until `updates` closes. An NTP step or a time zone change while
/// awake shows only at the next minute, up to a minute late.
pub async fn run(format: String, updates: UnboundedSender<Update>) {
    let send = || {
        let text = Local::now().format(&format).to_string();
        updates.send(Update::Clock(text)).is_ok()
    };
    // The first text does not wait for the system bus.
    if !send() {
        return;
    }
    let mut resumes = match time::timeout(BUS_TIMEOUT, resumes()).await {
        Ok(Ok(resumes)) => Some(resumes),
        Ok(Err(e)) => {
            warn!("no resume signal from logind ({e}); the clock may lag after a suspend");
            None
        }
        Err(_) => {
            warn!("no resume signal: logind did not answer; the clock may lag after a suspend");
            None
        }
    };
    // Sent again at once: connecting to the bus may have taken past a minute's start.
    while send() {
        let now = Local::now();
        let sleep = time::sleep(until_next_minute(now.second(), now.nanosecond()));
        match &mut resumes {
            Some(resumes) => tokio::select! {
                () = sleep => {}
                // On either edge: the end of a resume is what matters, the start is harmless.
                Some(_) = resumes.next() => {}
            },
            None => sleep.await,
        }
    }
}

async fn resumes() -> zbus::Result<PrepareForSleepStream> {
    let bus = zbus::Connection::system().await?;
    LogindManagerProxy::new(&bus)
        .await?
        .receive_prepare_for_sleep()
        .await
}

/// From `second` and `nanosecond` past the minute (a leap second's nanoseconds run past
/// 10⁹) to the start of the next minute.
fn until_next_minute(second: u32, nanosecond: u32) -> Duration {
    let into_minute = Duration::new(second.into(), 0) + Duration::from_nanos(nanosecond.into());
    Duration::from_secs(60).saturating_sub(into_minute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleeps_until_the_next_minute() {
        assert_eq!(until_next_minute(0, 0), Duration::from_secs(60));
        assert_eq!(until_next_minute(41, 0), Duration::from_secs(19));
        assert_eq!(until_next_minute(59, 999_000_000), Duration::from_millis(1));
        assert_eq!(
            until_next_minute(30, 250_000_000),
            Duration::from_millis(29_750)
        );
    }

    #[test]
    fn a_leap_second_does_not_underflow() {
        assert_eq!(until_next_minute(59, 1_500_000_000), Duration::ZERO);
    }
}
