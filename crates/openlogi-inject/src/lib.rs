//! OS input-event synthesis split out of openlogi-core so the core stays platform- and IO-free.

mod inject;

pub use inject::{SYNTHETIC_EVENT_USER_DATA, execute, post_horizontal_scroll};

#[cfg(target_os = "linux")]
pub use inject::action_device_path;
