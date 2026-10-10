//! Native Linux suspend/resume notifications from systemd-logind.
//!
//! `PrepareForSleep(true)` closes the device-I/O gate before suspend and
//! `PrepareForSleep(false)` reopens it after resume. The process-lifetime
//! listener reconnects only after D-Bus failure; steady state blocks on the
//! native signal and performs no timed inventory work.

use std::thread;
use std::time::Duration;

use openlogi_hid::DeviceIoSignal;
use tracing::{debug, info, warn};
use zbus::blocking::Connection;
use zbus::proxy;

const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1",
    gen_blocking = true
)]
trait LoginManager {
    #[zbus(signal, name = "PrepareForSleep")]
    fn prepare_for_sleep(&self, sleeping: bool) -> zbus::Result<()>;
}

/// Start a process-lifetime logind listener. Failure is non-fatal: the
/// inventory recovery scan still detects long suspend gaps from the clocks.
pub fn register(signal: DeviceIoSignal) {
    let spawned = thread::Builder::new()
        .name("openlogi-linux-resume".into())
        .spawn(move || {
            let mut reconnect_delay = RECONNECT_DELAY;
            loop {
                let failed = match listen(&signal) {
                    Ok(()) => {
                        warn!("logind resume signal stream ended; reconnecting");
                        reconnect_delay = RECONNECT_DELAY;
                        false
                    }
                    Err(error) if reconnect_delay == RECONNECT_DELAY => {
                        warn!(%error, "logind resume listener failed; reconnecting");
                        true
                    }
                    Err(error) => {
                        debug!(%error, "logind resume listener still unavailable");
                        true
                    }
                };
                thread::sleep(reconnect_delay);
                if failed {
                    reconnect_delay = next_reconnect_delay(reconnect_delay);
                }
            }
        });
    if let Err(error) = spawned {
        warn!(%error, "could not start logind resume listener; clock-gap recovery remains active");
    }
}

fn listen(signal: &DeviceIoSignal) -> zbus::Result<()> {
    let connection = Connection::system()?;
    let proxy = LoginManagerProxyBlocking::new(&connection)?;
    let changes = proxy.receive_prepare_for_sleep()?;
    info!("logind suspend/resume notifications registered");
    for change in changes {
        let args = change.args()?;
        if *args.sleeping() {
            let _ = signal.suspend();
        } else {
            let _ = signal.resume();
        }
    }
    Ok(())
}

fn next_reconnect_delay(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_RECONNECT_DELAY)
}

#[cfg(test)]
mod tests {
    use super::{MAX_RECONNECT_DELAY, RECONNECT_DELAY, next_reconnect_delay};

    #[test]
    fn reconnect_backoff_is_bounded() {
        let mut delay = RECONNECT_DELAY;
        for _ in 0..10 {
            delay = next_reconnect_delay(delay);
        }
        assert_eq!(delay, MAX_RECONNECT_DELAY);
    }
}
