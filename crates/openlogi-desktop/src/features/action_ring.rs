//! Eight-slot Actions Ring editor for the active device.

mod action_icons;
mod editor;

use gpui::{
    App, AppContext as _, Context, Div, Entity, FocusHandle, Focusable, Hsla, InteractiveElement,
    IntoElement, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px, rgb, svg,
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
use openlogi_ui::color;
use openlogi_ui::ring_style::{EMPTY_SLOT_OUTLINE, classic, in_motion, obsidian};

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
            ActionRingTheme::Obsidian => tr!("action_ring.theme_obsidian"),
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
                    .child(ring_preview(ring.theme, &layout, self.selected_slot, &view))
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
/// What the ring floats over in the preview: a mid grey, so the classic dark
/// disc, In Motion's dark and white pills and Obsidian's glass cards all read
/// against it, as they do over a desktop.
const PREVIEW_STAGE: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.32,
    a: 1.0,
};

/// How the preview draws a slot: the theme's shape, at the preview's scale.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotShape {
    /// Classic: a round icon button.
    Round,
    /// In Motion: a pill carrying icon and caption.
    Pill,
    /// Obsidian: a glass card, icon above caption.
    Card,
}

impl SlotShape {
    const fn of(theme: ActionRingTheme) -> Self {
        match theme {
            ActionRingTheme::Classic => Self::Round,
            ActionRingTheme::InMotion => Self::Pill,
            ActionRingTheme::Obsidian => Self::Card,
        }
    }

    /// The theme's panel shrunk to the preview: every size the ring style
    /// gives is multiplied by this.
    const fn scale(self) -> f32 {
        match self {
            Self::Round => PREVIEW_SIZE / classic::PANEL_SIZE,
            Self::Pill => PREVIEW_SIZE / in_motion::DESIGN_EDGE,
            Self::Card => PREVIEW_SIZE / obsidian::DESIGN_EDGE,
        }
    }

    /// Distance from the preview's centre to each slot's centre.
    const fn radius(self) -> f32 {
        match self {
            Self::Round => classic::RADIUS * self.scale(),
            Self::Pill => in_motion::RADIUS * self.scale(),
            Self::Card => obsidian::RADIUS * self.scale(),
        }
    }

    const fn size(self) -> (f32, f32) {
        match self {
            Self::Round => (
                classic::SLOT_SIZE * self.scale(),
                classic::SLOT_SIZE * self.scale(),
            ),
            Self::Pill => (
                in_motion::PILL_W * self.scale(),
                in_motion::PILL_H * self.scale(),
            ),
            Self::Card => (
                obsidian::CARD_W * self.scale(),
                obsidian::CARD_H * self.scale(),
            ),
        }
    }

    /// Top-left of `slot`, with its centre on the slot circle.
    fn origin(self, slot: ActionRingSlot) -> (f32, f32) {
        let (x, y) = slot.unit_offset();
        let (width, height) = self.size();
        (
            PREVIEW_SIZE / 2.0 + x * self.radius() - width / 2.0,
            PREVIEW_SIZE / 2.0 + y * self.radius() - height / 2.0,
        )
    }

    const fn debug_selector(self) -> &'static str {
        match self {
            Self::Round => "action-ring-preview-round",
            Self::Pill => "action-ring-preview-pill",
            Self::Card => "action-ring-preview-card",
        }
    }

    /// The slot's caption, when the shape carries one beside its glyph.
    const fn carries_caption(self) -> bool {
        !matches!(self, Self::Round)
    }
}

/// A still frame of the ring as `theme` draws it, with the selected slot in
/// its hovered state. Sizes, colours and fills come from
/// [`openlogi_ui::ring_style`], which the overlay draws from too; only the
/// motion is missing. The slots are the editor's slot picker as well.
fn ring_preview(
    theme: ActionRingTheme,
    layout: &ActionRingLayout,
    selected_slot: ActionRingSlot,
    view: &Entity<ActionRingPanel>,
) -> impl IntoElement {
    let shape = SlotShape::of(theme);
    let selected_caption = layout
        .slots
        .get(&selected_slot)
        .map(|entry| localized_action_label(entry.action()));
    let backdrop = match shape {
        SlotShape::Round => classic_backdrop(selected_caption).into_any_element(),
        SlotShape::Pill => in_motion_backdrop().into_any_element(),
        SlotShape::Card => obsidian_backdrop().into_any_element(),
    };
    div()
        .relative()
        .flex_none()
        .size(px(PREVIEW_SIZE))
        .rounded_lg()
        .overflow_hidden()
        .bg(PREVIEW_STAGE)
        .child(backdrop)
        .children(ActionRingSlot::ALL.into_iter().map(|slot| {
            slot_button(
                shape,
                slot,
                layout.slots.get(&slot),
                selected_slot == slot,
                view,
            )
        }))
}

/// A centred circle of `diameter`, for the backdrops.
fn centred_circle(diameter: f32) -> Div {
    div()
        .absolute()
        .left(px(PREVIEW_SIZE / 2.0 - diameter / 2.0))
        .top(px(PREVIEW_SIZE / 2.0 - diameter / 2.0))
        .size(px(diameter))
        .rounded_full()
}

/// The classic dark disc, its cancel button, and under it the selected
/// slot's label, where the ring shows the hovered one.
fn classic_backdrop(caption: Option<SharedString>) -> Div {
    let scale = SlotShape::Round.scale();
    div()
        .absolute()
        .size_full()
        .child(
            centred_circle((classic::PANEL_SIZE - 2.0 * classic::DISC_INSET) * scale)
                .bg(classic::PANEL)
                .shadow_lg(),
        )
        .child(
            centred_circle(classic::CANCEL_SIZE * scale)
                .flex()
                .items_center()
                .justify_center()
                .bg(classic::CANCEL_RESTING)
                .text_color(classic::CANCEL_GLYPH)
                .child(
                    svg()
                        .path(RING_CANCEL_ICON)
                        .size(px(classic::CANCEL_GLYPH_SIZE * scale))
                        .flex_none(),
                ),
        )
        .when_some(caption, |backdrop, caption| {
            let width = classic::LABEL_WIDTH * scale;
            backdrop.child(
                div()
                    .absolute()
                    .left(px(PREVIEW_SIZE / 2.0 - width / 2.0))
                    .top(px(PREVIEW_SIZE / 2.0 + classic::LABEL_OFFSET * scale))
                    .w(px(width))
                    .text_center()
                    .text_sm()
                    .text_color(classic::LABEL)
                    .truncate()
                    .child(caption),
            )
        })
}

/// In Motion's hub glow, HUD tracks and hub, still: the glow and tracks at
/// the middle of their breath, the arcs that travel them left out.
fn in_motion_backdrop() -> Div {
    let scale = SlotShape::Pill.scale();
    let track = in_motion::RADIUS * scale;
    div()
        .absolute()
        .size_full()
        .children(in_motion::HUB_GLOW.map(|(radius, alpha, swing)| {
            centred_circle(2.0 * radius * scale).bg(color::accent().alpha(alpha + swing / 2.0))
        }))
        .children(
            [in_motion::TRACK_INNER, 1.0, in_motion::TRACK_OUTER].map(|fraction| {
                centred_circle(2.0 * track * fraction)
                    .border_1()
                    .border_color(in_motion::track(0.5))
            }),
        )
        .child(
            centred_circle(in_motion::HUB * scale)
                .flex()
                .items_center()
                .justify_center()
                .bg(in_motion::hub_fill())
                .text_color(in_motion::CANCEL_GLYPH)
                .child(
                    svg()
                        .path(RING_CANCEL_ICON)
                        .size(px(in_motion::HUB_GLYPH * scale))
                        .flex_none(),
                ),
        )
}

/// Obsidian's dial at rest: the soft disc, the two orbit lines and the hub.
/// The ticks, the sector and the dot are left out: they are hover state, and
/// a still frame would spend 48 elements on hairlines.
fn obsidian_backdrop() -> Div {
    let scale = SlotShape::Card.scale();
    div()
        .absolute()
        .size_full()
        .child(
            centred_circle(2.0 * obsidian::AMBIENT * scale)
                .bg(obsidian::ambient_fill())
                .shadow(obsidian::ambient_shadow()),
        )
        .children(
            [
                (obsidian::ORBIT_OUTER, obsidian::orbit_outer()),
                (obsidian::ORBIT_INNER, obsidian::orbit_inner()),
            ]
            .map(|(radius, color)| {
                centred_circle(2.0 * radius * scale)
                    .border_1()
                    .border_color(color)
            }),
        )
        .child(
            centred_circle(obsidian::HUB * scale)
                .flex()
                .items_center()
                .justify_center()
                .bg(obsidian::hub_fill())
                .border_1()
                .border_color(obsidian::hub_border())
                .shadow(obsidian::hub_shadow())
                .text_color(obsidian::signal())
                .child(
                    svg()
                        .path(RING_CANCEL_ICON)
                        .size(px(obsidian::HUB_GLYPH * scale))
                        .flex_none(),
                ),
        )
}

/// A slot's caption colour, its glyph colour and its glyph size, for `shape`
/// with the slot selected or not.
///
/// The card's glyph takes the signal colour when the card is pressed and a
/// muted one otherwise; the other shapes ink glyph and caption alike.
fn slot_inks(shape: SlotShape, selected: bool) -> (Hsla, Hsla, f32) {
    let hover = if selected { 1.0 } else { 0.0 };
    let ink = match shape {
        SlotShape::Round => classic::GLYPH,
        SlotShape::Pill => in_motion::slot_ink(hover),
        SlotShape::Card => obsidian::text(),
    };
    let glyph_ink = match shape {
        SlotShape::Card if selected => obsidian::signal(),
        SlotShape::Card => obsidian::glyph(),
        SlotShape::Round | SlotShape::Pill => ink,
    };
    let glyph = match shape {
        SlotShape::Round => classic::SLOT_GLYPH,
        SlotShape::Pill => in_motion::PILL_GLYPH,
        SlotShape::Card => obsidian::CARD_GLYPH,
    } * shape.scale();
    (ink, glyph_ink, glyph)
}

fn slot_button(
    shape: SlotShape,
    slot: ActionRingSlot,
    entry: Option<&ActionRingEntry>,
    selected: bool,
    view: &Entity<ActionRingPanel>,
) -> impl IntoElement {
    let index = slot.index();
    let (left, top) = shape.origin(slot);
    let (width, height) = shape.size();
    let scale = shape.scale();
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
    // The ring hides an empty slot; the editor shows it as an outline so it
    // can still be picked.
    let empty = entry.is_none();
    let hover = if selected { 1.0 } else { 0.0 };
    let (ink, glyph_ink, glyph) = slot_inks(shape, selected);

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
        .map(|button| match shape {
            SlotShape::Round | SlotShape::Pill => button.rounded_full(),
            SlotShape::Card => button.rounded(px(obsidian::CARD_RADIUS * scale)),
        })
        .text_color(ink)
        .cursor_pointer()
        .map(|button| match (shape, empty) {
            (_, true) => button.border_1().border_color(EMPTY_SLOT_OUTLINE),
            (SlotShape::Round, false) if selected => button
                .bg(classic::selected_fill())
                .border_2()
                .border_color(classic::selected_border()),
            (SlotShape::Round, false) => button.bg(classic::SLOT_RESTING).shadow_md(),
            (SlotShape::Pill, false) => button.bg(in_motion::slot_fill(hover)),
            // The still frame shows the selected card pressed: its 2 px lift
            // would not read at this scale, the mint tint does.
            (SlotShape::Card, false) => button
                .bg(obsidian::card_fill(selected))
                .border_1()
                .border_color(obsidian::card_border(selected))
                .shadow(obsidian::card_shadow(selected)),
        })
        // How the glyph and caption sit: the pill in a row from its left edge,
        // the card in a column; the round slot centres its glyph alone.
        .map(|button| match (shape, empty) {
            (SlotShape::Pill, false) => button
                .justify_start()
                .gap(px(6.0 * scale))
                .px(px(12.0 * scale)),
            (SlotShape::Card, false) => button
                .flex_col()
                .gap(px(obsidian::CARD_GAP * scale)),
            (SlotShape::Round, _) | (_, true) => button,
        })
        .accessibility_label(accessible_label)
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .when_some(icon_path, |button, path| {
            button.child(
                svg()
                    .path(path)
                    .size(px(glyph))
                    .flex_none()
                    .text_color(glyph_ink),
            )
        })
        .when(empty, |button| {
            button.child(Icon::new(IconName::Plus).size_4())
        })
        // The pill and the card carry their caption, as the ring's do; the
        // round slot leaves it to the label under the hub and the tooltip.
        .when(shape.carries_caption() && !empty, |button| {
            let (width, centred) = match shape {
                SlotShape::Card => ((obsidian::CARD_W - 6.0) * scale, true),
                SlotShape::Pill | SlotShape::Round => ((in_motion::PILL_W - 40.0) * scale, false),
            };
            button.child(
                div()
                    .w(px(width))
                    .text_xs()
                    .when(centred, Div::text_center)
                    .truncate()
                    .child(caption),
            )
        })
        .hover(|button| button.opacity(0.85))
        .focus_visible(|button| button.border_2().border_color(rgb(theme::ACCENT_BLUE)))
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
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let layout = ActionRingLayout::default();
            ring_preview(self.theme, &layout, ActionRingSlot::Top, &self.panel).into_any_element()
        }
    }

    #[gpui::test]
    fn the_preview_follows_the_theme(cx: &mut TestAppContext) {
        const ROUND: &str = "action-ring-preview-round";
        const PILL: &str = "action-ring-preview-pill";
        const CARD: &str = "action-ring-preview-card";
        cx.update(gpui_component::init);
        install_app_state(cx);
        for (theme, present, absent) in [
            (ActionRingTheme::Classic, ROUND, [PILL, CARD]),
            (ActionRingTheme::InMotion, PILL, [ROUND, CARD]),
            (ActionRingTheme::Obsidian, CARD, [ROUND, PILL]),
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
            // At the theme's own scale, give or take GPUI's pixel snapping.
            let (width, height) = SlotShape::of(theme).size();
            assert!(
                (f32::from(slot.size.width) - width).abs() <= 1.0
                    && (f32::from(slot.size.height) - height).abs() <= 1.0,
                "{theme:?}: {:?} is not {width}×{height}",
                slot.size
            );
            for other in absent {
                assert!(
                    cx.debug_bounds(other).is_none(),
                    "{theme:?} draws no {other}"
                );
            }
        }
    }
}
