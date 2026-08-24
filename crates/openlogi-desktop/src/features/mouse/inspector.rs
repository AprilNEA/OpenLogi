//! Fixed binding inspector for the Buttons workspace.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gpui::{
    AnyElement, BorrowAppContext as _, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, Role, StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _,
    px, rgb, svg,
};
use gpui_component::{
    Icon, IconName, Selectable as _, Sizable as _,
    button::Button,
    h_flex,
    input::{Input, InputState},
    scroll::ScrollableElement as _,
    v_flex,
};
use openlogi_core::binding::{Action, ButtonId, GestureDirection, default_binding};

use super::hotspots::MouseControlId;
use super::picker::{
    GESTURE_BUTTON_ICON, PickFn, action_icon_path, action_rows_matching, popover_section,
};
use super::thumbwheel::ThumbwheelPreset;
use super::view::{MouseModelView, localized_action_label};
use crate::state::AppState;
use crate::ui::components::MenuRow;
use crate::ui::theme::{ACCENT_BLUE, Palette, Typography as _};

pub(super) const INSPECTOR_W: f32 = 328.;

#[derive(Clone, Copy)]
pub(super) struct BindingInspectorData<'a> {
    pub selected: Option<MouseControlId>,
    pub gesture_direction: Option<GestureDirection>,
    pub bindings: &'a BTreeMap<ButtonId, Action>,
    pub gesture_maps: &'a BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    pub editing_app: Option<&'a str>,
    pub overridden: &'a BTreeSet<ButtonId>,
}

pub(super) fn binding_inspector(
    data: BindingInspectorData<'_>,
    action_search: &Entity<InputState>,
    view: &Entity<MouseModelView>,
    pal: Palette,
    cx: &mut Context<MouseModelView>,
) -> AnyElement {
    let body = match data.selected {
        None => empty_inspector(data.editing_app, data.overridden.len(), pal),
        Some(MouseControlId::ThumbwheelRotation) => {
            thumbwheel_inspector(data.bindings, data.editing_app, data.overridden, view, pal)
        }
        Some(MouseControlId::Button(button)) => {
            button_inspector(button, &data, action_search, view, pal, cx)
        }
    };

    v_flex()
        .w(px(INSPECTOR_W))
        .h_full()
        .min_h_0()
        .flex_shrink_0()
        .border_l_1()
        .border_color(pal.border)
        .bg(pal.surface)
        .child(
            div()
                .id("button-inspector-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .p_4()
                .child(body),
        )
        .into_any_element()
}

fn empty_inspector(app: Option<&str>, override_count: usize, pal: Palette) -> AnyElement {
    let summary = match (app, override_count) {
        (Some(app), 0) => tr!(
            "No overrides yet. Select a button to customize for %{app}.",
            app => app.to_string()
        ),
        (Some(app), 1) => tr!(
            "%{app} overrides 1 button. Others inherit Default.",
            app => app.to_string()
        ),
        (Some(app), count) => tr!(
            "%{app} overrides %{count} buttons. Others inherit Default.",
            app => app.to_string(),
            count => count.to_string()
        ),
        (None, _) => tr!("Select a button on the device to change what it does."),
    };
    v_flex()
        .gap_3()
        .child(inspector_heading(tr!("Button inspector"), None, pal))
        .child(div().text_body().text_color(pal.text_muted).child(summary))
        .into_any_element()
}

fn button_inspector(
    button: ButtonId,
    data: &BindingInspectorData<'_>,
    action_search: &Entity<InputState>,
    view: &Entity<MouseModelView>,
    pal: Palette,
    cx: &mut Context<MouseModelView>,
) -> AnyElement {
    let gesture_map = data.gesture_maps.get(&button);
    let overridden = data.overridden.contains(&button);
    if data.editing_app.is_none()
        && let Some(gesture_map) = gesture_map
    {
        return gesture_inspector(
            button,
            gesture_map,
            data.gesture_direction,
            action_search,
            view,
            pal,
            cx,
        );
    }
    if let Some(app) = data.editing_app
        && !overridden
        && gesture_map.is_some()
    {
        return inherited_gesture_inspector(button, app, action_search, view, pal, cx);
    }

    let action = data
        .bindings
        .get(&button)
        .cloned()
        .unwrap_or_else(|| default_binding(button));
    let status = match (
        data.editing_app,
        overridden,
        action == default_binding(button),
    ) {
        (Some(app), true, _) => tr!("Overridden in %{app}", app => app.to_string()),
        (Some(_), false, _) => tr!("Inherited from Default"),
        (None, _, true) => tr!("Device default"),
        (None, _, false) => tr!("Customized"),
    };
    let observer = view.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        cx.update_global::<AppState, _>(|state, _| state.commit_binding(button, action));
        observer.update(cx, |_, cx| cx.notify());
        cx.refresh_windows();
    });

    v_flex()
        .gap_3()
        .child(inspector_heading(tr!(button.label()), Some(status), pal))
        .child(current_action_card(&action, pal))
        .when(overridden, |panel| {
            let observer = view.clone();
            panel.child(
                Button::new("inspector-use-default")
                    .small()
                    .w_full()
                    .icon(IconName::Undo)
                    .label(tr!("Use the default profile"))
                    .on_click(move |_, _, cx| {
                        cx.update_global::<AppState, _>(|state, _| {
                            state.clear_app_binding(button);
                        });
                        observer.update(cx, |_, cx| cx.notify());
                        cx.refresh_windows();
                    }),
            )
        })
        .when(
            data.editing_app.is_none()
                && (button.is_hidpp_gesture_source() || button.is_os_hook_button()),
            |panel| {
                let observer = view.clone();
                panel.child(
                    Button::new("inspector-use-gestures")
                        .small()
                        .w_full()
                        .icon(Icon::empty().path(GESTURE_BUTTON_ICON))
                        .label(tr!("Use gestures"))
                        .on_click(move |_, _, cx| {
                            cx.update_global::<AppState, _>(|state, _| {
                                state.commit_gesture_mode(button, true);
                            });
                            observer.update(cx, |view, cx| {
                                view.set_gesture_selected_dir(Some(GestureDirection::Click));
                                cx.notify();
                            });
                            cx.refresh_windows();
                        }),
                )
            },
        )
        .child(action_library(
            "inspector-action",
            Some(&action),
            action_search,
            &on_pick,
            pal,
            cx,
        ))
        .into_any_element()
}

fn inherited_gesture_inspector(
    button: ButtonId,
    app: &str,
    action_search: &Entity<InputState>,
    view: &Entity<MouseModelView>,
    pal: Palette,
    cx: &mut Context<MouseModelView>,
) -> AnyElement {
    let observer = view.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        cx.update_global::<AppState, _>(|state, _| state.commit_binding(button, action));
        observer.update(cx, |_, cx| cx.notify());
        cx.refresh_windows();
    });
    let edit_default = view.clone();
    v_flex()
        .gap_3()
        .child(inspector_heading(
            tr!(button.label()),
            Some(tr!("Inherited from Default")),
            pal,
        ))
        .child(gesture_summary_card(pal))
        .child(div().text_caption().text_color(pal.text_muted).child(tr!(
            "Choosing an action replaces the inherited gestures in %{app}.",
            app => app.to_string()
        )))
        .child(
            Button::new("inspector-edit-default-gestures")
                .small()
                .w_full()
                .label(tr!("Edit Default gestures"))
                .on_click(move |_, _, cx| {
                    cx.update_global::<AppState, _>(|state, _| state.set_editing_app(None));
                    edit_default.update(cx, |view, cx| {
                        view.set_gesture_selected_dir(Some(GestureDirection::Click));
                        cx.notify();
                    });
                    cx.refresh_windows();
                }),
        )
        .child(action_library(
            "inspector-gesture-override",
            None,
            action_search,
            &on_pick,
            pal,
            cx,
        ))
        .into_any_element()
}

fn gesture_inspector(
    button: ButtonId,
    gesture_map: &BTreeMap<GestureDirection, Action>,
    selected_direction: Option<GestureDirection>,
    action_search: &Entity<InputState>,
    view: &Entity<MouseModelView>,
    pal: Palette,
    cx: &mut Context<MouseModelView>,
) -> AnyElement {
    let direction = selected_direction.unwrap_or(GestureDirection::Click);
    let current = gesture_action(gesture_map, button, direction);
    let observer = view.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        cx.update_global::<AppState, _>(|state, _| {
            state.commit_gesture_binding(button, direction, action);
        });
        observer.update(cx, |_, cx| cx.notify());
        cx.refresh_windows();
    });
    let turn_off = view.clone();

    v_flex()
        .gap_3()
        .child(inspector_heading(
            tr!(button.label()),
            Some(tr!("5 directions")),
            pal,
        ))
        .child(gesture_directions(
            direction,
            gesture_map,
            button,
            view,
            pal,
        ))
        .child(current_action_card(&current, pal))
        .child(
            Button::new("inspector-single-action")
                .small()
                .w_full()
                .label(tr!("Use a single action"))
                .on_click(move |_, _, cx| {
                    cx.update_global::<AppState, _>(|state, _| {
                        state.commit_gesture_mode(button, false);
                    });
                    turn_off.update(cx, |view, cx| {
                        view.set_gesture_selected_dir(None);
                        cx.notify();
                    });
                    cx.refresh_windows();
                }),
        )
        .child(action_library(
            "inspector-gesture-action",
            Some(&current),
            action_search,
            &on_pick,
            pal,
            cx,
        ))
        .into_any_element()
}

fn gesture_directions(
    active: GestureDirection,
    gesture_map: &BTreeMap<GestureDirection, Action>,
    button: ButtonId,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> AnyElement {
    v_flex()
        .gap_1()
        .child(popover_section(tr!("Direction"), pal))
        .children(
            GestureDirection::ALL
                .into_iter()
                .enumerate()
                .map(|(index, direction)| {
                    let selected = direction == active;
                    let action = gesture_action(gesture_map, button, direction);
                    let view = view.clone();
                    MenuRow::new(("inspector-direction", index))
                        .selected(selected)
                        .role(Role::Button)
                        .aria_selected(selected)
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(div().text_body().child(format!(
                                    "{}  {}",
                                    direction.glyph(),
                                    tr!(direction.label())
                                )))
                                .child(
                                    div()
                                        .truncate()
                                        .text_caption()
                                        .text_color(pal.text_muted)
                                        .child(localized_action_label(&action)),
                                ),
                        )
                        .when(selected, |row| {
                            row.child(
                                Icon::new(IconName::Check)
                                    .size_3()
                                    .text_color(rgb(ACCENT_BLUE)),
                            )
                        })
                        .on_click(move |_, _, cx| {
                            view.update(cx, |view, cx| {
                                view.set_gesture_selected_dir(Some(direction));
                                cx.notify();
                            });
                        })
                }),
        )
        .into_any_element()
}

fn thumbwheel_inspector(
    bindings: &BTreeMap<ButtonId, Action>,
    editing_app: Option<&str>,
    overridden: &BTreeSet<ButtonId>,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> AnyElement {
    let backward = bindings
        .get(&ButtonId::ThumbwheelScrollDown)
        .cloned()
        .unwrap_or_else(|| default_binding(ButtonId::ThumbwheelScrollDown));
    let forward = bindings
        .get(&ButtonId::ThumbwheelScrollUp)
        .cloned()
        .unwrap_or_else(|| default_binding(ButtonId::ThumbwheelScrollUp));
    let current = ThumbwheelPreset::recognize(&backward, &forward);
    let is_overridden = overridden.contains(&ButtonId::ThumbwheelScrollDown)
        || overridden.contains(&ButtonId::ThumbwheelScrollUp);
    let status = match (editing_app, is_overridden) {
        (Some(app), true) => tr!("Overridden in %{app}", app => app.to_string()),
        (Some(_), false) => tr!("Inherited from Default"),
        (None, _) => tr!("Default profile"),
    };
    let observer = view.clone();

    v_flex()
        .gap_3()
        .child(inspector_heading(tr!("Thumb Wheel"), Some(status), pal))
        .child(
            v_flex()
                .gap_1()
                .child(popover_section(tr!("Preset"), pal))
                .children(
                    ThumbwheelPreset::ALL
                        .into_iter()
                        .enumerate()
                        .map(|(index, preset)| {
                            let selected = current == Some(preset);
                            let observer = observer.clone();
                            MenuRow::new(("inspector-thumbwheel", index))
                                .selected(selected)
                                .role(Role::Button)
                                .aria_selected(selected)
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            svg()
                                                .path(preset.icon())
                                                .size_4()
                                                .text_color(pal.text_muted),
                                        )
                                        .child(div().child(tr!(preset.label()))),
                                )
                                .when(selected, |row| {
                                    row.child(
                                        Icon::new(IconName::Check)
                                            .size_3()
                                            .text_color(rgb(ACCENT_BLUE)),
                                    )
                                })
                                .on_click(move |_, _, cx| {
                                    cx.update_global::<AppState, _>(|state, _| {
                                        state.commit_thumbwheel_preset(preset);
                                    });
                                    observer.update(cx, |_, cx| cx.notify());
                                    cx.refresh_windows();
                                })
                        }),
                ),
        )
        .when(is_overridden, |panel| {
            let observer = view.clone();
            panel.child(
                Button::new("inspector-thumbwheel-use-default")
                    .small()
                    .w_full()
                    .icon(IconName::Undo)
                    .label(tr!("Use the default profile"))
                    .on_click(move |_, _, cx| {
                        cx.update_global::<AppState, _>(|state, _| {
                            state.clear_app_thumbwheel();
                        });
                        observer.update(cx, |_, cx| cx.notify());
                        cx.refresh_windows();
                    }),
            )
        })
        .into_any_element()
}

fn inspector_heading(
    title: gpui::SharedString,
    status: Option<gpui::SharedString>,
    pal: Palette,
) -> AnyElement {
    v_flex()
        .gap_1()
        .child(div().text_heading().child(title))
        .children(status.map(|status| {
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(status)
        }))
        .into_any_element()
}

fn current_action_card(action: &Action, pal: Palette) -> AnyElement {
    v_flex()
        .gap_2()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.surface_hover)
        .p_3()
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("Current action")),
        )
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .path(action_icon_path(action))
                        .size_4()
                        .text_color(pal.text_muted),
                )
                .child(div().text_body().child(localized_action_label(action))),
        )
        .into_any_element()
}

fn gesture_summary_card(pal: Palette) -> AnyElement {
    v_flex()
        .gap_2()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.surface_hover)
        .p_3()
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("Current action")),
        )
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .path(GESTURE_BUTTON_ICON)
                        .size_4()
                        .text_color(pal.text_muted),
                )
                .child(div().text_body().child(tr!("5 directions"))),
        )
        .into_any_element()
}

fn action_library(
    id_prefix: &'static str,
    current: Option<&Action>,
    action_search: &Entity<InputState>,
    on_pick: &PickFn,
    pal: Palette,
    cx: &Context<MouseModelView>,
) -> AnyElement {
    let query = action_search.read(cx).value();
    let rows = action_rows_matching(id_prefix, current, &query, on_pick, pal);
    v_flex()
        .gap_2()
        .pt_1()
        .child(popover_section(tr!("Actions"), pal))
        .child(Input::new(action_search).small().cleanable(true))
        .child(
            v_flex()
                .gap_0p5()
                .when(rows.is_empty(), |list| {
                    list.child(
                        div()
                            .py_3()
                            .text_body()
                            .text_color(pal.text_muted)
                            .child(tr!("No actions found")),
                    )
                })
                .children(rows),
        )
        .into_any_element()
}

fn gesture_action(
    gesture_map: &BTreeMap<GestureDirection, Action>,
    button: ButtonId,
    direction: GestureDirection,
) -> Action {
    gesture_map.get(&direction).cloned().unwrap_or_else(|| {
        if direction == GestureDirection::Click {
            default_binding(button)
        } else {
            Action::None
        }
    })
}
