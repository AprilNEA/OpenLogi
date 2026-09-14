//! Windows input hooks and foreground-application observation.
//!
//! Keep the cursor and worker models available to tests on every host;
//! native hooks and message pumps are only compiled on Windows.

mod cursor;
mod worker;

#[cfg(target_os = "windows")]
pub(crate) mod foreground;
#[cfg(target_os = "windows")]
mod hook;

#[cfg(target_os = "windows")]
pub(crate) use hook::Backend;
