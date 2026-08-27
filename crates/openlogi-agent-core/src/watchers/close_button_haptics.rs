//! Cursor-over-close-button hover polling watcher.
//!
//! Unlike the other watchers in this module, the caller does not spawn this
//! one unconditionally at startup: the OS hit-test it polls is only worth
//! running while at least one connected device has the close-button haptic
//! setting enabled. The caller is expected to [`spawn`] this watcher when
//! that becomes true and drop the receiver when it stops being true — the
//! underlying `Poll` thread exits on its own the next time it finds the
//! receiver gone.

use std::time::Duration;

use tokio::sync::mpsc;

use super::poll::Poll;

/// Whether the cursor currently sits over a window's close button.
fn sample() -> bool {
    openlogi_hook::cursor_position().is_some_and(openlogi_hook::cursor_is_over_close_button)
}

/// Watch whether the cursor is hovering a window's close button, reporting
/// only the false→true and true→false transitions (`Poll::on_change`'s own
/// dedupe) — the consumer fires a haptic pulse on `true` only.
pub fn spawn(period: Duration) -> mpsc::UnboundedReceiver<bool> {
    Poll {
        name: "openlogi-close-button-watcher",
        period,
        degrades: "the close-button haptic pulse is disabled",
    }
    .on_change(sample)
}
