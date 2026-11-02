//! The live Accessibility grant: the capability probe, and the cue that says
//! when the tap thread runs it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CallbackResult,
};
use tracing::warn;

/// How long the live tap goes without a capability probe when no grant
/// notification arrives. Every System Settings edit measured so far posts one,
/// so this is the backstop for a lost notification, not the cadence: a
/// revocation nobody announced is caught within this period plus one run-loop
/// slice.
pub(super) const PROBE_HEARTBEAT: Duration = Duration::from_secs(5);

/// Can this process create an *active* (event-filtering) tap right now?
///
/// The probe mirrors the real tap's location, placement and options — that is
/// the capability being tested — but subscribes to `kCGEventNull`, an event
/// type nothing ever posts, so it cannot gate a single real event during the
/// microseconds it exists. Dropping it invalidates the port.
pub(super) fn can_filter_events() -> bool {
    CGEventTap::new(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![CGEventType::Null],
        |_proxy: CGEventTapProxy, _etype: CGEventType, _event: &CGEvent| CallbackResult::Keep,
    )
    .is_ok()
}

/// When the between-slice capability probe runs.
///
/// A probe is a WindowServer round trip that takes seconds around a sleep
/// transition (#952), so the tap thread runs one only when the grant may have
/// changed: macOS posted a privacy notification, or [`PROBE_HEARTBEAT`] passed
/// without one. The first slice after arming always probes.
pub(super) enum ProbeCue {
    /// The notifications cannot be observed: probe every slice, as the tap
    /// did before it had them.
    EverySlice,
    /// Probe when the watch flagged a wake since the last probe.
    OnWake {
        due: Arc<AtomicBool>,
        /// Dropped with the cue, which unregisters its observers.
        _watch: axwatch::Watch,
    },
}

impl ProbeCue {
    /// Start watching the grant. The handler runs on the main run loop and on
    /// the heartbeat thread, and does nothing but raise the flag.
    pub(super) fn arm() -> Self {
        let due = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&due);
        match axwatch::Watch::start(Some(PROBE_HEARTBEAT), move |_| {
            flag.store(true, Ordering::Release);
        }) {
            Ok(watch) => Self::OnWake { due, _watch: watch },
            Err(error) => {
                warn!(
                    %error,
                    "could not watch the Accessibility grant — probing it every slice instead"
                );
                Self::EverySlice
            }
        }
    }

    /// Whether this slice probes. Consumes the wake it answers.
    pub(super) fn take(&self) -> bool {
        match self {
            Self::EverySlice => true,
            Self::OnWake { due, .. } => due.swap(false, Ordering::AcqRel),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wake_driven_cue_probes_on_the_first_slice_and_once_per_wake() {
        let due = Arc::new(AtomicBool::new(true));
        let cue = ProbeCue::OnWake {
            due: Arc::clone(&due),
            _watch: axwatch::Watch::start(None, |_| {}).expect("registration needs no run loop"),
        };
        assert!(cue.take(), "the slice after arming probes");
        assert!(!cue.take(), "nothing woke the watch since");
        due.store(true, Ordering::Release);
        assert!(cue.take(), "a wake buys exactly one probe");
        assert!(!cue.take());
    }
}
