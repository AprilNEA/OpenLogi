//! Categorized action, shortcut, path, icon, and wheel editor for one ring
//! slot.

use std::rc::Rc;

use gpui::{
    Entity, InteractiveElement, IntoElement, ParentElement, Role, ScrollHandle,
    StatefulInteractiveElement as _, Styled, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_kit::component::{
    Icon, IconName, Selectable as _, button::Button, h_flex, input::InputState,
    scroll::ScrollableElement as _, v_flex,
};
use openlogi_core::binding::{
    Action, ActionRingEntry, ActionRingIcon, ActionRingSlot, ApplicationTarget, Category, KeyCombo,
    RingAction, RingScroll, RingScrollDirection, RingScrollPreset,
};

use super::action_icons::action_icon_path;
use crate::features::binding_editor::editor_section;
use crate::features::keyboard::editors::PowerUserKind;
use crate::state::AppState;
use crate::ui::action::localized_action_label;
use crate::ui::components::{MenuRow, control_input};
use crate::ui::theme::{self, Palette, Typography as _};

/// Height of the fade over the library's bottom edge. The library ends with
/// as much padding, so its last row can scroll clear of the fade.
const LIBRARY_FADE_H: f32 = 24.;

/// The three single-field Power User inputs the ring's slot editor exposes.
/// [`PowerUserKind::Workflow`] needs a multi-step list editor this panel
/// doesn't have yet, so it stays keyboard-only for now.
pub(super) struct PowerUserInputs {
    pub(super) type_text: Entity<InputState>,
    pub(super) applescript: Entity<InputState>,
    pub(super) shell_command: Entity<InputState>,
}

/// Which of a slot's actions the library's rows and fields currently set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum LibraryTarget {
    /// The action a click on the slot runs.
    #[default]
    Click,
    /// The action one wheel step in this direction runs.
    Scroll(RingScrollDirection),
}

impl LibraryTarget {
    /// Where the library goes once it set this target: up leads on to down,
    /// so a custom pair is two picks in a row, and down returns to the click.
    const fn after_commit(self) -> Self {
        match self {
            Self::Scroll(RingScrollDirection::Up) => Self::Scroll(RingScrollDirection::Down),
            Self::Click | Self::Scroll(RingScrollDirection::Down) => Self::Click,
        }
    }
}

/// Points the library at another of the slot's actions.
pub(super) type Retarget = Rc<dyn Fn(LibraryTarget, &mut gpui::App)>;

/// The slot the editor edits, and which of its actions the library sets.
#[derive(Clone)]
pub(super) struct SlotEditing {
    pub(super) slot: ActionRingSlot,
    pub(super) target: LibraryTarget,
    /// Point the library at another of the slot's actions.
    pub(super) retarget: Retarget,
}

impl SlotEditing {
    /// Set the library's current target to `action`.
    fn commit(&self, action: Action, cx: &mut gpui::App) {
        let Ok(action) = RingAction::new(action) else {
            return;
        };
        let slot = self.slot;
        match self.target {
            LibraryTarget::Click => commit_slot(slot, Some(action), cx),
            LibraryTarget::Scroll(direction) => AppState::apply(cx, |state| {
                state.commit_action_ring_scroll_action(slot, direction, action)
            }),
        }
        (self.retarget)(self.target.after_commit(), cx);
    }
}

pub(super) fn action_library(
    editing: &SlotEditing,
    current: Option<&ActionRingEntry>,
    application_input: &Entity<InputState>,
    shortcut_input: &Entity<InputState>,
    power_user: &PowerUserInputs,
    library_scroll: &ScrollHandle,
    pal: Palette,
) -> impl IntoElement {
    let slot = editing.slot;
    let current_action = current.map(ActionRingEntry::action).cloned();
    let scroll = current.and_then(ActionRingEntry::scroll);
    // The library marks the action its target holds now.
    let target_action = match editing.target {
        LibraryTarget::Click => current_action.clone(),
        LibraryTarget::Scroll(direction) => scroll.map(|pair| pair.action(direction).clone()),
    };
    let current_label = current_action
        .as_ref()
        .map_or_else(|| tr!("action_ring.empty_slot"), localized_action_label);
    let clear = editing.clone();

    v_flex()
        .debug_selector(|| "ring-slot-editor".into())
        .flex_1()
        .min_w(px(280.0))
        .max_w(px(320.0))
        .h(px(420.0))
        .overflow_hidden()
        .rounded(pal.card_radius)
        .border_1()
        .border_color(pal.border)
        .bg(pal.panel)
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
                        .child(div().text_subheading().child(slot_title(slot)))
                        .child(
                            Button::new("ring-clear-slot")
                                .compact()
                                .label(tr!("action_ring.clear_slot"))
                                .on_click(move |_, _, cx| {
                                    commit_slot(slot, None, cx);
                                    (clear.retarget)(LibraryTarget::Click, cx);
                                }),
                        ),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(current_label),
                )
                .when_some(choosing_banner(editing), ParentElement::child),
        )
        .child(action_rows_scroller(
            v_flex()
                .p_1p5()
                .pb(px(LIBRARY_FADE_H))
                .when_some(current_action.as_ref(), |library, action| {
                    library
                        .child(scroll_editor(editing, scroll, pal))
                        .child(icon_editor(
                            slot,
                            action,
                            current.and_then(ActionRingEntry::custom_icon),
                            pal,
                        ))
                })
                .child(path_editor(editing, application_input, pal))
                .child(text_action_editor(
                    editing,
                    PowerUserKind::TypeText,
                    &power_user.type_text,
                    pal,
                ))
                // RunAppleScript has no execution path outside macOS
                // (`openlogi-inject`'s Linux/Windows backends only log a
                // warning and do nothing) — hide the editor there instead of
                // letting a binding be saved that silently never fires.
                .when(cfg!(target_os = "macos"), |library| {
                    library.child(text_action_editor(
                        editing,
                        PowerUserKind::RunAppleScript,
                        &power_user.applescript,
                        pal,
                    ))
                })
                .child(text_action_editor(
                    editing,
                    PowerUserKind::RunShellCommand,
                    &power_user.shell_command,
                    pal,
                ))
                .children(action_sections(editing, target_action.as_ref(), pal)),
            library_scroll,
            pal,
        ))
        // Pinned below the scrolling library so the field and its Add button
        // are never cut off by the card's height cap.
        .child(
            div()
                .flex_none()
                .border_t_1()
                .border_color(pal.border)
                .px_1p5()
                .pb_2()
                .child(shortcut_editor(editing, shortcut_input, pal)),
        )
}

/// The card title: the ring position being edited.
fn slot_title(slot: ActionRingSlot) -> gpui::SharedString {
    match slot {
        ActionRingSlot::Top => tr!("action_ring.top_slot"),
        ActionRingSlot::TopRight => tr!("action_ring.top_right_slot"),
        ActionRingSlot::Right => tr!("action_ring.right_slot"),
        ActionRingSlot::BottomRight => tr!("action_ring.bottom_right_slot"),
        ActionRingSlot::Bottom => tr!("action_ring.bottom_slot"),
        ActionRingSlot::BottomLeft => tr!("action_ring.bottom_left_slot"),
        ActionRingSlot::Left => tr!("action_ring.left_slot"),
        ActionRingSlot::TopLeft => tr!("action_ring.top_left_slot"),
    }
}

/// While the library sets a wheel direction, say so under the title, with
/// the way back to the click action beside it.
fn choosing_banner(editing: &SlotEditing) -> Option<impl IntoElement> {
    let LibraryTarget::Scroll(direction) = editing.target else {
        return None;
    };
    let prompt = match direction {
        RingScrollDirection::Up => tr!("action_ring.scroll_choosing_up"),
        RingScrollDirection::Down => tr!("action_ring.scroll_choosing_down"),
    };
    let done = editing.clone();
    Some(
        h_flex()
            .debug_selector(|| "ring-scroll-choosing".into())
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .text_caption()
                    .text_color(rgb(theme::ACCENT_BLUE))
                    .child(prompt),
            )
            .child(
                Button::new("ring-scroll-done")
                    .compact()
                    .label(tr!("common.done"))
                    .on_click(move |_, _, cx| (done.retarget)(LibraryTarget::Click, cx)),
            ),
    )
}

/// What the wheel does over the slot: nothing, a preset pair, or a custom
/// one, and the two actions of the pair it has.
fn scroll_editor(
    editing: &SlotEditing,
    scroll: Option<&RingScroll>,
    pal: Palette,
) -> impl IntoElement {
    let slot = editing.slot;
    let preset = scroll.and_then(RingScroll::preset);
    let custom = scroll.is_some() && preset.is_none();
    let none = Button::new("ring-scroll-none")
        .compact()
        .outline()
        .label(tr!("action_ring.scroll_none"))
        .selected(scroll.is_none())
        .on_click({
            let editing = editing.clone();
            move |_, _, cx| {
                AppState::apply(cx, |state| state.commit_action_ring_scroll(slot, None));
                (editing.retarget)(LibraryTarget::Click, cx);
            }
        });
    let presets = RingScrollPreset::ALL
        .into_iter()
        .enumerate()
        .map(|(index, choice)| {
            Button::new(("ring-scroll-preset", index))
                .compact()
                .outline()
                .label(rust_i18n::t!(choice.translation_key()))
                .selected(preset == Some(choice))
                .on_click(move |_, _, cx| {
                    AppState::apply(cx, |state| {
                        state.commit_action_ring_scroll(slot, Some(choice.scroll()))
                    });
                })
        });
    // Custom starts from the pair the slot has (or volume) and walks the
    // library through both directions.
    let custom_button = Button::new("ring-scroll-custom")
        .compact()
        .outline()
        .label(tr!("common.custom"))
        .selected(custom)
        .on_click({
            let editing = editing.clone();
            move |_, _, cx| {
                (editing.retarget)(LibraryTarget::Scroll(RingScrollDirection::Up), cx);
            }
        });

    v_flex()
        .debug_selector(|| "ring-scroll-editor".into())
        .gap_1()
        .child(editor_section(tr!("action_ring.scroll_heading"), pal))
        .child(
            div()
                .px_1p5()
                .text_caption()
                .text_color(pal.text_muted)
                .child(tr!("action_ring.scroll_hint")),
        )
        .child(
            h_flex()
                .px_1p5()
                .flex_wrap()
                .gap_1()
                .child(none)
                .children(presets)
                .child(custom_button),
        )
        .when_some(scroll, |editor, pair| {
            editor.child(
                v_flex()
                    .px_1p5()
                    .pt_1()
                    .gap_1()
                    .child(scroll_direction_row(
                        editing,
                        pair,
                        RingScrollDirection::Up,
                        pal,
                    ))
                    .child(scroll_direction_row(
                        editing,
                        pair,
                        RingScrollDirection::Down,
                        pal,
                    )),
            )
        })
}

/// One direction of a pair: its action, and the button that points the
/// library at it.
fn scroll_direction_row(
    editing: &SlotEditing,
    pair: &RingScroll,
    direction: RingScrollDirection,
    pal: Palette,
) -> impl IntoElement {
    let (id, glyph, name) = match direction {
        RingScrollDirection::Up => (
            "ring-scroll-change-up",
            IconName::ChevronUp,
            tr!("action_ring.scroll_up"),
        ),
        RingScrollDirection::Down => (
            "ring-scroll-change-down",
            IconName::ChevronDown,
            tr!("action_ring.scroll_down"),
        ),
    };
    let action = pair.action(direction);
    let choosing = editing.target == LibraryTarget::Scroll(direction);
    let retarget = Rc::clone(&editing.retarget);
    h_flex()
        .items_center()
        .gap_2()
        .child(
            Icon::new(glyph)
                .size_4()
                .flex_none()
                .text_color(pal.text_muted),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_caption().text_color(pal.text_muted).child(name))
                .child(
                    h_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            svg()
                                .path(action_icon_path(action))
                                .size_3()
                                .flex_none()
                                .text_color(pal.text_muted),
                        )
                        .child(div().truncate().child(localized_action_label(action))),
                ),
        )
        .child(
            Button::new(id)
                .compact()
                .label(tr!("action_ring.scroll_change"))
                .selected(choosing)
                .on_click(move |_, _, cx| retarget(LibraryTarget::Scroll(direction), cx)),
        )
}

fn action_rows_scroller(
    content: impl IntoElement,
    scroll: &ScrollHandle,
    pal: Palette,
) -> impl IntoElement {
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .child(
            div()
                .id("ring-action-library")
                .size_full()
                .track_scroll(scroll)
                .overflow_y_scroll()
                .on_scroll(|_, _, cx| cx.stop_propagation())
                .child(content),
        )
        // Fade the cut-off bottom edge into the card so a partly visible row
        // reads as more content below rather than a clipped control.
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(LIBRARY_FADE_H))
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(pal.panel.opacity(0.), 0.),
                    linear_color_stop(pal.panel, 1.),
                )),
        )
        .vertical_scrollbar(scroll)
}

fn icon_editor(
    slot: ActionRingSlot,
    action: &Action,
    current: Option<ActionRingIcon>,
    pal: Palette,
) -> impl IntoElement {
    let default_path = action_icon_path(action);
    let default = icon_button(
        "ring-default-icon",
        default_path,
        tr!("action_ring.use_action_icon"),
        current.is_none(),
        pal,
    )
    .on_click(move |_, _, cx| commit_icon(slot, None, cx));

    v_flex()
        .gap_1()
        .child(editor_section(tr!("action_ring.icon"), pal))
        .child(
            h_flex().flex_wrap().gap_1().child(default).children(
                ActionRingIcon::ALL
                    .into_iter()
                    .enumerate()
                    .map(move |(index, icon)| {
                        icon_button(
                            ("ring-custom-icon", index),
                            icon.asset_path(),
                            rust_i18n::t!(icon.translation_key()),
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
    editing: &SlotEditing,
    input: &Entity<InputState>,
    pal: Palette,
) -> impl IntoElement {
    let submit_input = input.clone();
    let editing = editing.clone();
    v_flex()
        .debug_selector(|| "ring-custom-shortcut".into())
        .gap_1()
        .child(editor_section(tr!("action_ring.custom_shortcut"), pal))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(control_input(input).cleanable(true)),
                )
                .child(
                    Button::new("ring-add-shortcut")
                        .compact()
                        .label(tr!("common.add"))
                        .on_click(move |_, _, cx| {
                            let shortcut = submit_input.read(cx).value().to_string();
                            if let Ok(combo) = shortcut.parse::<KeyCombo>() {
                                editing.commit(Action::CustomShortcut(combo), cx);
                            }
                        }),
                ),
        )
}

fn path_editor(
    editing: &SlotEditing,
    input: &Entity<InputState>,
    pal: Palette,
) -> impl IntoElement {
    let submit_input = input.clone();
    let editing = editing.clone();
    v_flex()
        .gap_1()
        .child(editor_section(
            tr!("action_ring.open_application_or_folder"),
            pal,
        ))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(control_input(input).cleanable(true)),
                )
                .child(
                    Button::new("ring-add-path")
                        .compact()
                        .label(tr!("common.add"))
                        .on_click(move |_, _, cx| {
                            let path = submit_input.read(cx).value().to_string();
                            if let Ok(target) = ApplicationTarget::new(path, "") {
                                editing.commit(Action::OpenApplication(target), cx);
                            }
                        }),
                ),
        )
}

/// Single-field Power User editor shared by TypeText, RunAppleScript and
/// RunShellCommand — each is one text field committed as its own [`Action`]
/// variant, the same shape as [`shortcut_editor`]/[`path_editor`].
fn text_action_editor(
    editing: &SlotEditing,
    kind: PowerUserKind,
    input: &Entity<InputState>,
    pal: Palette,
) -> impl IntoElement {
    let submit_input = input.clone();
    let editing = editing.clone();
    v_flex()
        .gap_1()
        .child(editor_section(tr!(kind.heading_key()), pal))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(control_input(input).cleanable(true)),
                )
                .child(
                    Button::new(("ring-add-power-user", kind as usize))
                        .compact()
                        .label(tr!("common.add"))
                        .on_click(move |_, _, cx| {
                            let text = submit_input.read(cx).value().to_string();
                            if let Some(action) = power_user_action(kind, text) {
                                editing.commit(action, cx);
                            }
                        }),
                ),
        )
}

/// Build the [`Action`] a Power User editor's text commits as, or `None` for
/// empty input, [`PowerUserKind::Workflow`] (no single-field editor here), or
/// [`PowerUserKind::OpenApplication`] (the ring has its own path editor).
fn power_user_action(kind: PowerUserKind, text: String) -> Option<Action> {
    if text.is_empty() {
        return None;
    }
    match kind {
        PowerUserKind::TypeText => Some(Action::TypeText(text)),
        PowerUserKind::RunAppleScript => Some(Action::RunAppleScript(text)),
        PowerUserKind::RunShellCommand => Some(Action::RunShellCommand(text)),
        PowerUserKind::OpenApplication | PowerUserKind::Workflow => None,
    }
}

fn action_sections(
    editing: &SlotEditing,
    current: Option<&Action>,
    pal: Palette,
) -> impl Iterator<Item = impl IntoElement> {
    let editing = editing.clone();
    let mut index = 0usize;
    ring_catalog().into_iter().map(move |(category, actions)| {
        v_flex()
            .child(editor_section(
                rust_i18n::t!(category.translation_key()),
                pal,
            ))
            .children(actions.into_iter().map(|action| {
                let selected = current == Some(&action);
                let action_to_commit = action.clone();
                let editing = editing.clone();
                let label = localized_action_label(&action);
                let icon_path = action_icon_path(&action);
                let row_index = index;
                index += 1;
                MenuRow::new(("ring-action", row_index))
                    .role(Role::MenuItem)
                    .aria_label(label.clone())
                    .selected(selected)
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
                    .on_click(move |_, _, cx| {
                        editing.commit(action_to_commit.clone(), cx);
                    })
            }))
    })
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

fn commit_slot(slot: ActionRingSlot, action: Option<RingAction>, cx: &mut gpui::App) {
    AppState::apply(cx, |state| state.commit_action_ring_slot(slot, action));
}

fn commit_icon(slot: ActionRingSlot, icon: Option<ActionRingIcon>, cx: &mut gpui::App) {
    AppState::apply(cx, |state| state.commit_action_ring_icon(slot, icon));
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, Render, ScrollDelta, ScrollEvent, ScrollHandle, TestAppContext,
        Window, point, px,
    };

    use super::*;

    /// The slot editor as the panel lays it out, editing a slot that holds an
    /// action so the icon grid is shown too.
    struct SlotEditorView {
        application: Entity<InputState>,
        shortcut: Entity<InputState>,
        power_user: PowerUserInputs,
        library_scroll: ScrollHandle,
    }

    impl Render for SlotEditorView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let cut = RingAction::new(Action::Cut).expect("Cut is a ring action");
            let editing = SlotEditing {
                slot: ActionRingSlot::Top,
                target: LibraryTarget::Click,
                retarget: Rc::new(|_, _| {}),
            };
            h_flex().w(px(640.0)).items_start().child(action_library(
                &editing,
                Some(&ActionRingEntry::new(cut)),
                &self.application,
                &self.shortcut,
                &self.power_user,
                &self.library_scroll,
                theme::palette(cx),
            ))
        }
    }

    struct NestedScrollView {
        page_scroll: ScrollHandle,
        sidebar_scroll: ScrollHandle,
    }

    impl Render for NestedScrollView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("page-scroll")
                .w(px(160.0))
                .h(px(120.0))
                .track_scroll(&self.page_scroll)
                .overflow_y_scroll()
                .child(
                    v_flex()
                        .h(px(300.0))
                        .child(
                            v_flex()
                                .w(px(160.0))
                                .h(px(100.0))
                                .child(action_rows_scroller(
                                    div().h(px(240.0)),
                                    &self.sidebar_scroll,
                                    theme::palette(cx),
                                )),
                        )
                        .child(div().h(px(200.0))),
                )
        }
    }

    #[gpui::test]
    fn sidebar_scroll_does_not_move_the_page(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        cx.update(gpui_kit::init);
        let page_scroll = ScrollHandle::new();
        let sidebar_scroll = ScrollHandle::new();
        let (_, cx) = cx.add_window_view({
            let page_scroll = page_scroll.clone();
            let sidebar_scroll = sidebar_scroll.clone();
            move |_, _| NestedScrollView {
                page_scroll,
                sidebar_scroll,
            }
        });
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));

        let overlay_before = cx
            .debug_bounds("scrollbar-overlay")
            .expect("scrollbar overlay should exist");
        assert_eq!(overlay_before.origin, point(px(0.0), px(0.0)));
        assert_eq!(overlay_before.size, gpui::size(px(160.0), px(100.0)));

        // Scroll inside the sidebar scroller
        cx.simulate_event(ScrollEvent {
            position: point(px(80.0), px(50.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-20.0))),
            ..Default::default()
        });
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));

        // Sidebar scrolled, page did not
        assert_eq!(sidebar_scroll.offset().y, px(-20.0));
        assert_eq!(page_scroll.offset().y, px(0.0));

        // Scrollbar overlay stays anchored to the scroller viewport
        let overlay_after_sidebar_scroll = cx
            .debug_bounds("scrollbar-overlay")
            .expect("scrollbar overlay should exist");
        assert_eq!(overlay_after_sidebar_scroll.origin, point(px(0.0), px(0.0)));
        assert_eq!(
            overlay_after_sidebar_scroll.size,
            gpui::size(px(160.0), px(100.0))
        );

        // Click near the bottom of the scrollbar track (x: 155px, y: 80px) to jump
        cx.simulate_click(point(px(155.0), px(80.0)), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));

        // The sidebar scroller should have jumped further down
        assert!(sidebar_scroll.offset().y < px(-20.0));
        assert_eq!(page_scroll.offset().y, px(0.0));

        // Now scroll the outer page from outside the sidebar (e.g. at y: 110px)
        cx.simulate_event(ScrollEvent {
            position: point(px(10.0), px(110.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-15.0))),
            ..Default::default()
        });
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));

        assert_eq!(page_scroll.offset().y, px(-15.0));

        // The sidebar's scrollbar overlay moves together with the sidebar inside the page
        let overlay_after_page_scroll = cx
            .debug_bounds("scrollbar-overlay")
            .expect("scrollbar overlay should exist");
        assert_eq!(overlay_after_page_scroll.origin, point(px(0.0), px(-15.0)));
    }

    #[gpui::test]
    fn custom_shortcut_stays_inside_the_slot_editor(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        cx.update(gpui_kit::init);
        let library_scroll = ScrollHandle::new();
        let (_, cx) = cx.add_window_view({
            let library_scroll = library_scroll.clone();
            move |window, cx| {
                let mut input = || cx.new(|cx| InputState::new(window, cx));
                SlotEditorView {
                    application: input(),
                    shortcut: input(),
                    power_user: PowerUserInputs {
                        type_text: input(),
                        applescript: input(),
                        shell_command: input(),
                    },
                    library_scroll,
                }
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let card = cx
            .debug_bounds("ring-slot-editor")
            .expect("the slot editor renders");
        let shortcut = cx
            .debug_bounds("ring-custom-shortcut")
            .expect("the custom shortcut editor renders");
        assert!(
            shortcut.top() >= card.top() && shortcut.bottom() <= card.bottom(),
            "custom shortcut {shortcut:?} is cut off by the slot editor {card:?}"
        );

        // Scrolling the library leaves the shortcut field where it was.
        cx.simulate_event(ScrollEvent {
            position: card.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-200.0))),
            ..Default::default()
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(library_scroll.offset().y < px(0.0), "the library scrolls");
        assert_eq!(cx.debug_bounds("ring-custom-shortcut"), Some(shortcut));
    }

    #[test]
    fn a_custom_pair_walks_the_library_from_up_to_down_and_back() {
        let up = LibraryTarget::Scroll(RingScrollDirection::Up);
        let down = LibraryTarget::Scroll(RingScrollDirection::Down);
        assert_eq!(up.after_commit(), down);
        assert_eq!(down.after_commit(), LibraryTarget::Click);
        assert_eq!(LibraryTarget::Click.after_commit(), LibraryTarget::Click);
    }

    #[gpui::test]
    fn the_wheel_editor_shows_while_choosing_a_direction(cx: &mut TestAppContext) {
        let _locale = crate::services::i18n::LOCALE_LOCK.lock().unwrap();
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let mut input = || cx.new(|cx| InputState::new(window, cx));
            ChoosingView {
                application: input(),
                shortcut: input(),
                power_user: PowerUserInputs {
                    type_text: input(),
                    applescript: input(),
                    shell_command: input(),
                },
                library_scroll: ScrollHandle::new(),
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let card = cx
            .debug_bounds("ring-slot-editor")
            .expect("the slot editor renders");
        let banner = cx
            .debug_bounds("ring-scroll-choosing")
            .expect("choosing a direction is announced under the title");
        assert!(
            banner.top() >= card.top() && banner.bottom() <= card.bottom(),
            "the banner {banner:?} stays inside the slot editor {card:?}"
        );
        let wheel = cx
            .debug_bounds("ring-scroll-editor")
            .expect("a slot with an action offers the wheel editor");
        assert!(
            wheel.top() > banner.bottom(),
            "the wheel editor scrolls below the header"
        );
    }

    /// The slot editor on a muting slot with a volume pair, choosing the
    /// action for scrolling up.
    struct ChoosingView {
        application: Entity<InputState>,
        shortcut: Entity<InputState>,
        power_user: PowerUserInputs,
        library_scroll: ScrollHandle,
    }

    impl Render for ChoosingView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let mute = RingAction::new(Action::MuteVolume).expect("Mute is a ring action");
            let mut layout = openlogi_core::binding::ActionRingLayout {
                slots: std::collections::BTreeMap::new(),
            };
            layout.set_action(ActionRingSlot::Bottom, Some(mute));
            layout.set_scroll(
                ActionRingSlot::Bottom,
                Some(RingScrollPreset::Volume.scroll()),
            );
            let editing = SlotEditing {
                slot: ActionRingSlot::Bottom,
                target: LibraryTarget::Scroll(RingScrollDirection::Up),
                retarget: Rc::new(|_, _| {}),
            };
            h_flex().w(px(640.0)).items_start().child(action_library(
                &editing,
                layout.slots.get(&ActionRingSlot::Bottom),
                &self.application,
                &self.shortcut,
                &self.power_user,
                &self.library_scroll,
                theme::palette(cx),
            ))
        }
    }

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

    #[test]
    fn power_user_action_builds_the_matching_variant() {
        assert_eq!(
            power_user_action(PowerUserKind::TypeText, "hello".to_string()),
            Some(Action::TypeText("hello".to_string()))
        );
        assert_eq!(
            power_user_action(PowerUserKind::RunAppleScript, "beep".to_string()),
            Some(Action::RunAppleScript("beep".to_string()))
        );
        assert_eq!(
            power_user_action(PowerUserKind::RunShellCommand, "echo hi".to_string()),
            Some(Action::RunShellCommand("echo hi".to_string()))
        );
    }

    #[test]
    fn power_user_action_rejects_empty_text_and_workflow() {
        assert_eq!(
            power_user_action(PowerUserKind::TypeText, String::new()),
            None
        );
        assert_eq!(
            power_user_action(PowerUserKind::Workflow, "irrelevant".to_string()),
            None
        );
    }
}
