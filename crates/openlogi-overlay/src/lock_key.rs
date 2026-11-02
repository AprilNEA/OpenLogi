//! Compact transient lock-key indicator: "Caps Lock On" and the like, shown
//! when a keyboard reports one of its lock keys turning on or off.
//!
//! The agent decides what shows and for how long
//! ([`openlogi_ipc::LockKeyObservation`]); this module only draws it. The window
//! opens with the first change, shows each later change in place, and closes
//! when the agent reports nothing showing.

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, FontWeight, Hsla, IntoElement,
    ParentElement, Render, Size, Styled, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, div, px, svg,
};
use openlogi_core::device::LockKey;
use openlogi_ipc::LockKeyChange;
use openlogi_ui::action_icons::LOCK_KEY_ICON;
use tracing::warn;

/// Wide enough for the longest translated title on one line.
const WIDTH: f32 = 360.0;
const HEIGHT: f32 = 72.0;

/// The glyph of a key that is now on: the backlight indicator's accent.
const ENGAGED: Hsla = Hsla {
    h: 0.58,
    s: 0.75,
    l: 0.62,
    a: 1.0,
};
/// The glyph of a key that is now off.
const RELEASED: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.60,
    a: 1.0,
};

/// The catalog key for the text that announces `change`.
pub(crate) const fn title_key(change: LockKeyChange) -> &'static str {
    match (change.key, change.engaged) {
        (LockKey::CapsLock, true) => "keyboard.caps_lock_on",
        (LockKey::CapsLock, false) => "keyboard.caps_lock_off",
        (LockKey::NumLock, true) => "keyboard.num_lock_on",
        (LockKey::NumLock, false) => "keyboard.num_lock_off",
        (LockKey::ScrollLock, true) => "keyboard.scroll_lock_on",
        (LockKey::ScrollLock, false) => "keyboard.scroll_lock_off",
    }
}

/// The colour of the glyph for a key that is now `engaged` or not.
const fn glyph_colour(engaged: bool) -> Hsla {
    if engaged { ENGAGED } else { RELEASED }
}

/// The indicator window's view: the latest change the agent is showing.
pub(crate) struct LockKeyView {
    change: LockKeyChange,
}

impl Render for LockKeyView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(12.0))
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .px(px(20.0))
            .rounded(px(12.0))
            .bg(gpui::hsla(0.0, 0.0, 0.08, 0.94))
            .text_color(gpui::hsla(0.0, 0.0, 0.96, 1.0))
            .child(
                svg()
                    .path(LOCK_KEY_ICON)
                    .size(px(24.0))
                    .flex_none()
                    .text_color(glyph_colour(self.change.engaged)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(rust_i18n::t!(title_key(self.change)).into_owned()),
            )
    }
}

/// Show `shown`, or close the indicator when it is `None`.
///
/// A change while the window is open replaces its text in place, so presses
/// in quick succession do not flash the window closed and open again.
pub(crate) fn present(cx: &mut App, shown: Option<LockKeyChange>) {
    let open: Vec<WindowHandle<LockKeyView>> = cx
        .windows()
        .iter()
        .filter_map(AnyWindowHandle::downcast::<LockKeyView>)
        .collect();
    let Some(change) = shown else {
        for view in open {
            let _ = view.update(cx, |_, window, _| window.remove_window());
        }
        return;
    };
    if open.is_empty() {
        let options = window_options(cx);
        if let Err(error) = cx.open_window(options, |_, cx| cx.new(|_| LockKeyView { change })) {
            warn!(%error, "could not open the lock-key indicator window");
        }
        return;
    }
    for view in open {
        let _ = view.update(cx, |view, _, cx| {
            view.change = change;
            cx.notify();
        });
    }
}

fn window_options(cx: &mut App) -> WindowOptions {
    let bounds = Bounds::centered(None, Size::new(px(WIDTH), px(HEIGHT)), cx);
    WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("openlogi-lock-key-osd".to_string()),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn change(key: LockKey, engaged: bool) -> LockKeyChange {
        LockKeyChange { key, engaged }
    }

    #[test]
    fn each_change_announces_its_own_key_and_state() {
        let expected = [
            (change(LockKey::CapsLock, true), "keyboard.caps_lock_on"),
            (change(LockKey::CapsLock, false), "keyboard.caps_lock_off"),
            (change(LockKey::NumLock, true), "keyboard.num_lock_on"),
            (change(LockKey::NumLock, false), "keyboard.num_lock_off"),
            (change(LockKey::ScrollLock, true), "keyboard.scroll_lock_on"),
            (
                change(LockKey::ScrollLock, false),
                "keyboard.scroll_lock_off",
            ),
        ];
        // Exhaustive on purpose: a new lock key stops this compiling until it
        // is listed above.
        let _: fn(LockKey) = |key| match key {
            LockKey::CapsLock | LockKey::NumLock | LockKey::ScrollLock => {}
        };
        for (change, key) in expected {
            assert_eq!(title_key(change), key, "{change:?}");
        }
    }

    /// A key missing from the catalog would show the raw key on screen. The
    /// other locales are held to the same keys by `catalog_parity`.
    #[test]
    fn every_announcement_is_in_the_catalog() {
        for key in [LockKey::CapsLock, LockKey::NumLock, LockKey::ScrollLock] {
            for engaged in [true, false] {
                let title = title_key(change(key, engaged));
                assert_ne!(rust_i18n::t!(title, locale = "en"), title);
            }
        }
    }

    #[test]
    fn a_key_turning_on_is_drawn_apart_from_one_turning_off() {
        assert_eq!(glyph_colour(true), ENGAGED);
        assert_eq!(glyph_colour(false), RELEASED);
        assert_ne!(ENGAGED, RELEASED);
    }

    /// The change each open indicator window shows.
    fn shown(cx: &mut gpui::TestAppContext) -> Vec<LockKeyChange> {
        cx.update(|cx| {
            cx.windows()
                .iter()
                .filter_map(AnyWindowHandle::downcast::<LockKeyView>)
                .filter_map(|window| window.read(cx).ok().map(|view| view.change))
                .collect()
        })
    }

    #[gpui::test]
    fn one_window_shows_each_change_in_place_until_nothing_shows(cx: &mut gpui::TestAppContext) {
        let on = change(LockKey::CapsLock, true);
        let off = change(LockKey::CapsLock, false);

        cx.update(|cx| present(cx, None));
        assert!(shown(cx).is_empty(), "nothing showing opens nothing");

        cx.update(|cx| present(cx, Some(on)));
        assert_eq!(shown(cx), [on]);

        cx.update(|cx| present(cx, Some(off)));
        cx.run_until_parked();
        assert_eq!(shown(cx), [off], "the open window shows the newer change");

        cx.update(|cx| present(cx, None));
        cx.run_until_parked();
        assert!(shown(cx).is_empty(), "nothing showing closes the window");
    }
}
