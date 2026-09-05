//! The Actions Ring's shared display-duration constant and geometry.
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

/// The ring's shape, in points. The overlay draws the real ring from these and
/// the settings app draws its preview from them, so the preview shows the ring
/// the user will actually get. The radius is the mouse travel to any slot, so
/// it is as tight as the slots allow: eight slots of [`SLOT_DIAMETER`] on
/// [`RADIUS`] leave a 16 pt gap between neighbours — enough for the desktop to
/// read through and for a slot's hover edge to be unambiguous.
pub mod geometry {
    /// Diameter of each of the eight slot buttons.
    pub const SLOT_DIAMETER: f32 = 48.0;
    /// Distance from the ring's centre (the cursor) to each slot's centre.
    pub const RADIUS: f32 = 84.0;
    /// Diameter of the central cancel button.
    pub const CANCEL_DIAMETER: f32 = 36.0;
}
