//! Eight-slot Actions Ring editor for the active device.

mod action_icons;
mod editor;

use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, Window, div, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Icon, IconName, Selectable as _, button::Button, h_flex, input::InputState, tooltip::Tooltip,
    v_flex,
};
use openlogi_core::binding::{
    ActionRingConfig, ActionRingEntry, ActionRingIcon, ActionRingLayout, ActionRingSlot,
};
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use rust_i18n::t;

use self::action_icons::action_icon_path;
use self::editor::action_library;
use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::action::localized_action_label;
use crate::ui::theme::{self, Palette, Typography as _};

/// Stateful Actions Ring editor. Ring configuration itself lives in
/// [`AppState`]; this entity owns selection and editor input state.
pub struct ActionRingPanel {
    focus_handle: FocusHandle,
    selected_slot: ActionRingSlot,
    application_input: Option<Entity<InputState>>,
    shortcut_input: Option<Entity<InputState>>,
    library_scroll: ScrollHandle,
    #[expect(dead_code, reason = "held to keep the AppState subscription alive")]
    state_obs: Subscription,
}

impl ActionRingPanel {
    /// Create the editor and repaint it after any config/device change.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.subscribe(&AppState::global(cx), |_, _, event: &StateEvent, cx| {
            let relevant = match event {
                StateEvent::InventoryChanged | StateEvent::DeviceSelected(_) => true,
                StateEvent::BindingsChanged(key) => AppState::try_read(cx)
                    .and_then(AppState::current_record)
                    .is_some_and(|record| record.device_key() == *key),
                _ => false,
            };
            if relevant {
                cx.notify();
            }
        });
        Self {
            focus_handle: cx.focus_handle(),
            selected_slot: ActionRingSlot::Top,
            application_input: None,
            shortcut_input: None,
            library_scroll: ScrollHandle::new(),
            state_obs,
        }
    }
}

impl Focusable for ActionRingPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ActionRingPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let (ring, layout) = action_ring_editor_state(cx);
        let haptics_supported = current_device_supports_haptics(cx);
        let application_input = editor_input(
            &mut self.application_input,
            t!("action_ring.application_folder_path_or_url"),
            window,
            cx,
        );
        let shortcut_input = editor_input(
            &mut self.shortcut_input,
            t!("action_ring.shortcut_e_g_cmd_plus_shift_plus_p"),
            window,
            cx,
        );
        let view = cx.entity();

        v_flex()
            .w_full()
            .gap_4()
            .tab_group()
            .track_focus(&self.focus_handle)
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_subheading()
                            .child(t!("action_ring.actions_ring")),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(t!("action_ring.action_ring_description")),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .justify_center()
                    .gap_4()
                    .child(ring_preview(&layout, self.selected_slot, &view, pal))
                    .child(action_library(
                        self.selected_slot,
                        layout.slots.get(&self.selected_slot),
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
                            .child(div().text_body().child(t!("action_ring.actions_ring")))
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(pal.text_muted)
                                    .child(t!("action_ring.open_at_the_current_cursor_position")),
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
                                .child(div().text_body().child(t!("action_ring.haptic_feedback")))
                                .child(
                                    div()
                                        .text_caption()
                                        .text_color(pal.text_muted)
                                        .child(t!("action_ring.action_ring_haptic_description")),
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

fn action_ring_editor_state(cx: &Context<ActionRingPanel>) -> (ActionRingConfig, ActionRingLayout) {
    AppState::try_read(cx).map_or_else(
        || {
            let ring = ActionRingConfig::default();
            let layout = ring.default.clone();
            (ring, layout)
        },
        |state| {
            let ring = state.current_action_ring();
            let layout = state.current_action_ring_layout();
            (ring, layout)
        },
    )
}

fn editor_input(
    state: &mut Option<Entity<InputState>>,
    placeholder: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<ActionRingPanel>,
) -> Entity<InputState> {
    let placeholder = placeholder.into();
    let state = state
        .get_or_insert_with(|| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.clone()))
        })
        .clone();
    // Callers pass a per-render `rust_i18n::t!` string, so a cached input follows a live
    // language switch instead of keeping the placeholder it was built with.
    crate::ui::components::localize_placeholder(&state, placeholder, window, cx);
    state
}

fn current_device_supports_haptics(cx: &Context<ActionRingPanel>) -> bool {
    AppState::try_read(cx).is_some_and(|state| {
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
        .label(if enabled {
            rust_i18n::t!("common.on")
        } else {
            rust_i18n::t!("common.off")
        })
        .selected(enabled)
        .on_click(move |_, _, cx| {
            AppState::update(cx, |state, cx| {
                let key = state.current_record().map(DeviceRecord::device_key);
                commit(state, !enabled);
                if let Some(key) = key {
                    cx.emit(StateEvent::BindingsChanged(key));
                }
            });
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
                .bg(pal.panel),
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
                .bg(pal.muted)
                .text_color(pal.text_muted)
                .child(svg().path(RING_CANCEL_ICON).size(px(20.0)).flex_none()),
        )
        .children(ActionRingSlot::ALL.into_iter().map(|slot| {
            slot_button(
                slot,
                layout.slots.get(&slot),
                selected_slot == slot,
                view,
                pal,
            )
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
        || rust_i18n::t!("action_ring.empty_slot").to_string(),
        |entry| localized_action_label(entry.action()).to_string(),
    );
    let icon_path = entry.map(|entry| {
        entry.custom_icon().map_or_else(
            || action_icon_path(entry.action()),
            ActionRingIcon::asset_path,
        )
    });
    let accessible_label = label.clone();
    let selected_view = view.clone();

    BaseButton::new(("action-ring-slot", index))
        .selected(selected)
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
            pal.control
        })
        .text_color(if selected {
            pal.text_primary
        } else {
            pal.text_muted
        })
        .cursor_pointer()
        .accessibility_label(accessible_label)
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
                pal.control_hover
            })
        })
        .focus_visible(move |button| {
            button
                .border_color(rgb(theme::ACCENT_BLUE))
                .bg(if selected {
                    theme::accent_tint_hover()
                } else {
                    pal.control_hover
                })
        })
        .on_click(move |_, _, cx| {
            selected_view.update(cx, |panel, cx| {
                panel.selected_slot = slot;
                cx.notify();
            });
        })
}
