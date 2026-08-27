//! Host-wide primary mouse button integration.
//!
//! IPC and the agent runtime use this platform-neutral facade. Each operating
//! system supplies one backend implementing [`Backend`]; polling and error
//! semantics stay shared so adding a backend does not fork agent behavior.

use std::time::Duration;

use openlogi_ipc::{PrimaryMouseButton, SystemMouseSettingError};
use tokio::sync::mpsc;
use tracing::{info, warn};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(not(target_os = "macos"), test))]
mod unsupported;

#[cfg(target_os = "macos")]
use macos::MacOsBackend as ActiveBackend;
#[cfg(not(target_os = "macos"))]
use unsupported::UnsupportedBackend as ActiveBackend;

/// Contract implemented by the one backend selected for the target OS.
///
/// `is_available` may inspect the current desktop session, which lets a future
/// Linux implementation decline unsupported compositors at runtime.
trait Backend {
    const NAME: &'static str;

    fn is_available() -> bool;
    fn read() -> Result<PrimaryMouseButton, SystemMouseSettingError>;
    fn set(button: PrimaryMouseButton) -> Result<PrimaryMouseButton, SystemMouseSettingError>;
}

pub(crate) fn read() -> Result<PrimaryMouseButton, SystemMouseSettingError> {
    ActiveBackend::read()
}

pub(crate) fn set(
    button: PrimaryMouseButton,
) -> Result<PrimaryMouseButton, SystemMouseSettingError> {
    ActiveBackend::set(button)
}

/// Poll the externally mutable setting and publish edges.
///
/// System settings tools can change the value without going through OpenLogi,
/// so a successful OpenLogi write is never treated as the only source of truth.
/// Unsupported desktop sessions return `None` and consume no background task.
pub(crate) fn spawn(period: Duration) -> Option<mpsc::UnboundedReceiver<PrimaryMouseButton>> {
    if !ActiveBackend::is_available() {
        return None;
    }

    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last = None;
        let mut failed = false;
        loop {
            interval.tick().await;
            match ActiveBackend::read() {
                Ok(button) => {
                    if failed {
                        info!(
                            backend = ActiveBackend::NAME,
                            "primary mouse button watcher recovered"
                        );
                    }
                    failed = false;
                    if last != Some(button) {
                        last = Some(button);
                        if tx.send(button).is_err() {
                            return;
                        }
                    }
                }
                Err(error) => {
                    if !failed {
                        warn!(
                            backend = ActiveBackend::NAME,
                            ?error,
                            "could not read the primary mouse button"
                        );
                    }
                    failed = true;
                }
            }
        }
    });
    Some(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_backend_declines_the_capability() {
        assert!(!unsupported::UnsupportedBackend::is_available());
        assert!(matches!(
            unsupported::UnsupportedBackend::read(),
            Err(SystemMouseSettingError::Unsupported)
        ));
        assert!(matches!(
            unsupported::UnsupportedBackend::set(PrimaryMouseButton::Right),
            Err(SystemMouseSettingError::Unsupported)
        ));
    }
}
