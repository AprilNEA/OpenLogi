//! OS input-event synthesis split out of openlogi-core so the core stays platform- and IO-free.

mod inject;
#[cfg(target_os = "linux")]
pub mod uinput;

pub use inject::{
    HeldChord, SYNTHETIC_EVENT_USER_DATA, SmoothScrollPhase, ax_navigate_browser, execute,
    natural_scrolling, post_phased_scroll, post_scroll, post_smooth_scroll, press_hold,
    press_hold_modifier,
};

#[cfg(target_os = "linux")]
pub use inject::action_device_path;
#[cfg(target_os = "macos")]
pub use inject::held_modifier_flags;
#[cfg(target_os = "macos")]
pub use inject::prepare_menu_shortcuts;
