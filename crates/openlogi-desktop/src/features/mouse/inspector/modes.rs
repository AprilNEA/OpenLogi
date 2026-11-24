//! The inspector's Mode list: every way a button can behave, one row each.
//!
//! A button's binding has one shape at a time (a single action, gestures,
//! holding to scroll sideways, or hold and drag), so the inspector offers
//! them as one choice. Picking a row converts the binding and keeps its
//! click action. The modes live in the global profile only, as the bindings
//! they write do.

use gpui::{
    Entity, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, rgb,
};
use gpui_kit::component::{Icon, IconName, Selectable as _, h_flex, v_flex};
use openlogi_core::binding::{ButtonId, GestureDirection, HeldDrag};

use super::{BindingInspectorData, can_enable_gestures, offers_drag_hold};
use crate::features::binding_editor::{GESTURE_BUTTON_ICON, editor_section};
use crate::features::mouse::view::{
    MouseModelView, drag_hold_fingers, drag_hold_icon, drag_hold_text,
};
use crate::state::{AppState, StateEvents};
use crate::ui::components::MenuRow;
use crate::ui::theme::{ACCENT_BLUE, Palette, Typography as _};

const SINGLE_ACTION_ICON: &str = "action-icons/mouse-pointer-click.svg";
const SCROLL_SIDEWAYS_ICON: &str = "action-icons/arrow-left-right.svg";

/// How a button behaves, one per binding shape the inspector edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ButtonMode {
    Single,
    Gestures,
    ScrollSideways,
    HoldAndDrag,
}

impl ButtonMode {
    const ALL: [Self; 4] = [
        Self::Single,
        Self::Gestures,
        Self::ScrollSideways,
        Self::HoldAndDrag,
    ];

    /// The mode `button` is in, read from the global profile's maps.
    pub(super) fn of(button: ButtonId, data: &BindingInspectorData<'_>) -> Self {
        if data.gesture_maps.contains_key(&button) {
            Self::Gestures
        } else if data.drag_hold.contains_key(&button) {
            Self::HoldAndDrag
        } else if data.wheel_hold.contains(&button) {
            Self::ScrollSideways
        } else {
            Self::Single
        }
    }

    fn title(self) -> gpui::SharedString {
        match self {
            Self::Single => tr!("actions.mode_single_action"),
            Self::Gestures => tr!("actions.gestures"),
            Self::ScrollSideways => tr!("actions.hold_to_scroll_sideways"),
            Self::HoldAndDrag => tr!("actions.hold_and_drag"),
        }
    }

    fn hint(self) -> gpui::SharedString {
        match self {
            Self::Single => tr!("actions.mode_single_action_hint"),
            Self::Gestures => tr!("actions.mode_gestures_hint"),
            Self::ScrollSideways => tr!("actions.mode_scroll_sideways_hint"),
            Self::HoldAndDrag => tr!("actions.mode_hold_and_drag_hint"),
        }
    }

    fn icon(self, button: ButtonId, data: &BindingInspectorData<'_>) -> &'static str {
        match self {
            Self::Single => SINGLE_ACTION_ICON,
            Self::Gestures => GESTURE_BUTTON_ICON,
            Self::ScrollSideways => SCROLL_SIDEWAYS_ICON,
            Self::HoldAndDrag => drag_hold_icon(
                data.drag_hold
                    .get(&button)
                    .copied()
                    .unwrap_or_else(|| first_drag(button)),
            ),
        }
    }

    /// The debug selector of this mode's row.
    const fn row_id(self) -> &'static str {
        match self {
            Self::Single => "inspector-mode-single",
            Self::Gestures => "inspector-mode-gestures",
            Self::ScrollSideways => "inspector-mode-scroll-sideways",
            Self::HoldAndDrag => "inspector-mode-hold-and-drag",
        }
    }

    /// Whether this device can put `button` in this mode.
    fn runs_on(self, button: ButtonId, data: &BindingInspectorData<'_>) -> bool {
        match self {
            Self::Single => true,
            Self::Gestures => {
                can_enable_gestures(button, None)
                    && (button != ButtonId::DpiToggle || data.dpi_gestures)
            }
            Self::ScrollSideways => button.supports_wheel_hold() && data.virtual_thumbwheel,
            Self::HoldAndDrag => offers_drag_hold(button, data),
        }
    }
}

/// The modes `button` offers, in list order. A mode the device cannot run
/// stays listed while the button is in it, so a binding carried over from
/// another mouse can still be left.
pub(super) fn offered_modes(button: ButtonId, data: &BindingInspectorData<'_>) -> Vec<ButtonMode> {
    let current = ButtonMode::of(button, data);
    ButtonMode::ALL
        .into_iter()
        .filter(|mode| *mode == current || mode.runs_on(button, data))
        .collect()
}

/// The swipe a button starts on when it enters hold and drag: Back switches
/// Spaces, Forward scrolls, as Mac Mouse Fix sets them up.
pub(super) const fn first_drag(button: ButtonId) -> HeldDrag {
    match button {
        ButtonId::Back => HeldDrag::ThreeFingerSwipe,
        _ => HeldDrag::TwoFingerSwipe,
    }
}

/// Put `button` in `to`, keeping its click action. Each commit is a no-op
/// when the button is already in that mode.
fn commit_mode(
    state: &mut AppState,
    button: ButtonId,
    from: ButtonMode,
    to: ButtonMode,
) -> StateEvents {
    match (from, to) {
        (ButtonMode::Single, ButtonMode::Single) => StateEvents::none(),
        (ButtonMode::Gestures, ButtonMode::Single) => state.commit_gesture_mode(button, false),
        (ButtonMode::ScrollSideways, ButtonMode::Single) => state.commit_wheel_hold(button, false),
        (ButtonMode::HoldAndDrag, ButtonMode::Single) => state.commit_drag_hold(button, None),
        (_, ButtonMode::Gestures) => state.commit_gesture_mode(button, true),
        (_, ButtonMode::ScrollSideways) => state.commit_wheel_hold(button, true),
        (_, ButtonMode::HoldAndDrag) => state.commit_drag_hold(button, Some(first_drag(button))),
    }
}

/// The Mode list, or `None` where the button has only one mode or the
/// inspector edits an app profile.
pub(super) fn mode_list(
    button: ButtonId,
    data: &BindingInspectorData<'_>,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> Option<gpui::AnyElement> {
    if data.editing_app.is_some() {
        return None;
    }
    let modes = offered_modes(button, data);
    if modes.len() < 2 {
        return None;
    }
    let current = ButtonMode::of(button, data);
    let rows = modes.into_iter().map(|mode| {
        let selected = mode == current;
        let observer = view.clone();
        choice_row(
            mode.row_id(),
            mode.icon(button, data),
            mode.title(),
            mode.hint(),
            selected,
            pal,
        )
        .on_click(move |_, _, cx| {
            AppState::apply(cx, |state| commit_mode(state, button, current, mode));
            observer.update(cx, |view, cx| {
                view.close_action_picker();
                view.select_gesture_direction(GestureDirection::Click);
                cx.notify();
            });
        })
    });
    Some(
        v_flex()
            .gap_1()
            .debug_selector(|| "inspector-mode-list".into())
            .child(editor_section(tr!("actions.button_mode"), pal))
            .children(rows)
            .into_any_element(),
    )
}

/// Hold and drag's swipe: what the drag does, with the trackpad fingers it
/// stands for as the hint.
pub(super) fn drag_swipe_choice(
    button: ButtonId,
    current: HeldDrag,
    pal: Palette,
) -> impl IntoElement {
    let choices = [
        ("inspector-drag-hold-two", HeldDrag::TwoFingerSwipe),
        ("inspector-drag-hold-three", HeldDrag::ThreeFingerSwipe),
    ];
    v_flex()
        .gap_1()
        .child(editor_section(tr!("actions.drag_plays"), pal))
        .children(choices.map(|(id, drag)| {
            choice_row(
                id,
                drag_hold_icon(drag),
                drag_hold_text(drag),
                drag_hold_fingers(drag),
                current == drag,
                pal,
            )
            .on_click(move |_, _, cx| {
                AppState::apply(cx, |state| state.commit_drag_hold(button, Some(drag)));
            })
        }))
}

/// One row of a choice list: icon, title, hint, and a check when chosen.
fn choice_row(
    id: &'static str,
    icon: &'static str,
    title: gpui::SharedString,
    hint: gpui::SharedString,
    selected: bool,
    pal: Palette,
) -> MenuRow {
    let aria_label = format!("{title}: {hint}");
    MenuRow::new(id)
        .debug_selector(move || id.into())
        .selected(selected)
        .role(Role::Button)
        .aria_label(aria_label)
        .child(
            h_flex()
                .min_w_0()
                .gap_2()
                // A bare `Icon` takes the font size; `.size_4()` keeps it on
                // the 16px column the direction and action rows use.
                .child(Icon::empty().path(icon).size_4())
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().text_body().child(title))
                        .child(
                            div()
                                .truncate()
                                .text_caption()
                                .text_color(pal.text_muted)
                                .child(hint),
                        ),
                ),
        )
        .when(selected, |row| {
            row.child(
                Icon::new(IconName::Check)
                    .size_3()
                    .text_color(rgb(ACCENT_BLUE)),
            )
        })
}
