//! The key and wheel sequence one [`ScrollDelivery`] lane injects for a batch
//! of horizontal notches.

use openlogi_core::config::KeyModifiers;
use openlogi_core::scroll::ScrollDelivery;

/// One wheel notch in high-resolution wheel units.
const NOTCH: i32 = 120;

/// The longest sequence a lane produces: two modifiers down, the wheel, the
/// mask tap, and two modifiers up.
pub(super) const MAX_STEPS: usize = 7;

/// A key a delivery lane presses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompatKey {
    Shift,
    Control,
    Alt,
    /// A key with no meaning of its own, tapped while the injected modifiers
    /// are still down, so their release never reads as a lone modifier tap:
    /// the press an input method may toggle its mode on, or that activates a
    /// menu bar.
    Mask,
}

impl CompatKey {
    fn is_held(self, held: KeyModifiers) -> bool {
        match self {
            Self::Shift => held.shift,
            Self::Control => held.control,
            Self::Alt => held.option,
            Self::Mask => false,
        }
    }
}

/// One synthetic input event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompatStep {
    KeyDown(CompatKey),
    KeyUp(CompatKey),
    /// Vertical wheel distance in high-resolution units; positive scrolls up.
    Wheel(i32),
    /// Horizontal wheel distance in high-resolution units; positive scrolls
    /// right.
    HorizontalWheel(i32),
}

/// A fixed-capacity step sequence, so building one never allocates.
#[derive(Clone, Copy, Debug)]
pub(super) struct StepBuf {
    steps: [CompatStep; MAX_STEPS],
    len: usize,
}

impl StepBuf {
    const fn new() -> Self {
        Self {
            steps: [CompatStep::Wheel(0); MAX_STEPS],
            len: 0,
        }
    }

    fn push(&mut self, step: CompatStep) {
        self.steps[self.len] = step;
        self.len += 1;
    }

    pub(super) fn as_slice(&self) -> &[CompatStep] {
        &self.steps[..self.len]
    }
}

/// The sequence that delivers `notches` whole horizontal notches through
/// `delivery`; positive notches scroll right, which the vertical lanes turn
/// into wheel-down.
///
/// A modifier in `held` is already down, so it is neither pressed nor
/// released and the physical key keeps its state. Injected modifiers are
/// released in reverse order, after one mask tap.
pub(super) fn compat_steps(delivery: ScrollDelivery, notches: i32, held: KeyModifiers) -> StepBuf {
    let mut steps = StepBuf::new();
    if notches == 0 {
        return steps;
    }
    let distance = notches.saturating_mul(NOTCH);
    let modifiers: &[CompatKey] = match delivery {
        ScrollDelivery::Native => {
            steps.push(CompatStep::HorizontalWheel(distance));
            return steps;
        }
        ScrollDelivery::ShiftWheel => &[CompatKey::Shift],
        ScrollDelivery::CtrlShiftWheel => &[CompatKey::Control, CompatKey::Shift],
        ScrollDelivery::AltWheel => &[CompatKey::Alt],
        ScrollDelivery::VerticalWheel => &[],
        ScrollDelivery::CtrlWheel => &[CompatKey::Control],
    };
    let injected = |key: &&CompatKey| !key.is_held(held);
    for &key in modifiers.iter().filter(injected) {
        steps.push(CompatStep::KeyDown(key));
    }
    steps.push(CompatStep::Wheel(distance.saturating_neg()));
    if modifiers.iter().any(|key| !key.is_held(held)) {
        steps.push(CompatStep::KeyDown(CompatKey::Mask));
        steps.push(CompatStep::KeyUp(CompatKey::Mask));
    }
    for &key in modifiers.iter().rev().filter(injected) {
        steps.push(CompatStep::KeyUp(key));
    }
    steps
}

/// The releases still owed once only the first `sent` of `steps` were
/// injected: every modifier pressed and not yet released, in the sequence's
/// release order, behind the mask tap unless it already landed. Nothing is
/// owed when no injected modifier is left down; the lost wheel is not retried.
pub(super) fn unreleased_steps(steps: &[CompatStep], sent: usize) -> StepBuf {
    let mut owed = StepBuf::new();
    let Some(unsent) = steps.get(sent..) else {
        return owed;
    };
    let pressed =
        |key: CompatKey| key != CompatKey::Mask && !unsent.contains(&CompatStep::KeyDown(key));
    if !unsent
        .iter()
        .any(|step| matches!(*step, CompatStep::KeyUp(key) if pressed(key)))
    {
        return owed;
    }
    for &step in unsent {
        match step {
            CompatStep::KeyDown(CompatKey::Mask) | CompatStep::KeyUp(CompatKey::Mask) => {
                owed.push(step);
            }
            CompatStep::KeyUp(key) if pressed(key) => owed.push(step),
            _ => {}
        }
    }
    owed
}

#[cfg(test)]
mod tests {
    use super::CompatKey::{Alt, Control, Mask, Shift};
    use super::CompatStep::{HorizontalWheel, KeyDown, KeyUp, Wheel};
    use super::*;

    fn modifiers(shift: bool, control: bool, option: bool) -> KeyModifiers {
        KeyModifiers {
            shift,
            control,
            option,
            command: false,
        }
    }

    fn steps(delivery: ScrollDelivery, notches: i32, held: KeyModifiers) -> Vec<CompatStep> {
        compat_steps(delivery, notches, held).as_slice().to_vec()
    }

    #[test]
    fn shift_lane_wraps_one_notch_and_masks_the_release() {
        assert_eq!(
            steps(ScrollDelivery::ShiftWheel, 1, KeyModifiers::default()),
            [
                KeyDown(Shift),
                Wheel(-120),
                KeyDown(Mask),
                KeyUp(Mask),
                KeyUp(Shift)
            ]
        );
    }

    #[test]
    fn a_held_modifier_is_neither_pressed_nor_released() {
        assert_eq!(
            steps(ScrollDelivery::ShiftWheel, 1, modifiers(true, false, false)),
            [Wheel(-120)]
        );
        assert_eq!(
            steps(
                ScrollDelivery::CtrlShiftWheel,
                1,
                modifiers(false, true, false)
            ),
            [
                KeyDown(Shift),
                Wheel(-120),
                KeyDown(Mask),
                KeyUp(Mask),
                KeyUp(Shift)
            ]
        );
    }

    #[test]
    fn ctrl_shift_lane_releases_in_reverse_after_the_mask() {
        assert_eq!(
            steps(ScrollDelivery::CtrlShiftWheel, 2, KeyModifiers::default()),
            [
                KeyDown(Control),
                KeyDown(Shift),
                Wheel(-240),
                KeyDown(Mask),
                KeyUp(Mask),
                KeyUp(Shift),
                KeyUp(Control)
            ]
        );
    }

    #[test]
    fn alt_lane_masks_its_release() {
        assert_eq!(
            steps(ScrollDelivery::AltWheel, -1, KeyModifiers::default()),
            [
                KeyDown(Alt),
                Wheel(120),
                KeyDown(Mask),
                KeyUp(Mask),
                KeyUp(Alt)
            ]
        );
    }

    #[test]
    fn scrolling_left_through_the_ctrl_lane_zooms_in() {
        assert_eq!(
            steps(ScrollDelivery::CtrlWheel, -1, KeyModifiers::default()),
            [
                KeyDown(Control),
                Wheel(120),
                KeyDown(Mask),
                KeyUp(Mask),
                KeyUp(Control)
            ]
        );
    }

    #[test]
    fn unmodified_lanes_inject_only_the_wheel() {
        assert_eq!(
            steps(ScrollDelivery::VerticalWheel, 3, KeyModifiers::default()),
            [Wheel(-360)]
        );
        assert_eq!(
            steps(ScrollDelivery::Native, -2, KeyModifiers::default()),
            [HorizontalWheel(-240)]
        );
    }

    #[test]
    fn zero_notches_inject_nothing() {
        for delivery in ScrollDelivery::ALL {
            assert!(
                steps(delivery, 0, KeyModifiers::default()).is_empty(),
                "{delivery:?}"
            );
        }
    }

    /// Every lane, direction and held-modifier combination: one wheel event
    /// whose sign follows the lane, each injected modifier released in
    /// reverse order, and a mask tap before the first release exactly when a
    /// modifier was injected.
    #[test]
    fn every_sequence_is_balanced_masked_and_signed() {
        for delivery in ScrollDelivery::ALL {
            for notches in [-20, -3, -1, 1, 2, 20] {
                for bits in 0..8 {
                    let held = modifiers(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
                    let sequence = steps(delivery, notches, held);
                    let context = format!("{delivery:?} {notches} {held:?}: {sequence:?}");

                    let wheels: Vec<_> = sequence
                        .iter()
                        .filter(|step| matches!(step, Wheel(_) | HorizontalWheel(_)))
                        .collect();
                    let expected = if delivery.is_native() {
                        HorizontalWheel(notches * 120)
                    } else {
                        Wheel(-notches * 120)
                    };
                    assert_eq!(wheels, [&expected], "{context}");

                    let downs: Vec<_> = sequence
                        .iter()
                        .filter_map(|step| match step {
                            KeyDown(key) if *key != Mask => Some(*key),
                            _ => None,
                        })
                        .collect();
                    let ups: Vec<_> = sequence
                        .iter()
                        .filter_map(|step| match step {
                            KeyUp(key) if *key != Mask => Some(*key),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(
                        ups,
                        downs.iter().rev().copied().collect::<Vec<_>>(),
                        "{context}"
                    );
                    assert!(
                        downs.iter().all(|key| !key.is_held(held)),
                        "a held modifier was injected: {context}"
                    );

                    let mask = sequence.iter().position(|step| *step == KeyDown(Mask));
                    if downs.is_empty() {
                        assert_eq!(mask, None, "{context}");
                    } else {
                        let mask = mask.expect("a mask tap guards injected modifiers");
                        let wheel = sequence
                            .iter()
                            .position(|step| matches!(step, Wheel(_)))
                            .expect("one wheel event");
                        let first_release = sequence
                            .iter()
                            .position(|step| matches!(step, KeyUp(key) if *key != Mask))
                            .expect("an injected modifier is released");
                        assert!(wheel < mask, "{context}");
                        assert_eq!(sequence[mask + 1], KeyUp(Mask), "{context}");
                        assert!(mask + 1 < first_release, "{context}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_short_batch_is_followed_by_the_releases_it_still_owes() {
        assert_eq!(
            unreleased_steps(
                &steps(ScrollDelivery::CtrlShiftWheel, 1, KeyModifiers::default()),
                1
            )
            .as_slice(),
            [KeyDown(Mask), KeyUp(Mask), KeyUp(Control)],
            "only Ctrl went down, so only Ctrl comes up, still masked"
        );
        assert_eq!(
            unreleased_steps(
                &steps(ScrollDelivery::ShiftWheel, 1, KeyModifiers::default()),
                3
            )
            .as_slice(),
            [KeyUp(Mask), KeyUp(Shift)],
            "a mask already down only needs its release"
        );
    }

    /// Whatever prefix `SendInput` accepts, it plus the owed releases leave
    /// no key down, and no modifier comes up without a mask press after it
    /// went down: the release never reads as a lone tap.
    #[test]
    fn every_short_batch_ends_with_every_key_up_and_no_lone_modifier_tap() {
        for delivery in ScrollDelivery::ALL {
            for notches in [-2, 1] {
                for bits in 0..8 {
                    let held = modifiers(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
                    let sequence = steps(delivery, notches, held);
                    for sent in 0..=sequence.len() {
                        let owed = unreleased_steps(&sequence, sent).as_slice().to_vec();
                        let context = format!("{delivery:?} {held:?} sent {sent}: {owed:?}");
                        assert!(
                            owed.iter()
                                .all(|step| matches!(step, KeyDown(Mask) | KeyUp(_))),
                            "only releases and the mask are retried: {context}"
                        );
                        if sent == sequence.len() {
                            assert!(owed.is_empty(), "{context}");
                        }

                        // Each key down, with whether a mask press followed it.
                        let mut down: Vec<(CompatKey, bool)> = Vec::new();
                        for step in sequence[..sent].iter().chain(&owed) {
                            match *step {
                                KeyDown(Mask) => {
                                    for entry in &mut down {
                                        entry.1 = true;
                                    }
                                    down.push((Mask, true));
                                }
                                KeyDown(key) => down.push((key, false)),
                                KeyUp(key) => {
                                    let at = down
                                        .iter()
                                        .position(|(pressed, _)| *pressed == key)
                                        .expect("only a key that is down is released");
                                    assert!(down.remove(at).1, "a lone tap: {context}");
                                }
                                Wheel(_) | HorizontalWheel(_) => {}
                            }
                        }
                        assert!(down.is_empty(), "a key is left down: {context}");
                    }
                }
            }
        }
    }
}
