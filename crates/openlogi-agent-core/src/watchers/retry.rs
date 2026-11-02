//! Retry pacing shared by the HID++ session managers.

use std::time::Duration;

use tokio::time::Instant;

/// How long a manager waits before retrying a firmware restore that stayed
/// pending, or re-arming a session that ended unexpectedly or found the
/// receiver leased.
pub(super) const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Sleep until `deadline`. A manager with nothing due waits here forever,
/// leaving its `select!` to the arms that can still make progress.
pub(super) async fn wait_for_deadline(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}
