//! Eight-slot Actions Ring editor for the active device.

mod action_icons;
mod editor;

use gpui::{
    AppContext as _, BorrowAppContext as _, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, Render, Role, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled, Subscription, Window, div, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{
    Icon, IconName, Selectable as _, button::Button, h_flex, input::InputState, tooltip::Tooltip,
    v_flex,
};
use openlogi_core::binding::{ActionRingEntry, ActionRingIcon, ActionRingLayout, ActionRingSlot};

use self::action_icons::action_icon_path;
use self::editor::action_library;
use crate::state::AppState;
use crate::ui::theme::{self, Palette, Typography as _};

/// Stateful Actions Ring editor. Ring configuration itself lives in
/// [`AppState`]; this entity owns selection and editor input state.
pub struct ActionRingPanel {
    selected_slot: ActionRingSlot,
    application_input: Option<Entity<InputState>>,
    shortcut_input: Option<Entity<InputState>>,
    library_scroll: ScrollHandle,
    #[allow(dead_code, reason = "held to keep the AppState observer alive")]
    state_obs: Subscription,
}

impl ActionRingPanel {
    /// Create the editor and repaint it after any config/device change.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            selected_slot: ActionRingSlot::Top,
            application_input: None,
            shortcut_input: None,
            library_scroll: ScrollHandle::new(),
            state_obs: cx.observe_global::<AppState>(|_, cx| cx.notify()),
        }
    }
}

impl Render for ActionRingPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let ring = cx
            .try_global::<AppState>()
            .map(AppState::current_action_ring)
            .unwrap_or_default();
        let haptics_supported = current_device_supports_haptics(cx);
        let application_input = editor_input(
            &mut self.application_input,
            tr!("Application, folder path, or URL"),
            window,
            cx,
        );
        let shortcut_input = editor_input(
            &mut self.shortcut_input,
            tr!("Shortcut, e.g. Cmd+Shift+P"),
            window,
            cx,
        );
        let view = cx.entity();

        v_flex()
            .w_full()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_subheading().child(tr!("Actions Ring")))
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("Configure the eight actions shown around the cursor.")),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .justify_center()
                    .gap_4()
                    .child(ring_preview(&ring.default, self.selected_slot, &view, pal))
                    .child(action_library(
                        self.selected_slot,
                        ring.default.slots.get(&self.selected_slot),
                        &application_input,
                        &shortcut_input,
                        &self.library_scroll,
                        pal,
                    )),
            )
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .child(div().text_body().child(tr!("Actions Ring")))
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(tr!("Open at the current cursor position.")),
                            ),
                    )
                    .child(toggle_button(
                        "ring-enabled",
                        ring.enabled,
                        |state, enabled| {
                            state.commit_action_ring_enabled(enabled);
                        },
                    )),
            )
            .when(haptics_supported, |panel| {
                panel.child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            v_flex()
                                .child(div().text_body().child(tr!("Haptic feedback")))
                                .child(
                                    div()
                                        .text_caption()
                                        .text_color(pal.text_muted)
                                        .child(tr!("Play feedback when hovering and activating.")),
                                ),
                        )
                        .child(toggle_button(
                            "ring-haptics",
                            ring.haptics,
                            |state, enabled| {
                                state.commit_action_ring_haptics(enabled);
                            },
                        )),
                )
            })
    }
}

fn editor_input(
    state: &mut Option<Entity<InputState>>,
    placeholder: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<ActionRingPanel>,
) -> Entity<InputState> {
    state
        .get_or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder)))
        .clone()
}

fn current_device_supports_haptics(cx: &Context<ActionRingPanel>) -> bool {
    cx.try_global::<AppState>().is_some_and(|state| {
        state.current_record().is_some_and(|record| {
            record
                .capabilities
                .unwrap_or_else(|| {
                    openlogi_core::device::Capabilities::presumed_from_kind(record.kind)
                })
                .haptic_feedback
        })
    })
}

fn toggle_button(
    id: &'static str,
    enabled: bool,
    commit: impl Fn(&mut AppState, bool) + 'static,
) -> Button {
    Button::new(id)
        .compact()
        .label(if enabled { tr!("On") } else { tr!("Off") })
        .selected(enabled)
        .on_click(move |_, _, cx| {
            cx.update_global::<AppState, _>(|state, _| commit(state, !enabled));
            cx.refresh_windows();
        })
}

const PREVIEW_SIZE: f32 = 320.0;
const PREVIEW_RADIUS: f32 = 106.0;
const PREVIEW_SLOT_SIZE: f32 = 50.0;

fn ring_preview(
    layout: &ActionRingLayout,
    selected_slot: ActionRingSlot,
    view: &Entity<ActionRingPanel>,
    pal: Palette,
) -> impl IntoElement {
    div()
        .relative()
        .flex_none()
        .size(px(PREVIEW_SIZE))
        .child(
            div()
                .absolute()
                .left(px(24.0))
                .top(px(24.0))
                .size(px(PREVIEW_SIZE - 48.0))
                .rounded_full()
                .border_1()
                .border_color(pal.border)
                .bg(pal.surface),
        )
        .child(
            div()
                .absolute()
                .left(px(PREVIEW_SIZE / 2.0 - 24.0))
                .top(px(PREVIEW_SIZE / 2.0 - 24.0))
                .size(px(48.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(pal.surface_hover)
                .text_color(pal.text_muted)
                .child("×"),
        )
        .children(ActionRingSlot::ALL.into_iter().map(|slot| {
            slot_button(
                slot,
                layout.slots.get(&slot),
                selected_slot == slot,
                view,
                pal,
            )
            .into_any_element()
        }))
}

fn slot_button(
    slot: ActionRingSlot,
    entry: Option<&ActionRingEntry>,
    selected: bool,
    view: &Entity<ActionRingPanel>,
    pal: Palette,
) -> impl IntoElement {
    let index = slot.index();
    let (left, top) = slot.placement(PREVIEW_SIZE, PREVIEW_RADIUS, PREVIEW_SLOT_SIZE);
    let label = entry.map_or_else(
        || tr!("Empty slot").to_string(),
        |entry| rust_i18n::t!(entry.action().label()).into_owned(),
    );
    let icon_path = entry.map(|entry| {
        entry.custom_icon().map_or_else(
            || action_icon_path(entry.action()),
            ActionRingIcon::asset_path,
        )
    });
    let accessible_label = label.clone();
    let selected_view = view.clone();

    div()
        .id(("action-ring-slot", index))
        .absolute()
        .left(px(left))
        .top(px(top))
        .size(px(PREVIEW_SLOT_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(if selected {
            rgb(theme::ACCENT_BLUE).into()
        } else {
            pal.border
        })
        .bg(if selected {
            theme::accent_tint()
        } else {
            pal.surface_hover
        })
        .text_color(if selected {
            pal.text_primary
        } else {
            pal.text_muted
        })
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(accessible_label)
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .when_some(icon_path, |button, path| {
            button.child(svg().path(path).size(px(20.0)).text_color(if selected {
                pal.text_primary
            } else {
                pal.text_muted
            }))
        })
        .when(icon_path.is_none(), |button| {
            button.child(Icon::new(IconName::Plus).size_4())
        })
        .hover(move |button| {
            button.bg(if selected {
                theme::accent_tint_hover()
            } else {
                pal.surface_hover
            })
        })
        .on_click(move |_, _, cx| {
            selected_view.update(cx, |panel, cx| {
                panel.selected_slot = slot;
                cx.notify();
            });
        })
}
