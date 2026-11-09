//! Platform and OS integration helpers.

pub mod app_icon;
pub mod installation;
pub mod os;
#[cfg(target_os = "macos")]
pub(crate) mod permission_guide;
pub mod registration;
pub mod updater;
