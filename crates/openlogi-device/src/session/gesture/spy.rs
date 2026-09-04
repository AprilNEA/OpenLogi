//! `0x8110 MouseButtonSpy` capture support: the per-model map of which
//! buttons are safe to dispatch from the spy tap, and the pure edge decoder
//! that turns consecutive button-state masks into [`CapturedInput`].
//!
//! `0x8110` is a tap, not a divert (see [`crate::mouse_button_spy`]'s module
//! docs): starting it never stops a button's native HID report. Only buttons
//! with **no native report at all** can be dispatched from it without
//! double-firing — every entry in [`SPY_BUTTON_MAPS`] is one of those.
//! Standard buttons (left/right/middle/side/wheel-tilt) stay on the OS hook
//! exactly as they always have; this module never lists them.

use openlogi_core::binding::ButtonId;
use tokio::sync::mpsc;

use crate::mouse_button_spy::{MouseButtonMask, MouseButtonSpy, MouseButtonSpyEvent};

use super::CapturedInput;

/// One model's buttons captured through the `0x8110` spy tap.
#[derive(Debug, Clone, Copy)]
pub struct SpyButtonMap {
    /// `DeviceModelInfo::config_key()` — hex `ext` + primary model id.
    pub model_key: &'static str,
    /// `(1-based spy index, dispatched ButtonId)` pairs.
    pub buttons: &'static [(u8, ButtonId)],
}

/// Known G-series spy-owned button maps. One verified device today; add a
/// new [`SpyButtonMap`] entry per model as each is verified on hardware via
/// `openlogi diag mouse-buttons --watch`.
pub static SPY_BUTTON_MAPS: &[SpyButtonMap] = &[SpyButtonMap {
    // Logitech G502 X PLUS (ext=0x00, model_ids[0]=0x4099 -> config_key
    // "04099"). Verified on real hardware, cross-checked against Logitech's
    // official button-map diagram:
    //   1 left, 2 right, 3 middle, 4/6 side (back/forward, native — not
    //   listed here), 5 DPI Shift, 7/8 wheel tilt (native scroll, not
    //   listed), 9 Profile Cycling, 10/11 DPI up/down (order unconfirmed on
    //   hardware — see the plan's verification step 5).
    //
    // The firmware still acts on 5/9/10/11 itself (profile cycle / DPI
    // stage change) while in Onboard mode — confirmed on hardware (LED
    // flash on a press of 9). This is the button's stock behavior with or
    // without OpenLogi; the spy tap cannot suppress it (that needs Host
    // mode, deliberately out of scope).
    model_key: "04099",
    buttons: &[
        (5, ButtonId::DpiShift),
        (9, ButtonId::ProfileCycle),
        (10, ButtonId::DpiUp),
        (11, ButtonId::DpiDown),
    ],
}];

/// The spy-owned button map for `model_key`, if the device family is known.
#[must_use]
pub fn spy_buttons_for_model(model_key: &str) -> Option<&'static [(u8, ButtonId)]> {
    SPY_BUTTON_MAPS
        .iter()
        .find(|entry| entry.model_key == model_key)
        .map(|entry| entry.buttons)
}

/// The `0x8110` accessor, its captured buttons, and an already-registered
/// event receiver — present when a session armed the spy.
///
/// `events` is created by `listen()` *before* `start_reporting()` is ever
/// awaited (see the arming site in `gesture.rs`): the feature's emitter does
/// not buffer, so a receiver created only after the device starts sending
/// would silently miss any press that lands in the gap. Holding both alive
/// keeps delivery working — dropping the accessor would stop the listener,
/// dropping the receiver would stop this session from seeing its events.
pub(super) struct ArmedSpy {
    pub(super) spy: MouseButtonSpy,
    pub(super) buttons: &'static [(u8, ButtonId)],
    pub(super) events: async_channel::Receiver<MouseButtonSpyEvent>,
}

/// Emit the down/up edges between two consecutive spy masks.
///
/// The spy reports the whole button state on every change, so an XOR against
/// the previous mask is the entire diff — simpler than the CID-list
/// membership diffing the `0x1b04` path needs. Bits outside `map` are
/// ignored: they are the natively-handled buttons this device never hands to
/// the spy path.
pub(super) fn spy_edges(
    previous: MouseButtonMask,
    current: MouseButtonMask,
    map: &[(u8, ButtonId)],
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    if previous == current {
        return;
    }
    for &(index, button) in map {
        // `index` is 1-based; a malformed 0 entry (should never happen — see
        // `spy_button_maps_are_well_formed`) is skipped rather than
        // underflowing (debug: panic; release: wraps to 255, masked to a
        // shift of 15 — a silently wrong bit).
        let Some(bit) = index.checked_sub(1).map(|shift| 1u16 << shift) else {
            continue;
        };
        let was_down = previous.bits() & bit != 0;
        let down = current.bits() & bit != 0;
        if down && !was_down {
            let _ = sink.send(CapturedInput::ButtonDown(button));
        } else if !down && was_down {
            let _ = sink.send(CapturedInput::ButtonUp(button));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn recv_all(rx: &mut mpsc::UnboundedReceiver<CapturedInput>) -> Vec<CapturedInput> {
        let mut out = Vec::new();
        while let Ok(input) = rx.try_recv() {
            out.push(input);
        }
        out
    }

    #[test]
    fn single_button_down_then_up() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let map = [(5, ButtonId::DpiShift)];
        spy_edges(
            MouseButtonMask::default(),
            MouseButtonMask::from_bits(0x0010),
            &map,
            &tx,
        );
        assert_eq!(
            recv_all(&mut rx),
            vec![CapturedInput::ButtonDown(ButtonId::DpiShift)]
        );

        spy_edges(
            MouseButtonMask::from_bits(0x0010),
            MouseButtonMask::default(),
            &map,
            &tx,
        );
        assert_eq!(
            recv_all(&mut rx),
            vec![CapturedInput::ButtonUp(ButtonId::DpiShift)]
        );
    }

    #[test]
    fn simultaneous_edges_in_one_transition() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let map = [(9, ButtonId::ProfileCycle), (10, ButtonId::DpiUp)];

        spy_edges(
            MouseButtonMask::default(),
            MouseButtonMask::from_bits(0x0100),
            &map,
            &tx,
        );
        assert_eq!(
            recv_all(&mut rx),
            vec![CapturedInput::ButtonDown(ButtonId::ProfileCycle)]
        );

        // 9 releases and 10 presses in the same mask transition.
        spy_edges(
            MouseButtonMask::from_bits(0x0100),
            MouseButtonMask::from_bits(0x0200),
            &map,
            &tx,
        );
        assert_eq!(
            recv_all(&mut rx),
            vec![
                CapturedInput::ButtonUp(ButtonId::ProfileCycle),
                CapturedInput::ButtonDown(ButtonId::DpiUp),
            ]
        );
    }

    #[test]
    fn unmapped_bit_is_ignored() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let map = [(5, ButtonId::DpiShift)];
        // Bit 0 (button 1, left click) is not in the map — never emitted.
        spy_edges(
            MouseButtonMask::default(),
            MouseButtonMask::from_bits(0x0001),
            &map,
            &tx,
        );
        assert!(recv_all(&mut rx).is_empty());
    }

    #[test]
    fn identical_masks_emit_nothing() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let map = [(5, ButtonId::DpiShift)];
        let mask = MouseButtonMask::from_bits(0x0010);
        spy_edges(mask, mask, &map, &tx);
        assert!(recv_all(&mut rx).is_empty());
    }

    #[test]
    fn spy_button_maps_are_well_formed() {
        for model in SPY_BUTTON_MAPS {
            let mut seen_indices = HashSet::new();
            let mut seen_buttons = HashSet::new();
            for &(index, button) in model.buttons {
                assert!(
                    (1..=16).contains(&index),
                    "spy index out of HID++ button range in {}",
                    model.model_key
                );
                assert!(
                    seen_indices.insert(index),
                    "duplicate spy index in {}",
                    model.model_key
                );
                assert!(
                    seen_buttons.insert(button),
                    "duplicate ButtonId in {}",
                    model.model_key
                );
                assert!(
                    ButtonId::GAMING_BUTTONS.contains(&button),
                    "{button:?} bound by the spy must be in ButtonId::GAMING_BUTTONS"
                );
            }
        }
    }

    #[test]
    fn spy_buttons_for_model_looks_up_by_key() {
        assert!(spy_buttons_for_model("04099").is_some());
        assert!(spy_buttons_for_model("nonexistent-model-key").is_none());
    }
}
