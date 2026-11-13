//! The key-face editor's rules: how a label splits into the two lines the
//! editor shows and joins back, which pictures the keypad can draw, and that
//! the panel's Appearance tab saves through the device's config.

use std::path::Path;

use super::super::key_face::{KeyPanelTab, is_key_face_image, joined_label, label_lines};
use super::*;
use openlogi_core::binding::{GridSlot, KeypadPage};
use openlogi_core::config::KeyFaceStyle;

fn labelled(label: Option<&str>) -> KeyFaceStyle {
    KeyFaceStyle {
        label: label.map(str::to_owned),
        ..KeyFaceStyle::default()
    }
}

#[test]
fn a_label_splits_into_the_two_lines_the_key_draws() {
    assert_eq!(label_lines(&labelled(None)), [String::new(), String::new()]);
    assert_eq!(
        label_lines(&labelled(Some("Export"))),
        ["Export".to_owned(), String::new()]
    );
    assert_eq!(
        label_lines(&labelled(Some("Export\nPNG"))),
        ["Export".to_owned(), "PNG".to_owned()]
    );
    // The key draws two lines at most: a third joins the second.
    assert_eq!(
        label_lines(&labelled(Some("a\nb\nc"))),
        ["a".to_owned(), "b c".to_owned()]
    );
}

#[test]
fn two_lines_join_into_one_label_and_blanks_into_none() {
    assert_eq!(joined_label("", "  "), None);
    assert_eq!(joined_label(" Export ", ""), Some("Export".to_owned()));
    assert_eq!(joined_label("", "PNG"), Some("PNG".to_owned()));
    assert_eq!(
        joined_label("Export", "PNG"),
        Some("Export\nPNG".to_owned())
    );
    // What the editor splits, it joins back unchanged.
    let [first, second] = label_lines(&labelled(Some("Export\nPNG")));
    assert_eq!(
        joined_label(&first, &second),
        Some("Export\nPNG".to_owned())
    );
}

#[test]
fn only_png_and_jpeg_pictures_go_on_a_key() {
    for drawable in ["a.png", "b.PNG", "c.jpg", "d.JPEG", "dir.v2/e.jpeg"] {
        assert!(is_key_face_image(Path::new(drawable)), "{drawable}");
    }
    for other in ["a.gif", "b.svg", "c", "png", "d.png.txt"] {
        assert!(!is_key_face_image(Path::new(other)), "{other}");
    }
}

/// On the Appearance tab, the icon toggle saves the face of the grid key on
/// the page being edited — the page-2 key, not page 1's.
#[gpui::test]
fn the_appearance_tab_styles_the_key_on_the_edited_page(cx: &mut gpui::TestAppContext) {
    use gpui::{Modifiers, size};

    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let (mut state, _commands) = crate::state::tests::keypad_pages::keypad_state();
    let second = KeypadPage::new(2).unwrap();
    let _ = state.commit_keypad_pages(second);
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        let state = cx.new(|_| state);
        AppState::set_global(state, cx);
    });
    let (view, cx) = cx.add_window_view(|_, cx| FunctionRowView::new(cx));
    cx.simulate_resize(size(px(1400.), px(1000.)));
    view.update(cx, |view, cx| {
        view.set_keypad_page(second, cx);
        view.select_key(Some(4), cx);
        view.set_panel_tab(KeyPanelTab::Appearance, cx);
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let middle = GridSlot {
        page: second,
        key: GridKey::Middle,
    };
    view.read_with(cx, |view, _| {
        assert!(view.face.is_some(), "the label inputs edit the key");
    });

    let toggle = cx.debug_bounds("key-face-icon").expect("the icon toggle");
    cx.simulate_click(toggle.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));

    cx.update(|_, cx| {
        let state = AppState::try_read(cx).unwrap();
        assert!(
            !state.current_key_face(middle).icon,
            "the icon is turned off"
        );
        assert!(
            state
                .current_key_face(GridSlot {
                    page: KeypadPage::FIRST,
                    ..middle
                })
                .is_default(),
            "page 1's key keeps its own face"
        );
    });

    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

/// A key whose face already carries a Lucide icon opens its Symbol section
/// on the icon picker, the grid listing the catalog.
#[gpui::test]
fn a_key_with_a_lucide_symbol_opens_the_icon_picker(cx: &mut gpui::TestAppContext) {
    use gpui::size;
    use openlogi_core::config::KeySymbol;

    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let (mut state, _commands) = crate::state::tests::keypad_pages::keypad_state();
    let top_left = GridSlot {
        page: KeypadPage::FIRST,
        key: GridKey::TopLeft,
    };
    let _ = state.commit_key_face(
        top_left,
        KeyFaceStyle {
            symbol: Some(KeySymbol::Lucide("camera".into())),
            ..KeyFaceStyle::default()
        },
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        let state = cx.new(|_| state);
        AppState::set_global(state, cx);
    });
    let (view, cx) = cx.add_window_view(|_, cx| FunctionRowView::new(cx));
    cx.simulate_resize(size(px(1400.), px(1000.)));
    view.update(cx, |view, cx| {
        view.select_key(Some(0), cx);
        view.set_panel_tab(KeyPanelTab::Appearance, cx);
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));

    assert!(
        cx.debug_bounds("key-symbol-grid").is_some(),
        "the icon grid shows"
    );
    assert!(cx.debug_bounds("key-symbol-emoji-presets").is_none());

    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

/// A key with a gradient background opens its background section on the
/// gradient: the presets and both ends' choosers show.
#[gpui::test]
fn a_gradient_key_opens_the_gradient_chooser(cx: &mut gpui::TestAppContext) {
    use gpui::size;
    use openlogi_core::color::Rgb;
    use openlogi_core::config::{GradientDirection, KeyGradient};

    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let (mut state, _commands) = crate::state::tests::keypad_pages::keypad_state();
    let top_left = GridSlot {
        page: KeypadPage::FIRST,
        key: GridKey::TopLeft,
    };
    let _ = state.commit_key_face(
        top_left,
        KeyFaceStyle {
            gradient: Some(KeyGradient {
                from: Rgb::new(0xff, 0x5a, 0x00),
                to: Rgb::new(0x78, 0x00, 0xc8),
                direction: GradientDirection::Diagonal,
            }),
            ..KeyFaceStyle::default()
        },
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        let state = cx.new(|_| state);
        AppState::set_global(state, cx);
    });
    let (view, cx) = cx.add_window_view(|_, cx| FunctionRowView::new(cx));
    cx.simulate_resize(size(px(1400.), px(1000.)));
    view.update(cx, |view, cx| {
        view.select_key(Some(0), cx);
        view.set_panel_tab(KeyPanelTab::Appearance, cx);
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));

    assert!(cx.debug_bounds("key-face-gradient-presets").is_some());
    assert!(cx.debug_bounds("key-face-gradient-from-swatches").is_some());
    assert!(cx.debug_bounds("key-face-gradient-to-swatches").is_some());
    assert!(
        cx.debug_bounds("key-face-background-swatches").is_none(),
        "the solid chooser gives way to the gradient's"
    );
    assert!(cx.debug_bounds("key-face-icon-color-swatches").is_some());

    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

/// A live key opens its Live tab on the live-key editor, its fields made
/// for that key; leaving for another tab drops them.
#[gpui::test]
fn the_live_tab_edits_the_live_key(cx: &mut gpui::TestAppContext) {
    use gpui::size;
    use openlogi_core::config::{LiveKey, LiveSource};

    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let (mut state, _commands) = crate::state::tests::keypad_pages::keypad_state();
    let top_left = GridSlot {
        page: KeypadPage::FIRST,
        key: GridKey::TopLeft,
    };
    let _ = state.commit_live_key(
        top_left,
        Some(LiveKey::new(LiveSource::Command("date +%H:%M".into()))),
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        let state = cx.new(|_| state);
        AppState::set_global(state, cx);
    });
    let (view, cx) = cx.add_window_view(|_, cx| FunctionRowView::new(cx));
    cx.simulate_resize(size(px(1400.), px(1000.)));
    view.update(cx, |view, cx| {
        view.select_key(Some(0), cx);
        view.set_panel_tab(KeyPanelTab::Live, cx);
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));

    assert!(
        cx.debug_bounds("live-key").is_some(),
        "the live editor shows"
    );
    view.read_with(cx, |view, _| {
        assert!(view.live.is_some(), "the command fields edit the key");
    });

    view.update(cx, |view, cx| view.set_panel_tab(KeyPanelTab::Action, cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("live-key").is_none());
    view.read_with(cx, |view, _| assert!(view.live.is_none()));

    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

/// A command live key with its action fields, in the shortest window the
/// app allows and at the largest interface scale: the Live panel stays in
/// its column and scrolls, so the increment field can be reached.
#[gpui::test]
fn the_live_tab_scrolls_to_its_last_field_in_a_short_window(cx: &mut gpui::TestAppContext) {
    use gpui::{ScrollDelta, ScrollEvent, point, size};
    use openlogi_core::config::{LiveKey, LiveSource, UiScale};

    let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let (mut state, _commands) = crate::state::tests::keypad_pages::keypad_state();
    let top_left = GridSlot {
        page: KeypadPage::FIRST,
        key: GridKey::TopLeft,
    };
    let _ = state.commit_live_key(
        top_left,
        Some(LiveKey::new(LiveSource::Command("date +%H:%M".into()))),
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        let state = cx.new(|_| state);
        AppState::set_global(state, cx);
    });
    let (view, cx) = cx.add_window_view(|_, cx| FunctionRowView::new(cx));
    // The device page's share of the main window at its minimum size.
    let window_h = px(520.);
    cx.simulate_resize(size(px(1280.), window_h));
    cx.update(|window, _| crate::ui::theme::apply_scale(window, UiScale::ExtraLarge));
    view.update(cx, |view, cx| {
        view.select_key(Some(0), cx);
        view.set_panel_tab(KeyPanelTab::Live, cx);
    });
    let draw = |cx: &mut gpui::VisualTestContext| {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
    };
    draw(cx);

    let panel = cx
        .debug_bounds("live-key-panel")
        .expect("the Live panel shows");
    assert!(
        panel.bottom() <= window_h,
        "the panel ends inside the window: {panel:?}"
    );
    let viewport = cx
        .debug_bounds("scrollbar-overlay")
        .expect("the Live fields scroll");
    assert!(
        viewport.bottom() <= panel.bottom(),
        "the fields end inside the panel: {viewport:?} in {panel:?}"
    );

    for _ in 0..3 {
        cx.simulate_event(ScrollEvent {
            position: viewport.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-2000.))),
            ..Default::default()
        });
        draw(cx);
    }
    let increment = cx
        .debug_bounds("live-key-increment")
        .expect("the increment field is laid out");
    assert!(
        increment.top() >= viewport.top() && increment.bottom() <= viewport.bottom(),
        "scrolling reaches the increment field: {increment:?} in {viewport:?}"
    );

    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}
