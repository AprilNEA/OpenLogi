//! Confirms a copy or cut with a haptic: after a Command-key press arms the
//! watch, a change of the general pasteboard is the copy that press made.
//!
//! The pasteboard announces nothing, so this polls its change count — only
//! within [`openlogi_agent_core::event_haptics::COPY_WINDOW`] of a
//! Command-key press, and otherwise slowly enough
//! to keep the count it compares against fresh.

use std::time::{Duration, Instant};

use objc2_app_kit::NSPasteboard;
use openlogi_agent_core::event_haptics::{EventCue, EventHaptics};

/// How often the change count is read while a copy may be landing.
const COPY_POLL: Duration = Duration::from_millis(25);

/// How often the change count is re-read while nothing is armed, so a
/// change another app made long before a Command-key press is not taken for
/// that press's copy.
const BASELINE_REFRESH: Duration = Duration::from_secs(2);

/// The general pasteboard's change count. `changeCount` is safe on any
/// thread; the pool drains what AppKit autoreleases on this worker thread.
fn change_count() -> isize {
    objc2::rc::autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount())
}

/// Watch the pasteboard for copies for as long as the agent runs.
pub(crate) fn spawn(haptics: EventHaptics) {
    tokio::spawn(async move {
        let mut seen = change_count();
        loop {
            tokio::select! {
                () = haptics.copy_keyed() => {}
                () = tokio::time::sleep(BASELINE_REFRESH) => {
                    seen = change_count();
                    continue;
                }
            }
            while haptics.copy_armed(Instant::now()) {
                let count = change_count();
                if count != seen {
                    haptics.observe_host(EventCue::Copy);
                    break;
                }
                tokio::time::sleep(COPY_POLL).await;
            }
            // Whatever landed in the window is accounted for now.
            seen = change_count();
        }
    });
}
