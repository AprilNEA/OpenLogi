//! One command at a time per raw-HID route.
//!
//! A standalone device driven over raw reports has no HID++ channel to order
//! its traffic, so each raw driver holds its route's lock across the device's
//! I/O. The map is shared by every raw driver, so two drivers addressing one
//! device cannot interleave their reports either.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, PoisonError};

use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::channel::route::DeviceRoute;

/// The map itself is a `std::sync::Mutex`, held only to find a route's lock
/// and never across an await. Every runtime in the process shares it, so a
/// command that waited on it asynchronously would leave its own runtime idle
/// while another runtime held it, and paused time would run on past the
/// command.
static DEVICE_LOCKS: LazyLock<std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// The lock for `route`, held until the guard drops.
pub(super) async fn device_lock(route: &DeviceRoute) -> OwnedMutexGuard<()> {
    let lock = Arc::clone(
        DEVICE_LOCKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(route.to_string())
            .or_default(),
    );
    lock.lock_owned().await
}

#[cfg(test)]
mod tests {
    use std::sync::{PoisonError, TryLockError, mpsc};
    use std::time::Duration;

    use super::{DEVICE_LOCKS, device_lock};
    use crate::DeviceRoute;

    /// Another runtime holding the lock map must not leave this one idle:
    /// paused time would then run on past the command (#2446).
    #[tokio::test(start_paused = true)]
    async fn a_lock_map_held_by_another_runtime_does_not_let_paused_time_run() {
        let route = DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:device-lock-map".into(),
        };
        // The holder lets go after 200 ms. A lookup that starts later finds the
        // map free and proves nothing, so that round is run again.
        for _round in 0..5 {
            let (held, holding) = mpsc::channel();
            let holder = std::thread::spawn(move || {
                let _map = DEVICE_LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
                held.send(()).expect("the test waits for the held map");
                std::thread::sleep(Duration::from_millis(200));
            });
            holding.recv().expect("the holder takes the map");
            let contended = matches!(DEVICE_LOCKS.try_lock(), Err(TryLockError::WouldBlock));
            if contended {
                tokio::select! {
                    biased;
                    _guard = device_lock(&route) => {}
                    () = tokio::time::sleep(Duration::from_secs(5)) => {
                        panic!("paused time ran on while the command waited for the lock map");
                    }
                }
            }
            holder.join().expect("the holder does not panic");
            if contended {
                return;
            }
        }
        panic!("the lock map was already free each time the command looked it up");
    }
}
