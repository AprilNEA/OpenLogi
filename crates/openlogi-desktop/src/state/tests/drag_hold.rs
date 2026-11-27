//! Drag-hold: the inspector's swipe choice for Back/Forward persists a
//! binding shape and reloads the agent; it needs measured raw-XY support to
//! turn on, and a stored one can always be turned off.
use openlogi_core::binding::HeldDrag;

use super::fn_lock::drain;
use super::*;

/// The known mouse with its Back/Forward raw-XY support measured as given,
/// or with no capabilities measured at all (`None`).
fn mouse_state(raw_xy: Option<bool>, config: Config) -> AppState {
    let mut inventory = direct_inventory([0xa3, 0x93, 0xca, 0xe0]);
    let record = &mut inventory.paired[0];
    match raw_xy {
        Some(supported) => {
            record
                .capabilities
                .as_mut()
                .expect("the fixture mouse has capabilities")
                .drag_hold = supported;
        }
        None => record.capabilities = None,
    }
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(config, &resolver, commands)
    });
    drain(&mut receiver);
    state
}

#[test]
fn drag_hold_capability_boundary() {
    for (raw_xy, turns_on) in [(None, false), (Some(false), false), (Some(true), true)] {
        let mut state = mouse_state(raw_xy, Config::ephemeral());

        let _ = state.commit_drag_hold(ButtonId::Back, Some(HeldDrag::ThreeFingerSwipe));

        assert_eq!(
            state.drag_hold_buttons().get(&ButtonId::Back).copied(),
            turns_on.then_some(HeldDrag::ThreeFingerSwipe),
            "{raw_xy:?}"
        );
    }
}

#[test]
fn a_carried_over_drag_hold_turns_off_without_raw_xy() {
    for raw_xy in [None, Some(false)] {
        let mut config = Config::ephemeral();
        config.set_drag_hold(
            KNOWN_MOUSE_KEY,
            ButtonId::Back,
            Some(HeldDrag::TwoFingerSwipe),
        );
        let mut state = mouse_state(raw_xy, config);
        assert!(state.drag_hold_buttons().contains_key(&ButtonId::Back));

        let _ = state.commit_drag_hold(ButtonId::Back, None);

        assert!(state.drag_hold_buttons().is_empty(), "{raw_xy:?}");
        assert_eq!(
            state.config.drag_hold(KNOWN_MOUSE_KEY, ButtonId::Back),
            None
        );
    }
}
