//! Which Xcode the macOS build runs under.
//!
//! Every developer tool the bundle build shells out to — the cargo builds that
//! link against the macOS SDK, and `actool` — has to be the same one, or a
//! bundle is assembled from two toolchains. `OPENLOGI_DEVELOPER_DIR` picks it;
//! without one, whatever `/Applications/Xcode.app` points at.
//!
//! That default is why this exists as a knob at all: a machine — a CI runner
//! especially — can have several Xcodes installed with the symlink aimed at an
//! old one. Compiling an Icon Composer document needs Xcode 26 or newer.

use std::env;

use anyhow::Result;
use xshell::{Shell, cmd};

/// The environment every macOS build command runs with: the chosen developer
/// directory, and the SDK path that Xcode resolves to.
pub(crate) fn env() -> Result<Vec<(String, String)>> {
    let sh = Shell::new()?;
    let developer_dir = env::var("OPENLOGI_DEVELOPER_DIR")
        .unwrap_or_else(|_| "/Applications/Xcode.app/Contents/Developer".to_string());
    let sdkroot = cmd!(sh, "/usr/bin/xcrun --sdk macosx --show-sdk-path")
        .env("DEVELOPER_DIR", &developer_dir)
        .read()?;
    Ok(vec![
        ("DEVELOPER_DIR".to_string(), developer_dir),
        ("SDKROOT".to_string(), sdkroot.trim().to_string()),
    ])
}
