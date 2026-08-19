//! The Actions Ring's shared display-duration constant.
//!
//! The ring's runtime session state (`ActionRingManager`: `Mutex`, `Notify`,
//! `Instant`) is agent-only and stays in `openlogi-agent-core` — it is not a
//! shared contract. This constant is, though: the agent derives the
//! session's expiry window from it and the GUI's overlay helper times its own
//! window by it, so the two clocks cannot drift out of step. The ring's
//! persisted layout/config schema ([`crate::binding::ActionRingLayout`] and
//! friends) is a separate concern, unrelated to this runtime timing value.

use std::time::Duration;

/// How long the overlay keeps the ring on screen, counted from the moment its
/// window opens. The overlay owns the display; the constant lives here so the
/// session that has to outlive it is derived from it rather than kept in step
/// by hand.
pub const DISPLAY_LIFETIME: Duration = Duration::from_secs(15);
