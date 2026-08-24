//! Locking shared by the crate's `std::sync::Mutex` holders.

use std::sync::{Mutex, MutexGuard};

/// Locks `mutex`, treating poisoning as unrecoverable: a panicking holder
/// leaves the guarded structure — a channel's pending queues, an emitter's
/// sender list — in an inconsistent state, so continuing would operate on
/// corrupt data.
#[expect(
    clippy::expect_used,
    reason = "mutex poisoning is unrecoverable here — see doc comment"
)]
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("mutex poisoned")
}
