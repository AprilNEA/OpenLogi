//! The key panel's custom shortcut: which mode it shows for a key's binding,
//! what switching the mode rebinds, what a typed chord binds, and that the
//! editor binds, shows, and clears a keypad key's chord.

use super::super::key_shortcut::{rebound_in_mode, shown_mode, typed_shortcut};
use super::*;
use crate::features::binding_editor::{ShortcutError, ShortcutMode};
use openlogi_core::binding::KeyCombo;

fn chord(text: &str) -> KeyCombo {
    text.parse().expect("a valid chord")
}

/// The editor opens on the mode the key fires its chord with — holding the
/// modifiers while tapping the key included — and on tapping otherwise.
#[test]
fn the_shown_mode_is_the_bindings_own() {
    assert_eq!(shown_mode(None), ShortcutMode::Tap);
    assert_eq!(shown_mode(Some(&Action::Copy)), ShortcutMode::Tap);
    assert_eq!(
        shown_mode(Some(&Action::CustomShortcut(chord("Ctrl+C")))),
        ShortcutMode::Tap
    );
    assert_eq!(
        shown_mode(Some(&Action::HoldShortcut(chord("Ctrl+Space")))),
        ShortcutMode::Hold
    );
    assert_eq!(
        shown_mode(Some(&Action::TapKeyHoldingModifiers(chord("Alt+Tab")))),
        ShortcutMode::HoldModifiers
    );
}

/// Switching the mode of a key that sends a chord rebinds that chord the new
/// way; a key without one, or the mode it already has, rebinds nothing.
#[test]
fn switching_the_mode_rebinds_only_a_bound_chord() {
    let tapped = Action::CustomShortcut(chord("Ctrl+Shift+P"));
    assert_eq!(
        rebound_in_mode(Some(&tapped), ShortcutMode::Hold),
        Some(Action::HoldShortcut(chord("Ctrl+Shift+P")))
    );
    assert_eq!(
        rebound_in_mode(Some(&tapped), ShortcutMode::HoldModifiers),
        Some(Action::TapKeyHoldingModifiers(chord("Ctrl+Shift+P")))
    );
    assert_eq!(rebound_in_mode(Some(&tapped), ShortcutMode::Tap), None);
    assert_eq!(
        rebound_in_mode(Some(&Action::Copy), ShortcutMode::Hold),
        None
    );
    assert_eq!(rebound_in_mode(None, ShortcutMode::Hold), None);
}

#[test]
fn a_typed_chord_binds_in_the_chosen_mode() {
    assert_eq!(
        typed_shortcut(" Ctrl+Shift+P ", ShortcutMode::Tap),
        Ok(Action::CustomShortcut(chord("Ctrl+Shift+P")))
    );
    assert_eq!(
        typed_shortcut("Alt+Tab", ShortcutMode::HoldModifiers),
        Ok(Action::TapKeyHoldingModifiers(chord("Alt+Tab")))
    );
    assert_eq!(
        typed_shortcut("not a chord", ShortcutMode::Hold),
        Err(ShortcutError::Unrecognized)
    );
}

/// On a keypad key, typing a chord and Add binds it in the chosen mode, the
/// panel shows it, and clearing it unbinds the key.
#[gpui::test]
fn the_key_panel_binds_shows_and_clears_a_chord(cx: &mut gpui::TestAppContext) {
    use gpui::{Modifiers, VisualTestContext, size};

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx.debug_bounds(selector).expect(selector);
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let (state, _commands) = crate::state::tests::keypad_pages::keypad_state();
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        let state = cx.new(|_| state);
        AppState::set_global(state, cx);
    });
    let (view, cx) = cx.add_window_view(|_, cx| FunctionRowView::new(cx));
    cx.simulate_resize(size(px(1400.), px(1000.)));
    view.update(cx, |view, cx| view.select_key(Some(0), cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let key = KeyTarget::grid(GridKey::TopLeft);
    assert!(cx.debug_bounds("key-shortcut-bound").is_none());

    click(cx, "key-shortcut-hold");
    click(cx, "key-shortcut-input");
    cx.simulate_input("Ctrl+Shift+P");
    click(cx, "key-shortcut-add");

    let bound = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| bound_action(AppState::try_read(cx).unwrap(), &key))
    };
    assert_eq!(bound(cx), Some(Action::HoldShortcut(chord("Ctrl+Shift+P"))));
    assert!(
        cx.debug_bounds("key-shortcut-bound").is_some(),
        "the chord shows"
    );

    // Tapping instead rebinds the same chord.
    click(cx, "key-shortcut-tap");
    assert_eq!(
        bound(cx),
        Some(Action::CustomShortcut(chord("Ctrl+Shift+P")))
    );

    click(cx, "key-shortcut-clear");
    assert_eq!(bound(cx), None);
    assert!(cx.debug_bounds("key-shortcut-bound").is_none());

    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}
