//! Everywhere but Windows: the LampArray lower filter and its service
//! registration are Windows constructs, so there is nothing to inspect.
//!
//! macOS and Linux share this one stub because they share the same answer —
//! the check does not apply — and a second file per platform would carry the
//! same empty body. The facade selects it with the same single `cfg` that
//! selects [`super::windows`], so a platform that gains a real implementation
//! replaces this module rather than growing a second one.

use anyhow::Result;

use super::HostState;

/// The check does not apply on this platform.
///
/// The `Result` is here so the facade can treat both platforms alike; this
/// arm has no registry to read and so nothing to fail on.
#[cfg_attr(
    not(target_os = "windows"),
    expect(
        clippy::unnecessary_wraps,
        reason = "the Windows arm reads the registry and can fail; this one has nothing to read"
    )
)]
pub(super) fn collect() -> Result<Option<HostState>> {
    Ok(None)
}
