use gpui::{TestAppContext, size};
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
                action_picker_open: false,
                bindings: &bindings,
                gesture_maps: &gesture_maps,
                dpi_gestures: false,
                editing_app: None,
                overridden: None,
            },
            &view.action_search,
            &entity,
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

/// A depot render whose authored markers cover both hook-visible and
/// HID++-only controls. A G-series mouse resolves a real depot, so the
/// asset path — not just the synthetic fallback — has to be filtered.
fn asset_with_slots(slot_names: &[&str]) -> ResolvedAsset {
    ResolvedAsset {
        depot: "g502_wireless".to_string(),
        display_name: "G502".to_string(),
        kind: Some(openlogi_core::device::DeviceKind::Mouse),
        image_path: std::path::PathBuf::from("/tmp/g502.png"),
        hero_image_path: None,
        glow: None,
        metadata: openlogi_assets::metadata::Metadata {
            images: vec![openlogi_assets::metadata::ImageEntry {
                key: "device_buttons_image".to_string(),
                origin: openlogi_assets::metadata::Origin {
                    width: 420,
                    height: 560,
                },
                assignments: slot_names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| openlogi_assets::metadata::Assignment {
                        slot_name: (*name).to_string(),
                        slot_id: format!("mx-master-6b012_c{i}"),
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "fixture indices are single digits"
                        )]
                        marker: openlogi_assets::metadata::Point {
                            x: 50.,
                            y: 10. * i as f32,
                        },
                        label: openlogi_assets::metadata::Direction { x: -1, y: -1 },
                    })
                    .collect(),
            }],
        },
        png_width: 420,
        png_height: 560,
    }
}

#[test]
fn asset_model_without_diversion_drops_hidpp_controls() {
    let asset = asset_with_slots(&[
        "SLOT_NAME_MIDDLE_BUTTON",
        "SLOT_NAME_BACK_BUTTON",
        "SLOT_NAME_FORWARD_BUTTON",
        "SLOT_NAME_MODESHIFT_BUTTON",
        "SLOT_NAME_GESTURE_BUTTON",
    ]);
    let controls = ModelControls {
        thumbwheel: false,
        can_divert: false,
    };
    let (_, _, hotspots, _) = scaled_model(
        Some(&asset),
        560.,
        420.,
        controls,
        LabelDistribution::LeftOnly,
    );
    assert_eq!(
        hotspots
            .iter()
            .map(|hotspot| hotspot.id)
            .collect::<Vec<_>>(),
        vec![
            MouseControlId::Button(ButtonId::MiddleClick),
            MouseControlId::Button(ButtonId::Back),
            MouseControlId::Button(ButtonId::Forward),
        ],
        "the depot's DPI and gesture markers can never fire without 0x1b04"
    );
}

/// Every control the model draws must be one this device can actually be
/// bound on, and every label must point at a control that is still drawn —
/// the synthetic silhouette builds its labels independently of its
/// hotspots, so a filtered model that forgets its labels leaves leader
/// lines aimed at nothing.
#[test]
fn fallback_model_without_diversion_draws_only_os_hook_controls() {
    let controls = ModelControls {
        thumbwheel: true,
        can_divert: false,
    };
    let (_, _, hotspots, labels) =
        scaled_model(None, 560., 420., controls, LabelDistribution::LeftOnly);
    assert_eq!(
        hotspots
            .iter()
            .map(|hotspot| hotspot.id)
            .collect::<Vec<_>>(),
        vec![
            MouseControlId::Button(ButtonId::MiddleClick),
            MouseControlId::Button(ButtonId::Back),
            MouseControlId::Button(ButtonId::Forward),
        ]
    );
    for label in &labels {
        assert!(
            hotspots.iter().any(|hotspot| hotspot.id == label.id),
            "label {:?} survived its hotspot",
            label.id
        );
    }
}

#[test]
fn fallback_model_only_adds_thumbwheel_when_capability_is_measured() {
    let divertable = |thumbwheel| ModelControls {
        thumbwheel,
        can_divert: true,
    };
    let (_, _, without, _) = scaled_model(
        None,
        560.,
        420.,
        divertable(false),
        LabelDistribution::LeftOnly,
    );
    let (_, _, with, _) = scaled_model(
        None,
        560.,
        420.,
        divertable(true),
        LabelDistribution::LeftOnly,
    );
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
