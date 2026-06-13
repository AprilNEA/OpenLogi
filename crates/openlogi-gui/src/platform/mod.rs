//! Platform and OS integration helpers.

pub mod os;
pub mod permissions;
#[cfg(target_os = "macos")]
pub mod spawn;
pub mod updater;
