//! Raw-touchpad gesture bindings for devices that expose HID++ `0x6100`.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    Anchor, App, InteractiveElement, IntoElement, ParentElement, Styled, WeakEntity, div,
    prelude::FluentBuilder as _, px, svg,
};
use gpui_component::{
    Icon, Selectable as _, h_flex,
    popover::{Popover, PopoverState},
    scroll::ScrollableElement as _,
    v_flex,
};
use openlogi_core::binding::{Action, ButtonId, default_binding};

use crate::features::mouse::picker::{
    PickFn, action_icon_path, action_rows, compact_panel, divider, title,
};
use crate::state::AppState;
use crate::ui::action::localized_action_label;
use crate::ui::components::{PanelCard, Toggle, control_button};
use crate::ui::theme::{self, Palette, Typography as _};

/// Complete touchpad configuration surface. The caller capability-gates the
/// tab; this function only projects and commits configuration through
/// [`AppState`].
pub(crate) fn gesture_panel(cx: &mut App) -> impl IntoElement {
    let pal = theme::palette(cx);
    let (enabled, bindings) = AppState::try_read(cx).map_or_else(
        || (false, BTreeMap::new()),
        |state| {
            (
                state.touchpad_gestures_enabled(),
                state.touchpad_bindings().clone(),
            )
        },
    );

    v_flex()
        .w_full()
        .gap_4()
        .child(management_card(enabled, pal))
        .child(gesture_group(
            tr!("2-finger gestures"),
            &ButtonId::TOUCHPAD_TWO_FINGER,
            &bindings,
            pal,
        ))
        .child(gesture_group(
            tr!("3-finger gestures"),
            &ButtonId::TOUCHPAD_THREE_FINGER,
            &bindings,
            pal,
        ))
        .child(gesture_group(
            tr!("4-finger gestures"),
            &ButtonId::TOUCHPAD_FOUR_FINGER,
            &bindings,
            pal,
        ))
}

fn management_card(enabled: bool, pal: Palette) -> PanelCard {
    PanelCard::new(
        tr!("Manage gestures"),
        Icon::empty().path("action-icons/move.svg"),
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .text_body()
                            .text_color(pal.text_primary)
                            .child(tr!("Use OpenLogi actions for multi-finger gestures.")),
                    )
                    .child(
                        Toggle::new("touchpad-gestures-enabled")
                            .selected(enabled)
                            .label(if enabled { tr!("On") } else { tr!("Off") })
                            .on_change(|enabled, _window, cx| {
                                AppState::update_bindings(cx, |state| {
                                    state.commit_touchpad_gestures_enabled(*enabled);
                                });
                            }),
                    ),
            )
            .child(div().text_caption().text_color(pal.text_muted).child(tr!(
                "Pointer movement, clicks, two-finger scrolling, and secondary click remain native."
            ))),
    )
}

fn gesture_group(
    title: gpui::SharedString,
    triggers: &[ButtonId],
    bindings: &BTreeMap<ButtonId, Action>,
    pal: Palette,
) -> PanelCard {
    PanelCard::new(
        title,
        Icon::empty().path("action-icons/move.svg"),
        v_flex().children(
            triggers
                .iter()
                .copied()
                .enumerate()
                .map(|(index, trigger)| {
                    let action = bindings
                        .get(&trigger)
                        .cloned()
                        .unwrap_or_else(|| default_binding(trigger));
                    gesture_row(trigger, &action, index > 0, pal)
                }),
        ),
    )
}

fn gesture_row(trigger: ButtonId, action: &Action, divided: bool, pal: Palette) -> gpui::Div {
    v_flex()
        .when(divided, |row| {
            row.child(div().h(px(1.)).w_full().bg(pal.border))
        })
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_4()
                .py_2()
                .child(
                    div()
                        .text_body()
                        .text_color(pal.text_primary)
                        .child(gesture_label(trigger)),
                )
                .child(action_picker(trigger, action, pal)),
        )
}

fn action_picker(trigger: ButtonId, action: &Action, pal: Palette) -> Popover {
    let identity = format!("touchpad-action-{trigger:?}");
    let list_identity = format!("touchpad-action-list-{trigger:?}");
    let action_label = localized_action_label(action);
    let action_icon = action_icon_path(action);
    let picker_action = action.clone();
    Popover::new(identity.clone())
        .anchor(Anchor::TopRight)
        .appearance(false)
        .trigger(
            control_button(format!("{identity}-trigger"))
                .outline()
                .child(
                    h_flex()
                        .min_w(px(180.))
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            h_flex()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .child(
                                    svg()
                                        .path(action_icon)
                                        .size_4()
                                        .flex_none()
                                        .text_color(pal.text_muted),
                                )
                                .child(div().truncate().child(action_label)),
                        )
                        .child(
                            svg()
                                .path("action-icons/chevrons-down.svg")
                                .size_3()
                                .flex_none()
                                .text_color(pal.text_muted),
                        ),
                ),
        )
        .content(move |_state, _window, cx| {
            action_picker_content(
                trigger,
                &picker_action,
                list_identity.clone(),
                cx.entity().downgrade(),
                pal,
            )
        })
}

fn action_picker_content(
    trigger: ButtonId,
    current: &Action,
    list_identity: String,
    popover: WeakEntity<PopoverState>,
    pal: Palette,
) -> gpui::Div {
    let on_pick: PickFn = Rc::new(move |action, window, cx| {
        AppState::update_bindings(cx, |state| {
            state.commit_touchpad_binding(trigger, action);
        });
        if let Some(popover) = popover.upgrade() {
            popover.update(cx, |state, cx| state.dismiss(window, cx));
        }
    });
    let rows = action_rows("touchpad-action-row", Some(current), &on_pick, pal);
    compact_panel(pal)
        .w(px(320.))
        .max_h(px(440.))
        .child(title(tr!("Choose an action"), pal))
        .child(divider(pal))
        .child(
            v_flex()
                .id(list_identity)
                .max_h(px(380.))
                .overflow_y_scrollbar()
                .children(rows),
        )
}

fn gesture_label(trigger: ButtonId) -> gpui::SharedString {
    match trigger {
        ButtonId::TouchpadTwoFingerTap
        | ButtonId::TouchpadThreeFingerTap
        | ButtonId::TouchpadFourFingerTap => tr!("Tap"),
        ButtonId::TouchpadTwoFingerPinchIn | ButtonId::TouchpadFourFingerPinchIn => {
            tr!("Pinch In")
        }
        ButtonId::TouchpadTwoFingerPinchOut | ButtonId::TouchpadFourFingerPinchOut => {
            tr!("Pinch Out")
        }
        ButtonId::TouchpadThreeFingerSwipeUp | ButtonId::TouchpadFourFingerSwipeUp => {
            tr!("Swipe Up")
        }
        ButtonId::TouchpadThreeFingerSwipeDown | ButtonId::TouchpadFourFingerSwipeDown => {
            tr!("Swipe Down")
        }
        ButtonId::TouchpadThreeFingerSwipeLeft | ButtonId::TouchpadFourFingerSwipeLeft => {
            tr!("Swipe Left")
        }
        ButtonId::TouchpadThreeFingerSwipeRight | ButtonId::TouchpadFourFingerSwipeRight => {
            tr!("Swipe Right")
        }
        _ => unreachable!("touchpad panel only renders touchpad gesture triggers"),
    }
}
