//! OS input-event synthesis split out of openlogi-core so the core stays platform- and IO-free.

mod inject;

pub use inject::{
    DockSwipeMotion, DockSwipePhase, HeldChord, SYNTHETIC_EVENT_USER_DATA, SmoothScrollPhase,
    ax_navigate_browser, dock_swipe_supported, execute, post_dock_swipe, post_scroll,
    post_smooth_scroll, post_touchpad_scroll, press_hold,
};

#[cfg(target_os = "linux")]
pub use inject::action_device_path;
