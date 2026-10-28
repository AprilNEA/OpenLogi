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
    Icon, IconName, Selectable as _,
    button::Button,
    h_flex,
    input::InputState,
    select::{SelectEvent, SelectItem, SelectState},
    tooltip::Tooltip,
    v_flex,
};
use openlogi_core::binding::{
    ActionRingConfig, ActionRingEntry, ActionRingIcon, ActionRingLayout, ActionRingSlot,
    ActionRingTheme,
};
use openlogi_ui::action_icons::RING_CANCEL_ICON;

use self::action_icons::action_icon_path;
use self::editor::{PowerUserInputs, action_library};
use crate::features::keyboard::editors::{PowerUserKind, text_editor_placeholder};
use crate::state::{AppState, StateEvent, StateEvents};
use crate::ui::action::localized_action_label;
use crate::ui::components::control_select;
use crate::ui::theme::{self, Palette, Typography as _};

/// Stateful Actions Ring editor. Ring configuration itself lives in
/// [`AppState`]; this entity owns selection and editor input state.
pub struct ActionRingPanel {
    focus_handle: FocusHandle,
    selected_slot: ActionRingSlot,
    application_input: Option<Entity<InputState>>,
    shortcut_input: Option<Entity<InputState>>,
    type_text_input: Option<Entity<InputState>>,
    applescript_input: Option<Entity<InputState>>,
    shell_command_input: Option<Entity<InputState>>,
    theme_select: Option<Entity<SelectState<Vec<ThemeOption>>>>,
    library_scroll: ScrollHandle,
    #[expect(dead_code, reason = "held to keep the AppState subscription alive")]
    state_obs: Subscription,
}

impl ActionRingPanel {
    /// Create the editor and repaint it after any config/device change.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::BindingsChanged(_)));
        Self {
            focus_handle: cx.focus_handle(),
            selected_slot: ActionRingSlot::Top,
            application_input: None,
            shortcut_input: None,
            type_text_input: None,
            applescript_input: None,
            shell_command_input: None,
            theme_select: None,
            library_scroll: ScrollHandle::new(),
            state_obs,
        }
    }

    /// Get-or-create the three Power User text inputs, split out of `render`
    /// to keep it under clippy's line budget.
    fn power_user_inputs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PowerUserInputs {
        PowerUserInputs {
            type_text: editor_input(
                &mut self.type_text_input,
                text_editor_placeholder(PowerUserKind::TypeText),
                window,
                cx,
            ),
            applescript: editor_input(
                &mut self.applescript_input,
                text_editor_placeholder(PowerUserKind::RunAppleScript),
                window,
                cx,
            ),
            shell_command: editor_input(
                &mut self.shell_command_input,
                text_editor_placeholder(PowerUserKind::RunShellCommand),
                window,
                cx,
            ),
        }
    }

    /// Get-or-create the theme picker, pointed at `current`: the device the
    /// panel shows can change under a picker that was built for another one.
    fn theme_select(
        &mut self,
        current: ActionRingTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<SelectState<Vec<ThemeOption>>> {
        let state = self
            .theme_select
            .get_or_insert_with(|| {
                let options = ActionRingTheme::ALL.map(ThemeOption).to_vec();
                let state = cx.new(|cx| SelectState::new(options, None, window, cx));
                cx.subscribe_in(&state, window, |_, state, event, _, cx| {
                    let SelectEvent::Confirm(_) = event;
                    if let Some(&theme) = state.read(cx).selected_value() {
                        AppState::apply(cx, |app| app.commit_action_ring_theme(theme));
                    }
                })
                .detach();
                state
            })
            .clone();
        if state.read(cx).selected_value() != Some(&current) {
            state.update(cx, |state, cx| {
                state.set_selected_value(&current, window, cx);
            });
        }
        state
    }
}

/// One entry of the theme picker.
#[derive(Clone)]
struct ThemeOption(ActionRingTheme);

impl SelectItem for ThemeOption {
    type Value = ActionRingTheme;

    fn title(&self) -> SharedString {
        match self.0 {
            ActionRingTheme::Classic => tr!("action_ring.theme_classic"),
            ActionRingTheme::InMotion => tr!("action_ring.theme_in_motion"),
        }
    }

    fn value(&self) -> &Self::Value {
        &self.0
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
            tr!("action_ring.application_folder_path_or_url"),
            window,
            cx,
        );
        let shortcut_input = editor_input(
            &mut self.shortcut_input,
            tr!("action_ring.shortcut_e_g_cmd_plus_shift_plus_p"),
            window,
            cx,
        );
        let power_user = self.power_user_inputs(window, cx);
        let theme_select = self.theme_select(ring.theme, window, cx);
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
                            .child(tr!("action_ring.actions_ring")),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("action_ring.action_ring_description")),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .justify_center()
                    .gap_4()
                    .child(ring_preview(
                        ring.theme,
                        &layout,
                        self.selected_slot,
                        &view,
                        pal,
                    ))
                    .child(action_library(
                        self.selected_slot,
                        layout.slots.get(&self.selected_slot),
                        &application_input,
                        &shortcut_input,
                        &power_user,
                        &self.library_scroll,
                        pal,
                    )),
            )
            .child(enabled_row(ring.enabled, pal))
            .child(theme_row(&theme_select, pal))
            .when(haptics_supported, |panel| {
                panel.child(haptics_row(ring.haptics, pal))
            })
    }
}

/// The ring on/off toggle row.
fn enabled_row(enabled: bool, pal: Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .child(div().text_body().child(tr!("action_ring.actions_ring")))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("action_ring.open_at_the_current_cursor_position")),
                ),
        )
        .child(toggle_button(
            "ring-enabled",
            enabled,
            AppState::commit_action_ring_enabled,
        ))
}

/// The overlay theme picker. A select rather than a segment, so adding a
/// theme adds a row, not a button the row must find room for.
fn theme_row(select: &Entity<SelectState<Vec<ThemeOption>>>, pal: Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .child(div().text_body().child(tr!("action_ring.theme")))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("action_ring.theme_description")),
                ),
        )
        .child(div().w(px(180.0)).child(control_select(select)))
}

/// The haptic-feedback toggle row, shown only when the device supports it.
fn haptics_row(haptics: bool, pal: Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .child(div().text_body().child(tr!("action_ring.haptic_feedback")))
                .child(
                    div()
                        .text_caption()
                        .text_color(pal.text_muted)
                        .child(tr!("action_ring.action_ring_haptic_description")),
                ),
        )
        .child(toggle_button(
            "ring-haptics",
            haptics,
            AppState::commit_action_ring_haptics,
        ))
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
    // Callers pass a per-render `tr!` string, so a cached input follows a live
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
    commit: impl Fn(&mut AppState, bool) -> StateEvents + 'static,
) -> Button {
    Button::new(id)
        .compact()
        .label(if enabled {
            tr!("common.on")
        } else {
            tr!("common.off")
        })
        .selected(enabled)
        .on_click(move |_, _, cx| AppState::apply(cx, |state| commit(state, !enabled)))
}

const PREVIEW_SIZE: f32 = 320.0;
const PREVIEW_RADIUS: f32 = 106.0;
const PREVIEW_SLOT_SIZE: f32 = 50.0;
/// The In Motion pill, at the preview's scale of the overlay's 132×34.
const PREVIEW_PILL_W: f32 = 96.0;
const PREVIEW_PILL_H: f32 = 28.0;

/// How the preview draws a slot: the theme's shape, at the preview's scale.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotShape {
    /// Classic: a round icon button, the caption in a tooltip.
    Round,
    /// In Motion: a pill carrying icon and caption.
    Pill,
}

impl SlotShape {
    const fn of(theme: ActionRingTheme) -> Self {
        match theme {
            ActionRingTheme::Classic => Self::Round,
            ActionRingTheme::InMotion => Self::Pill,
        }
    }

    const fn size(self) -> (f32, f32) {
        match self {
            Self::Round => (PREVIEW_SLOT_SIZE, PREVIEW_SLOT_SIZE),
            Self::Pill => (PREVIEW_PILL_W, PREVIEW_PILL_H),
        }
    }

    /// Top-left of `slot`, with its centre on the preview's slot circle.
    fn origin(self, slot: ActionRingSlot) -> (f32, f32) {
        let (x, y) = slot.unit_offset();
        let (width, height) = self.size();
        (
            PREVIEW_SIZE / 2.0 + x * PREVIEW_RADIUS - width / 2.0,
            PREVIEW_SIZE / 2.0 + y * PREVIEW_RADIUS - height / 2.0,
        )
    }

    const fn debug_selector(self) -> &'static str {
        match self {
            Self::Round => "action-ring-preview-round",
            Self::Pill => "action-ring-preview-pill",
        }
    }
}

/// The ring as `theme` draws it, still: no motion, and the app's palette in
/// place of the overlay's dark panel, so it reads as a preview and not a
/// second ring on the desktop.
fn ring_preview(
    theme: ActionRingTheme,
    layout: &ActionRingLayout,
    selected_slot: ActionRingSlot,
    view: &Entity<ActionRingPanel>,
    pal: Palette,
) -> impl IntoElement {
    let shape = SlotShape::of(theme);
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
                shape,
                slot,
                layout.slots.get(&slot),
                selected_slot == slot,
                view,
                pal,
            )
        }))
}

fn slot_button(
    shape: SlotShape,
    slot: ActionRingSlot,
    entry: Option<&ActionRingEntry>,
    selected: bool,
    view: &Entity<ActionRingPanel>,
    pal: Palette,
) -> impl IntoElement {
    let index = slot.index();
    let (left, top) = shape.origin(slot);
    let (width, height) = shape.size();
    let label = entry.map_or_else(
        || tr!("action_ring.empty_slot").to_string(),
        |entry| localized_action_label(entry.action()).to_string(),
    );
    let icon_path = entry.map(|entry| {
        entry.custom_icon().map_or_else(
            || action_icon_path(entry.action()),
            ActionRingIcon::asset_path,
        )
    });
    let accessible_label = label.clone();
    let caption = SharedString::from(label.clone());
    let selected_view = view.clone();
    let ink = if selected {
        pal.text_primary
    } else {
        pal.text_muted
    };

    BaseButton::new(("action-ring-slot", index))
        .debug_selector(move || shape.debug_selector().to_string())
        .selected(selected)
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .when(shape == SlotShape::Pill, |button| button.gap(px(6.0)).px(px(10.0)))
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
        .text_color(ink)
        .cursor_pointer()
        .accessibility_label(accessible_label)
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .when_some(icon_path, |button, path| {
            let glyph = if shape == SlotShape::Pill { 14.0 } else { 20.0 };
            button.child(svg().path(path).size(px(glyph)).flex_none().text_color(ink))
        })
        .when(icon_path.is_none(), |button| {
            button.child(Icon::new(IconName::Plus).size_4())
        })
        // The pill carries its caption, as the overlay's does; the round slot
        // leaves it to the tooltip.
        .when(shape == SlotShape::Pill, |button| {
            button.child(
                div()
                    .w(px(PREVIEW_PILL_W - 36.0))
                    .text_xs()
                    .truncate()
                    .child(caption),
            )
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

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, Window};
    use openlogi_core::config::Config;

    use super::*;
    use crate::services::assets::AssetResolver;
    use crate::state::Sources;

    /// A state with no device: enough for the panel entity the preview's slot
    /// buttons report their selection to.
    fn install_app_state(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let resolver = AssetResolver::new();
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::new(Sources::in_memory(Config::ephemeral(), &resolver, commands))
            });
            AppState::set_global(state, cx);
        });
    }

    /// The preview on its own, for a theme: the panel needs a device-bearing
    /// `AppState` to show In Motion, and the preview is what the theme changes.
    struct PreviewOnly {
        theme: ActionRingTheme,
        panel: Entity<ActionRingPanel>,
    }

    impl Render for PreviewOnly {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let layout = ActionRingLayout::default();
            ring_preview(
                self.theme,
                &layout,
                ActionRingSlot::Top,
                &self.panel,
                theme::palette(cx),
            )
            .into_any_element()
        }
    }

    #[gpui::test]
    fn the_preview_follows_the_theme(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        install_app_state(cx);
        for (theme, present, absent) in [
            (
                ActionRingTheme::Classic,
                "action-ring-preview-round",
                "action-ring-preview-pill",
            ),
            (
                ActionRingTheme::InMotion,
                "action-ring-preview-pill",
                "action-ring-preview-round",
            ),
        ] {
            let (_, cx) = cx.add_window_view(move |_, cx| PreviewOnly {
                theme,
                panel: cx.new(ActionRingPanel::new),
            });
            cx.run_until_parked();
            cx.update(|window, cx| drop(window.draw(cx)));
            let slot = cx
                .debug_bounds(present)
                .unwrap_or_else(|| panic!("{theme:?} draws {present}"));
            let (width, height) = SlotShape::of(theme).size();
            assert_eq!(slot.size, gpui::size(px(width), px(height)), "{theme:?}");
            assert!(
                cx.debug_bounds(absent).is_none(),
                "{theme:?} draws no {absent}"
            );
        }
    }
}
