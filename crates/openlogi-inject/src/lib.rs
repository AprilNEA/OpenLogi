//! OS input-event synthesis split out of openlogi-core so the core stays platform- and IO-free.

mod inject;

pub use inject::{
    HeldChord, SYNTHETIC_EVENT_USER_DATA, SmoothScrollPhase, ax_navigate_browser, execute,
    flush_gesture_sessions, post_pan, post_pan_begin, post_pan_end, post_scroll, post_smart_zoom,
    post_smooth_scroll, post_zoom_continuous, post_zoom_end, press_hold, seal_gesture_sessions,
};

#[cfg(target_os = "linux")]
pub use inject::action_device_path;
