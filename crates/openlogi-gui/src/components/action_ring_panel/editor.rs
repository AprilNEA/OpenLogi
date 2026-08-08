//! Categorized action, shortcut, path, and icon editor for one ring slot.

use gpui::{
    AnyElement, BorrowAppContext as _, Entity, InteractiveElement, IntoElement, ParentElement,
    Role, StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{
    Icon, IconName, Selectable as _, Sizable as _,
    button::Button,
    h_flex,
    input::{Input, InputState},
    scroll::ScrollableElement as _,
    v_flex,
};
use openlogi_core::binding::{
    Action, ActionRingIcon, ActionRingSlot, ApplicationTarget, Category, KeyCombo, RingAction,
};

use crate::action_icons::{action_icon_path, ring_icon_path};
use crate::state::AppState;
use crate::theme::{self, Palette, SelectableStyle as _, Typography as _};

pub(super) fn action_library(
    slot: ActionRingSlot,
    current: Option<&RingAction>,
    current_icon: Option<ActionRingIcon>,
    application_input: &Entity<InputState>,
    shortcut_input: &Entity<InputState>,
    pal: Palette,
) -> impl IntoElement {
    let current_action = current.map(RingAction::action).cloned();
    let current_label = current_action.as_ref().map_or_else(
        || tr!("Empty slot").to_string(),
        |action| rust_i18n::t!(action.label()).into_owned(),
    );

    v_flex()
        .flex_1()
        .min_w(px(280.0))
        .max_w(px(320.0))
        .h(px(420.0))
        .overflow_hidden()
        .rounded(pal.card_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.surface)
        .child(
            v_flex()
                .flex_none()
                .gap_1()
                .border_b_1()
                .border_color(pal.border)
                .p_3()
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(div().text_subheading().child(tr!("Actions Ring")))
                        .child(
                            Button::new("ring-clear-slot")
                                .compact()
                                .label(tr!("Clear slot"))
                                .on_click(move |_, _, cx| commit_slot(slot, None, cx)),
                        ),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(current_label),
                ),
        )
        .child(
            div()
                .id("ring-action-library")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(
                    v_flex()
                        .p_1p5()
                        .child(icon_editor(
                            slot,
                            current_action.as_ref(),
                            current_icon,
                            pal,
                        ))
                        .child(shortcut_editor(slot, shortcut_input, pal))
                        .child(path_editor(slot, application_input, pal))
                        .children(action_rows(slot, current_action.as_ref(), pal)),
                ),
        )
}

fn icon_editor(
    slot: ActionRingSlot,
    action: Option<&Action>,
    current: Option<ActionRingIcon>,
    pal: Palette,
) -> impl IntoElement {
    let default_path = action.map_or("action-icons/ban.svg", action_icon_path);
    let default = icon_button(
        "ring-default-icon",
        default_path,
        tr!("Use action icon"),
        current.is_none(),
        pal,
    )
    .on_click(move |_, _, cx| commit_icon(slot, None, cx));

    v_flex()
        .gap_1()
        .child(section_header(tr!("Icon"), pal))
        .child(
            h_flex().flex_wrap().gap_1().child(default).children(
                ActionRingIcon::ALL
                    .into_iter()
                    .enumerate()
                    .map(move |(index, icon)| {
                        icon_button(
                            ("ring-custom-icon", index),
                            ring_icon_path(icon),
                            rust_i18n::t!(icon.label()),
                            current == Some(icon),
                            pal,
                        )
                        .on_click(move |_, _, cx| commit_icon(slot, Some(icon), cx))
                    }),
            ),
        )
}

fn icon_button(
    id: impl Into<gpui::ElementId>,
    path: &'static str,
    label: impl Into<gpui::SharedString>,
    selected: bool,
    pal: Palette,
) -> Button {
    Button::new(id)
        .size(px(32.0))
        .rounded(px(16.0))
        .selected(selected)
        .icon(Icon::empty().path(path).text_color(pal.text_muted))
        .tooltip(label)
}

fn shortcut_editor(
    slot: ActionRingSlot,
    input: &Entity<InputState>,
    pal: Palette,
) -> impl IntoElement {
    let submit_input = input.clone();
    v_flex()
        .gap_1()
        .child(section_header(tr!("Custom shortcut"), pal))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(input).small().cleanable(true)),
                )
                .child(
                    Button::new("ring-add-shortcut")
                        .compact()
                        .label(tr!("Add"))
                        .on_click(move |_, _, cx| {
                            let shortcut = submit_input.read(cx).value().to_string();
                            if let Ok(combo) = shortcut.parse::<KeyCombo>() {
                                commit_slot(slot, Some(Action::CustomShortcut(combo)), cx);
                            }
                        }),
                ),
        )
}

fn path_editor(slot: ActionRingSlot, input: &Entity<InputState>, pal: Palette) -> impl IntoElement {
    let submit_input = input.clone();
    v_flex()
        .gap_1()
        .child(section_header(tr!("Open application or folder"), pal))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(input).small().cleanable(true)),
                )
                .child(
                    Button::new("ring-add-path")
                        .compact()
                        .label(tr!("Add"))
                        .on_click(move |_, _, cx| {
                            let path = submit_input.read(cx).value().to_string();
                            if let Ok(target) = ApplicationTarget::new(path, "") {
                                commit_slot(slot, Some(Action::OpenApplication(target)), cx);
                            }
                        }),
                ),
        )
}

fn action_rows(slot: ActionRingSlot, current: Option<&Action>, pal: Palette) -> Vec<AnyElement> {
    let mut index = 0usize;
    let mut rows = Vec::new();
    for (category, actions) in ring_catalog() {
        rows.push(section_header(rust_i18n::t!(category.label()), pal));
        for action in actions {
            let selected = current == Some(&action);
            let action_to_commit = action.clone();
            let label = tr!(action.label());
            let icon_path = action_icon_path(&action);
            let row_index = index;
            index += 1;
            rows.push(
                h_flex()
                    .id(("ring-action", row_index))
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(pal.control_radius)
                    .text_body()
                    .text_color(pal.text_primary)
                    .selected_fill(selected)
                    .role(Role::MenuItem)
                    .aria_label(label.clone())
                    .aria_selected(selected)
                    .cursor_pointer()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                svg()
                                    .path(icon_path)
                                    .size_4()
                                    .flex_none()
                                    .text_color(pal.text_muted),
                            )
                            .child(label),
                    )
                    .when(selected, |row| {
                        row.child(
                            Icon::new(IconName::Check)
                                .size_3()
                                .text_color(rgb(theme::ACCENT_BLUE)),
                        )
                    })
                    .hover(move |row| row.bg(pal.surface_hover))
                    .on_click(move |_, _, cx| {
                        commit_slot(slot, Some(action_to_commit.clone()), cx);
                    })
                    .into_any_element(),
            );
        }
    }
    rows
}

fn section_header(label: impl Into<gpui::SharedString>, pal: Palette) -> AnyElement {
    div()
        .w_full()
        .px_2()
        .pt_2()
        .pb_0p5()
        .text_caption()
        .text_color(pal.text_muted)
        .child(label.into().to_uppercase())
        .into_any_element()
}

fn ring_catalog() -> Vec<(Category, Vec<Action>)> {
    let mut sections: Vec<(Category, Vec<Action>)> = Vec::new();
    for action in Action::catalog() {
        if RingAction::new(action.clone()).is_err() {
            continue;
        }
        let category = action.category();
        if let Some((_, actions)) = sections
            .iter_mut()
            .find(|(candidate, _)| *candidate == category)
        {
            actions.push(action);
        } else {
            sections.push((category, vec![action]));
        }
    }
    sections
}

fn commit_slot(slot: ActionRingSlot, action: Option<Action>, cx: &mut gpui::App) {
    let action = action.and_then(|action| RingAction::new(action).ok());
    cx.update_global::<AppState, _>(|state, _| {
        state.commit_action_ring_slot(slot, action);
    });
    cx.refresh_windows();
}

fn commit_icon(slot: ActionRingSlot, icon: Option<ActionRingIcon>, cx: &mut gpui::App) {
    cx.update_global::<AppState, _>(|state, _| {
        state.commit_action_ring_icon(slot, icon);
    });
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_catalog_is_categorized_and_excludes_invalid_actions() {
        let sections = ring_catalog();
        assert!(
            sections
                .iter()
                .any(|(category, _)| *category == Category::Navigation)
        );
        let actions = sections
            .into_iter()
            .flat_map(|(_, actions)| actions)
            .collect::<Vec<_>>();
        assert!(actions.contains(&Action::MissionControl));
        assert!(!actions.contains(&Action::None));
        assert!(!actions.contains(&Action::ShowActionsRing));
    }
}
