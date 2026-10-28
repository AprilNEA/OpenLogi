//! Confirms a copy or cut with a haptic: after a Command-key press arms the
//! watch, a change of the general pasteboard is the copy that press made.
//!
//! The pasteboard announces nothing, so this polls its change count, and only
//! within [`openlogi_agent_core::event_haptics::COPY_WINDOW`] of a
//! Command-key press. The count it compares against is read as that press
//! wakes the watch: the hook sees the key before the app it is meant for
//! does, so a change made earlier — a clipboard manager's, say — is never
//! taken for the press's copy.

use std::time::{Duration, Instant};

use objc2_app_kit::NSPasteboard;
use openlogi_agent_core::event_haptics::{EventCue, EventHaptics};

/// How often the change count is read while a copy may be landing.
const COPY_POLL: Duration = Duration::from_millis(25);

/// The general pasteboard's change count. `changeCount` is safe on any
/// thread; the pool drains what AppKit autoreleases on this worker thread.
fn change_count() -> isize {
    objc2::rc::autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount())
}

/// Watch the pasteboard for copies for as long as the agent runs.
pub(crate) fn spawn(haptics: EventHaptics) {
    tokio::spawn(async move {
        loop {
            haptics.copy_keyed().await;
            let seen = change_count();
            while haptics.copy_armed(Instant::now()) {
                tokio::time::sleep(COPY_POLL).await;
                if change_count() != seen {
                    haptics.observe_host(EventCue::Copy);
                    break;
                }
            }
        }
    });
}
