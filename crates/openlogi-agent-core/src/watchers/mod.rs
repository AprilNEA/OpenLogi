//! Background watchers that poll external state — HID inventory, foreground
//! app, Accessibility, device pairing — and forward changes over channels to a
//! consumer (the agent's orchestrator, or the GUI).

pub mod accessibility;
pub mod camera;
pub mod foreground_app;
pub mod gesture;
pub mod host_switch;
pub mod input_monitoring;
pub mod inventory;
pub mod keyboard;
pub mod pairing;
mod poll;

/// Handle one `ReceiverAccess::watch_exclusive` edge inside a watcher's
/// select loop: pull the next management tick forward so the same reconcile
/// body runs immediately, instead of up to a full interval later. On a closed
/// channel (`changed_ok == false`) — the sender lives inside `ReceiverAccess`
/// for the process lifetime, so never in production — clear `open` so the arm
/// disarms and the plain ticker remains.
pub(crate) fn pull_tick_forward(
    changed_ok: bool,
    ticker: &mut tokio::time::Interval,
    open: &mut bool,
) {
    if changed_ok {
        ticker.reset_immediately();
    } else {
        *open = false;
    }
}
