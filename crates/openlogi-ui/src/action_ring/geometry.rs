//! Radial slot placement shared by the overlay and settings preview.

use openlogi_core::binding::ActionRingSlot;

/// Unit vector for a slot, with positive Y pointing down in GPUI coordinates.
#[must_use]
pub fn slot_offset(slot: ActionRingSlot) -> (f32, f32) {
    let diagonal = std::f32::consts::FRAC_1_SQRT_2;
    match slot {
        ActionRingSlot::Top => (0.0, -1.0),
        ActionRingSlot::TopRight => (diagonal, -diagonal),
        ActionRingSlot::Right => (1.0, 0.0),
        ActionRingSlot::BottomRight => (diagonal, diagonal),
        ActionRingSlot::Bottom => (0.0, 1.0),
        ActionRingSlot::BottomLeft => (-diagonal, diagonal),
        ActionRingSlot::Left => (-1.0, 0.0),
        ActionRingSlot::TopLeft => (-diagonal, -diagonal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinal_slots_use_screen_coordinates() {
        assert_eq!(slot_offset(ActionRingSlot::Top), (0.0, -1.0));
        assert_eq!(slot_offset(ActionRingSlot::Right), (1.0, 0.0));
        assert_eq!(slot_offset(ActionRingSlot::Bottom), (0.0, 1.0));
        assert_eq!(slot_offset(ActionRingSlot::Left), (-1.0, 0.0));
    }
}
