//! Stub for platforms whose login item the GUI does not manage — the agent
//! reconciles its own autostart there (its `autostart` module).

use super::ServiceStatus;

pub(super) fn status() -> ServiceStatus {
    ServiceStatus::Unsupported
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Result is the cross-platform contract; this stub cannot fail"
)]
pub(super) fn ensure_registered() -> Result<(), String> {
    Ok(())
}

pub(super) fn open_login_items_settings() {}
