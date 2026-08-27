//! The crate's one timer story, for native and wasm alike.
//!
//! `openlogi-device` exists to be host-free: the WebHID backend is meant to
//! drive it from a browser, which is why CI's `wasm (portable crates)` job
//! holds this crate to `wasm32-unknown-unknown`. `tokio::time` passed that job
//! and would still have been wrong — it *compiles* for wasm and panics the
//! first time a timer is polled, and a `cargo check` cannot see that.
//!
//! `futures-timer` is the timer both targets share. It was already in the
//! dependency graph — `openlogi-hidpp`, the other crate on the portable list,
//! times its own reads with it — so the global timer thread it costs on native
//! is paid whether or not this crate joins, and nothing here has to know which
//! target it is on.
//!
//! [`timeout`] is written once on top of [`sleep`] because `futures-timer`
//! ships no timeout combinator. That is the whole reason this module exists
//! rather than each call site reaching for `Delay` itself.

use std::future::Future;
use std::time::Duration;

use futures_timer::Delay;

/// The error [`timeout`] returns when its budget ran out first.
///
/// Deliberately carries nothing: every caller in this crate treats a timeout
/// as one opaque outcome, and a payload would only invite matching on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the operation did not finish within its budget")]
pub struct Elapsed;

/// Complete after `duration`.
#[must_use]
pub fn sleep(duration: Duration) -> Delay {
    Delay::new(duration)
}

/// Run `future` with a time budget, yielding [`Elapsed`] if it runs out.
///
/// The future is polled before the timer on every wake, so one that is already
/// ready wins a zero-length budget rather than racing it — the behaviour
/// `tokio::time::timeout` documents, and what the probe paths here rely on
/// when a cached answer resolves immediately.
pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, Elapsed>
where
    F: Future,
{
    futures_lite::future::or(async move { Ok(future.await) }, async move {
        sleep(duration).await;
        Err(Elapsed)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_future_that_finishes_in_budget_yields_its_output() {
        let out = timeout(Duration::from_secs(30), async { 7 }).await;
        assert_eq!(out, Ok(7));
    }

    /// The left side is polled first, so an already-ready future beats even a
    /// zero budget. The probe paths depend on this: a cached answer resolves
    /// without ever suspending, and must not be reported as timed out.
    #[tokio::test]
    async fn an_already_ready_future_wins_a_zero_budget() {
        let out = timeout(Duration::ZERO, async { "cached" }).await;
        assert_eq!(out, Ok("cached"));
    }

    #[tokio::test]
    async fn a_future_that_never_finishes_elapses() {
        let out = timeout(Duration::from_millis(1), std::future::pending::<()>()).await;
        assert_eq!(out, Err(Elapsed));
    }
}
