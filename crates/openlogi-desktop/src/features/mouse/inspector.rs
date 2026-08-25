//! Fixed binding inspector for the Buttons workspace.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, InteractiveElement, IntoElement, ParentElement, Role,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Icon, IconName, Selectable as _, Sizable as _, button::Button, h_flex, input::InputState,
    scroll::ScrollableElement as _, v_flex,
};
use openlogi_core::binding::{Action, ButtonId, GestureDirection, default_binding};

use super::hotspots::MouseControlId;
use super::picker::{
    GESTURE_BUTTON_ICON, PickFn, action_icon_path, action_rows_matching, editor_section,
};
use super::thumbwheel::ThumbwheelPreset;
use super::view::MouseModelView;
use crate::state::AppState;
use crate::ui::action::localized_action_label;
use crate::ui::components::{MenuRow, control_button, control_input};
use crate::ui::theme::{ACCENT_BLUE, Palette, Typography as _};

pub(super) const INSPECTOR_W: f32 = 328.;

#[derive(Clone, Copy)]
pub(super) struct BindingInspectorData<'a> {
    pub selected: Option<MouseControlId>,
    pub gesture_direction: Option<GestureDirection>,
    pub action_picker_open: bool,
    pub bindings: &'a BTreeMap<ButtonId, Action>,
    pub gesture_maps: &'a BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    pub editing_app: Option<&'a str>,
    pub overridden: Option<&'a BTreeMap<ButtonId, Action>>,
}

#[derive(Clone, Copy)]
struct ActionPickerContext<'a> {
    open: bool,
    search: &'a Entity<InputState>,
    view: &'a Entity<MouseModelView>,
}

pub(super) fn binding_inspector(
    data: BindingInspectorData<'_>,
    action_search: &Entity<InputState>,
    view: &Entity<MouseModelView>,
    pal: Palette,
    cx: &Context<MouseModelView>,
) -> AnyElement {
    let picker = ActionPickerContext {
        open: data.action_picker_open,
        search: action_search,
        view,
    };
    let body = match data.selected {
        None => empty_inspector(
            data.editing_app,
            data.overridden.map_or(0, BTreeMap::len),
            pal,
        ),
        Some(MouseControlId::ThumbwheelRotation) => thumbwheel_inspector(
            data.bindings,
            data.editing_app,
            data.overridden,
            picker,
            pal,
        ),
        Some(MouseControlId::Button(button)) => button_inspector(button, &data, picker, pal, cx),
    };

    v_flex()
        .w(px(INSPECTOR_W))
        .h_full()
        .min_h_0()
        .flex_shrink_0()
        .border_l_1()
        .border_color(pal.border)
        .bg(pal.panel)
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
    picker: ActionPickerContext<'_>,
    pal: Palette,
    cx: &Context<MouseModelView>,
) -> AnyElement {
    let gesture_map = data.gesture_maps.get(&button);
    let overridden = data
        .overridden
        .is_some_and(|overrides| overrides.contains_key(&button));
    if data.editing_app.is_none()
        && let Some(gesture_map) = gesture_map
    {
        return gesture_inspector(button, gesture_map, data.gesture_direction, picker, pal, cx);
    }
    if let Some(app) = data.editing_app
        && !overridden
        && gesture_map.is_some()
    {
        return inherited_gesture_inspector(button, app, picker, pal, cx);
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
    let observer = picker.view.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        AppState::update_bindings(cx, |state| state.commit_binding(button, action));
        observer.update(cx, |view, cx| {
            view.close_action_picker();
            cx.notify();
        });
    });

    v_flex()
        .gap_3()
        .child(inspector_heading(tr!(button.label()), Some(status), pal))
        .child(current_action_card(&action, picker, pal))
        .when(overridden, |panel| {
            let observer = picker.view.clone();
            panel.child(
                control_button("inspector-use-default")
                    .w_full()
                    .icon(IconName::Undo)
                    .label(tr!("Use the default profile"))
                    .on_click(move |_, _, cx| {
                        AppState::update_bindings(cx, |state| {
                            state.clear_app_binding(button);
                        });
                        observer.update(cx, |view, cx| {
                            view.close_action_picker();
                            cx.notify();
                        });
                    }),
            )
        })
        .when(
            data.editing_app.is_none()
                && (button.is_hidpp_gesture_source() || button.is_os_hook_button()),
            |panel| {
                let observer = picker.view.clone();
                panel.child(
                    control_button("inspector-use-gestures")
                        .w_full()
                        .icon(Icon::empty().path(GESTURE_BUTTON_ICON))
                        .label(tr!("Use gestures"))
                        .on_click(move |_, _, cx| {
                            AppState::update_bindings(cx, |state| {
                                state.commit_gesture_mode(button, true);
                            });
                            observer.update(cx, |view, cx| {
                                view.set_gesture_selected_dir(Some(GestureDirection::Click));
                                cx.notify();
                            });
                        }),
                )
            },
        )
        .when(picker.open, |panel| {
            panel.child(action_library(
                "inspector-action",
                Some(&action),
                picker.search,
                &on_pick,
                pal,
                cx,
            ))
        })
        .into_any_element()
}

fn inherited_gesture_inspector(
    button: ButtonId,
    app: &str,
    picker: ActionPickerContext<'_>,
    pal: Palette,
    cx: &Context<MouseModelView>,
) -> AnyElement {
    let observer = picker.view.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        AppState::update_bindings(cx, |state| state.commit_binding(button, action));
        observer.update(cx, |view, cx| {
            view.close_action_picker();
            cx.notify();
        });
    });
    let edit_default = picker.view.clone();
    v_flex()
        .gap_3()
        .child(inspector_heading(
            tr!(button.label()),
            Some(tr!("Inherited from Default")),
            pal,
        ))
        .child(gesture_summary_card(picker, pal))
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
                    AppState::update_bindings(cx, |state| state.set_editing_app(None));
                    edit_default.update(cx, |view, cx| {
                        view.set_gesture_selected_dir(Some(GestureDirection::Click));
                        cx.notify();
                    });
                }),
        )
        .when(picker.open, |panel| {
            panel.child(action_library(
                "inspector-gesture-override",
                None,
                picker.search,
                &on_pick,
                pal,
                cx,
            ))
        })
        .into_any_element()
}

fn gesture_inspector(
    button: ButtonId,
    gesture_map: &BTreeMap<GestureDirection, Action>,
    selected_direction: Option<GestureDirection>,
    picker: ActionPickerContext<'_>,
    pal: Palette,
    cx: &Context<MouseModelView>,
) -> AnyElement {
    let direction = selected_direction.unwrap_or(GestureDirection::Click);
    let current = gesture_action(gesture_map, button, direction);
    let observer = picker.view.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        AppState::update_bindings(cx, |state| {
            state.commit_gesture_binding(button, direction, action);
        });
        observer.update(cx, |view, cx| {
            view.close_action_picker();
            cx.notify();
        });
    });
    let turn_off = picker.view.clone();

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
            picker.view,
            pal,
        ))
        .child(current_action_card(&current, picker, pal))
        .child(
            control_button("inspector-single-action")
                .w_full()
                .label(tr!("Use a single action"))
                .on_click(move |_, _, cx| {
                    AppState::update_bindings(cx, |state| {
                        state.commit_gesture_mode(button, false);
                    });
                    turn_off.update(cx, |view, cx| {
                        view.set_gesture_selected_dir(None);
                        cx.notify();
                    });
                }),
        )
        .when(picker.open, |panel| {
            panel.child(action_library(
                "inspector-gesture-action",
                Some(&current),
                picker.search,
                &on_pick,
                pal,
                cx,
            ))
        })
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
        .child(editor_section(tr!("Direction"), pal))
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
    overridden: Option<&BTreeMap<ButtonId, Action>>,
    picker: ActionPickerContext<'_>,
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
    let is_overridden = overridden.is_some_and(|overrides| {
        overrides.contains_key(&ButtonId::ThumbwheelScrollDown)
            || overrides.contains_key(&ButtonId::ThumbwheelScrollUp)
    });
    let status = match (editing_app, is_overridden) {
        (Some(app), true) => tr!("Overridden in %{app}", app => app.to_string()),
        (Some(_), false) => tr!("Inherited from Default"),
        (None, _) => tr!("Default profile"),
    };
    let current_label = current.map_or_else(|| tr!("Custom"), |preset| tr!(preset.label()));
    let current_icon = current.map_or("action-icons/chevrons-right.svg", ThumbwheelPreset::icon);
    let observer = picker.view.clone();

    v_flex()
        .gap_3()
        .child(inspector_heading(tr!("Thumb Wheel"), Some(status), pal))
        .child(selection_card(
            "inspector-current-thumbwheel-preset",
            tr!("Preset"),
            current_icon,
            current_label,
            picker,
            pal,
        ))
        .when(picker.open, |panel| {
            panel.child(
                v_flex()
                    .gap_1()
                    .child(editor_section(tr!("Preset"), pal))
                    .children(ThumbwheelPreset::ALL.into_iter().enumerate().map(
                        |(index, preset)| {
                            let selected = current == Some(preset);
                            let observer = observer.clone();
                            MenuRow::new(("inspector-thumbwheel", index))
                                .selected(selected)
                                .role(Role::Button)
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
                                    AppState::update_bindings(cx, |state| {
                                        state.commit_thumbwheel_preset(preset);
                                    });
                                    observer.update(cx, |view, cx| {
                                        view.close_action_picker();
                                        cx.notify();
                                    });
                                })
                        },
                    )),
            )
        })
        .when(is_overridden, |panel| {
            let observer = picker.view.clone();
            panel.child(
                Button::new("inspector-thumbwheel-use-default")
                    .small()
                    .w_full()
                    .icon(IconName::Undo)
                    .label(tr!("Use the default profile"))
                    .on_click(move |_, _, cx| {
                        AppState::update_bindings(cx, |state| {
                            state.clear_app_thumbwheel();
                        });
                        observer.update(cx, |view, cx| {
                            view.close_action_picker();
                            cx.notify();
                        });
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

fn current_action_card(
    action: &Action,
    picker: ActionPickerContext<'_>,
    pal: Palette,
) -> AnyElement {
    selection_card(
        "inspector-current-action",
        tr!("Current action"),
        action_icon_path(action),
        localized_action_label(action),
        picker,
        pal,
    )
}

fn gesture_summary_card(picker: ActionPickerContext<'_>, pal: Palette) -> AnyElement {
    selection_card(
        "inspector-current-gesture-summary",
        tr!("Current action"),
        GESTURE_BUTTON_ICON,
        tr!("5 directions"),
        picker,
        pal,
    )
}

fn selection_card(
    id: &'static str,
    caption: gpui::SharedString,
    icon: &'static str,
    value: gpui::SharedString,
    picker: ActionPickerContext<'_>,
    pal: Palette,
) -> AnyElement {
    let toggle = picker.view.clone();
    let search = picker.search.clone();
    let opening = !picker.open;
    let accessible_label = value.clone();
    BaseButton::new(id)
        .accessibility_label(accessible_label)
        .aria_expanded(picker.open)
        .flex()
        .flex_col()
        .gap_2()
        .rounded(pal.control_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.control)
        .p_3()
        .cursor_pointer()
        .hover(move |card| card.bg(pal.control_hover))
        .focus_visible(move |card| card.bg(pal.control_hover).border_color(rgb(ACCENT_BLUE)))
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(caption),
        )
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    h_flex()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(
                            svg()
                                .path(icon)
                                .size_4()
                                .flex_none()
                                .text_color(pal.text_muted),
                        )
                        .child(div().min_w_0().truncate().text_body().child(value)),
                )
                .child(
                    svg()
                        .path(if picker.open {
                            "action-icons/chevrons-up.svg"
                        } else {
                            "action-icons/chevrons-down.svg"
                        })
                        .size_3()
                        .flex_none()
                        .text_color(pal.text_muted),
                ),
        )
        .on_click(move |_, window, cx| {
            if opening {
                search.update(cx, |search, cx| search.set_value("", window, cx));
            }
            toggle.update(cx, |view, cx| {
                view.toggle_action_picker();
                cx.notify();
            });
        })
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
        .child(editor_section(tr!("Actions"), pal))
        .child(control_input(action_search).cleanable(true))
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
