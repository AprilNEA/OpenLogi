//! Host accessibility policy for translucent window backgrounds.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub(crate) use macos::{allows_glass, install};
#[cfg(not(target_os = "macos"))]
pub(crate) use unsupported::{allows_glass, install};
