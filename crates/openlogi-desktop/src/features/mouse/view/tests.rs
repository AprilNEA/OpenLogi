use gpui::{Modifiers, TestAppContext, VisualTestContext, size};
use openlogi_core::binding::default_binding;
use openlogi_core::config::Config;

use super::*;
use crate::services::assets::AssetResolver;
use crate::services::i18n::LOCALE_LOCK;
use crate::state::Sources;

fn install_app_state(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let resolver = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state =
            cx.new(|_| AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands)));
        AppState::set_global(state, cx);
    });
}

fn click_picker_control(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector).expect(selector);
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn assert_forward_binding(cx: &mut VisualTestContext, expected: &Action) {
    cx.update(|_, cx| {
        assert_eq!(
            AppState::try_read(cx)
                .unwrap()
                .button_bindings()
                .get(&ButtonId::Forward),
            Some(expected)
        );
    });
}

#[gpui::test]
fn long_bindings_stay_inside_their_label_card(cx: &mut TestAppContext) {
    // #1401: the card's Button base centres its children on the cross axis,
    // so without `items_stretch` the value row keeps its natural width and
    // overflows both edges once the binding name is wider than the card. The
    // English defaults are enough to trip it under the test text system
    // ("Forward (Button 5)" measures a 206px row over a 156px card). Pinned
    // to English under the lock: another test in this binary leaves the
    // process locale at zh-CN, whose labels are short enough to fit and
    // would have made this a false pass.
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    cx.update(gpui_component::init);
    install_app_state(cx);
    let (view, cx) = cx.add_window_view(MouseModelView::new);
    // Wide enough for labels on both sides (`model_layout` hides them under 960).
    cx.simulate_resize(size(px(1200.), px(800.)));
    cx.update(|window, cx| window.draw(cx).clear(cx));

    // Selectors are `label-card-{MouseControlId:?}` / `label-value-row-{MouseControlId:?}`.
    for (card_selector, row_selector) in [
        (
            "label-card-Button(Forward)",
            "label-value-row-Button(Forward)",
        ),
        (
            "label-card-Button(MiddleClick)",
            "label-value-row-Button(MiddleClick)",
        ),
    ] {
        let card = cx
            .debug_bounds(card_selector)
            .expect("the synthetic model renders a label card for this control");
        let row = cx
            .debug_bounds(row_selector)
            .expect("the label card renders its value row");
        assert!(
            card.contains(&row.origin) && card.contains(&row.bottom_right()),
            "{row_selector}: value row {row:?} must sit inside its card {card:?}"
        );
    }

    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

#[gpui::test]
fn action_picker_renders_after_opening_through_the_inspector(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    cx.update(gpui_component::init);
    install_app_state(cx);
    let (view, cx) = cx.add_window_view(MouseModelView::new);
    cx.simulate_resize(size(px(1200.), px(1000.)));
    cx.update(|window, cx| window.draw(cx).clear(cx));

    click_picker_control(cx, "label-card-Button(Forward)");
    click_picker_control(cx, "inspector-current-action");
    view.read_with(cx, |view, _| assert!(view.action_picker_open));
    assert!(cx.debug_bounds("inspector-action-Copy").is_some());

    click_picker_control(cx, "inspector-action-custom-shortcut-add");
    view.read_with(cx, |view, _| {
        assert!(view.custom_shortcut_invalid);
        assert!(!view.custom_application_invalid);
    });
    assert!(
        cx.debug_bounds("inspector-action-custom-shortcut-error")
            .is_some()
    );

    click_picker_control(cx, "inspector-action-custom-application-add");
    view.read_with(cx, |view, _| {
        assert!(view.action_picker_open);
        assert!(view.custom_shortcut_invalid);
        assert!(view.custom_application_invalid);
    });
    assert!(
        cx.debug_bounds("inspector-action-custom-application-error")
            .is_some()
    );
    assert_forward_binding(cx, &default_binding(ButtonId::Forward));

    click_picker_control(cx, "inspector-action-custom-shortcut-input");
    cx.simulate_input("Cmd+K");
    cx.update(|window, cx| window.draw(cx).clear(cx));
    view.read_with(cx, |view, _| {
        assert!(!view.custom_shortcut_invalid);
        assert!(view.custom_application_invalid);
    });
    assert!(
        cx.debug_bounds("inspector-action-custom-shortcut-error")
            .is_none()
    );
    assert!(
        cx.debug_bounds("inspector-action-custom-application-error")
            .is_some()
    );
    click_picker_control(cx, "inspector-action-custom-shortcut-add");
    view.read_with(cx, |view, _| assert!(!view.action_picker_open));
    assert_forward_binding(cx, &Action::CustomShortcut("Cmd+K".parse().unwrap()));

    click_picker_control(cx, "inspector-current-action");
    view.read_with(cx, |view, cx| {
        assert_eq!(view.custom_shortcut_input.read(cx).value(), "");
        assert_eq!(view.custom_application_input.read(cx).value(), "");
        assert!(!view.custom_application_invalid);
    });
    assert!(
        cx.debug_bounds("inspector-action-custom-application-error")
            .is_none()
    );
    click_picker_control(cx, "inspector-action-custom-application-input");
    cx.simulate_input("/Applications/Calculator.app");
    click_picker_control(cx, "inspector-action-custom-application-add");
    view.read_with(cx, |view, _| assert!(!view.action_picker_open));
    cx.update(|_, cx| {
        let Action::OpenApplication(target) =
            &AppState::try_read(cx).unwrap().button_bindings()[&ButtonId::Forward]
        else {
            panic!("the application editor must commit an application action");
        };
        assert_eq!(target.path(), "/Applications/Calculator.app");
    });

    click_picker_control(cx, "inspector-current-action");
    cx.update(|window, cx| {
        view.read(cx)
            .action_search
            .focus_handle(cx)
            .focus(window, cx);
    });
    cx.simulate_input("Copy");
    cx.update(|window, cx| window.draw(cx).clear(cx));
    click_picker_control(cx, "inspector-action-Copy");
    view.read_with(cx, |view, _| assert!(!view.action_picker_open));
    assert_forward_binding(cx, &Action::Copy);

    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

#[gpui::test]
fn a_selected_gesture_can_render_in_the_binding_inspector(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    install_app_state(cx);
    let (view, cx) = cx.add_window_view(MouseModelView::new);
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        view.set_gesture_selected_dir(Some(GestureDirection::Up));
        let gesture_maps = BTreeMap::from([(
            ButtonId::MiddleClick,
            BTreeMap::from([(
                GestureDirection::Click,
                default_binding(ButtonId::MiddleClick),
            )]),
        )]);
        let bindings = BTreeMap::new();
        let entity = cx.entity();

        binding_inspector(
            BindingInspectorData {
                selected: Some(MouseControlId::Button(ButtonId::MiddleClick)),
                gesture_direction: Some(GestureDirection::Up),
                bindings: &bindings,
                gesture_maps: &gesture_maps,
                dpi_gestures: false,
                editing_app: None,
                overridden: None,
            },
            ActionPickerContext {
                open: true,
                search: &view.action_search,
                shortcut_input: &view.custom_shortcut_input,
                application_input: &view.custom_application_input,
                shortcut_invalid: view.custom_shortcut_invalid,
                application_invalid: view.custom_application_invalid,
                view: &entity,
            },
            cx,
        );
    });
    cx.run_until_parked();
    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

#[gpui::test]
fn selecting_another_control_closes_the_action_picker(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    install_app_state(cx);
    let (view, cx) = cx.add_window_view(MouseModelView::new);
    cx.run_until_parked();

    view.update(cx, |view, _| {
        view.selected = Some(MouseControlId::Button(ButtonId::Back));
        view.action_picker_open = true;

        view.select(MouseControlId::Button(ButtonId::Forward));

        assert!(!view.action_picker_open);
    });
    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

#[gpui::test]
fn clearing_custom_action_drafts_resets_text_and_invalid_state(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    install_app_state(cx);
    let (view, cx) = cx.add_window_view(MouseModelView::new);
    cx.run_until_parked();

    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.custom_shortcut_input
                .update(cx, |input, cx| input.set_value("Cmd+K", window, cx));
            view.custom_application_input
                .update(cx, |input, cx| input.set_value("/bin/true", window, cx));
            view.custom_shortcut_invalid = true;
            view.custom_application_invalid = true;

            view.clear_custom_action_drafts(window, cx);
        });
    });

    view.update(cx, |view, cx| {
        assert_eq!(view.custom_shortcut_input.read(cx).value(), "");
        assert_eq!(view.custom_application_input.read(cx).value(), "");
        assert!(!view.custom_shortcut_invalid);
        assert!(!view.custom_application_invalid);
    });
    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

#[test]
fn active_thumbwheel_directions_highlight_the_paired_control() {
    assert_eq!(
        MouseControlId::from_active_button(ButtonId::ThumbwheelScrollUp),
        MouseControlId::ThumbwheelRotation
    );
    assert_eq!(
        MouseControlId::from_active_button(ButtonId::ThumbwheelScrollDown),
        MouseControlId::ThumbwheelRotation
    );
}

#[test]
fn fallback_model_only_adds_thumbwheel_when_capability_is_measured() {
    let (_, _, without, _) = scaled_model(None, 560., 420., false, LabelDistribution::LeftOnly);
    let (_, _, with, _) = scaled_model(None, 560., 420., true, LabelDistribution::LeftOnly);
    assert_eq!(
        without
            .iter()
            .filter(|hotspot| hotspot.id == MouseControlId::ThumbwheelRotation)
            .count(),
        0
    );
    assert_eq!(
        with.iter()
            .filter(|hotspot| hotspot.id == MouseControlId::ThumbwheelRotation)
            .count(),
        1
    );
}
