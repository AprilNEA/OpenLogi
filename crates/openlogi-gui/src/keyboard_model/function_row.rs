//! The keyboard function-row remapper view — the Keys tab body.
//!
//! A two-pane inspector model (the "pro-tool" layout): the keyboard photo sits
//! beside a row of mouse-style callout bubbles, and clicking a function key
//! **selects** it (no popover). A tall, scrollable config panel slides in on the
//! right while the keyboard physically makes room. Only one key is selected at a
//! time.
//!
//! F-key bindings are global (`AppState`'s keyboard map), committed via
//! [`AppState::commit_keyboard_binding`]. The panel lists the same action
//! catalog the mouse picker uses, plus a Power User section.

use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, BorrowAppContext as _, Bounds, Context, Entity, FontWeight,
    InteractiveElement, IntoElement, ParentElement, PathBuilder, Render,
    StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, hsla, point,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{h_flex, input::InputState, v_flex};
use openlogi_core::binding::WorkflowStep;
use openlogi_core::config::{KeyModifiers, KeyTrigger};

use crate::asset::ResolvedAsset;
use crate::data::mouse_buttons::Action;
use crate::keyboard_model::editors::{
    PowerUserKind, text_editor_placeholder, text_editor_seed, workflow_editor_seed,
};
use crate::mouse_model::picker::{
    PickFn, action_icon_path, action_rows, divider, menu_card, menu_row, scroll_list,
    section_header,
};
use crate::state::AppState;
use crate::theme::{self, ACCENT_BLUE, Palette};
use gpui::ease_in_out;
use gpui::{Animation, AnimationExt, img};

/// The programmable top-row keys visible on MX Keys-class keyboards: Esc, then
/// F1-F19. Each entry is the display label (on the key) + the [`KeyTrigger`]
/// keycode it binds.
const FUNCTION_KEYS: [(&str, u16); 20] = [
    ("Esc", 0x35),
    ("F1", 0x7A),
    ("F2", 0x78),
    ("F3", 0x63),
    ("F4", 0x76),
    ("F5", 0x60),
    ("F6", 0x61),
    ("F7", 0x62),
    ("F8", 0x64),
    ("F9", 0x65),
    ("F10", 0x6D),
    ("F11", 0x67),
    ("F12", 0x6F),
    ("F13", 0x69),
    ("F14", 0x6B),
    ("F15", 0x71),
    ("F16", 0x6A),
    ("F17", 0x40),
    ("F18", 0x4F),
    ("F19", 0x50),
];

/// Width of the config panel (CSS px) when a key is selected.
const PANEL_W: f32 = 320.;
/// Duration of the keyboard slide + panel slide animation.
const SLIDE_MS: u64 = 180;
/// Authored keyboard render width in the Keys inspector.
const KEYBOARD_W: f32 = 700.;
/// Approximate keyboard render height used for hit/leader overlays.
const KEYBOARD_IMG_H: f32 = 220.;
/// Space above the keyboard reserved for function-key callouts.
const CALLOUT_BAND_H: f32 = 118.;
const KEYBOARD_TOTAL_H: f32 = CALLOUT_BAND_H + KEYBOARD_IMG_H;
const KEY_CALLOUT_W: f32 = 60.;
const KEY_CALLOUT_H: f32 = 48.;
const KEY_CALLOUT_TOP_UPPER: f32 = 4.;
const KEY_CALLOUT_TOP_LOWER: f32 = 50.;
const KEY_TARGET_W: f32 = 30.;
const KEY_TARGET_H: f32 = 30.;
const KEY_HOTSPOT_DOT: f32 = 12.;
const FALLBACK_KEY_Y_FRAC: f32 = 0.153;
/// Logitech key markers are authored against a tighter internal keyboard
/// image. The rendered `front.png` includes a little more top/left padding, so
/// the raw marker lands high-left of the visible keycap center.
const FRONT_MARKER_X_OFFSET_FRAC: f32 = 0.02;
const FRONT_MARKER_Y_OFFSET_FRAC: f32 = 0.023;
/// Even-spacing fallback band (fractions of image width) when no metadata.
const EVEN_SPACING_START: f32 = 0.04;
const EVEN_SPACING_END: f32 = 0.96;

/// The function-row remapper view.
pub struct FunctionRowView {
    /// The single selected key index (0 = Esc), or `None` when nothing is
    /// selected (no panel shown).
    selected_key: Option<usize>,
    /// The hovered function-row key index, shared by callout bubbles, key hit
    /// zones, and leader lines.
    hovered_key: Option<usize>,
    /// Which power-user editor is showing in the panel, if any.
    active_editor: Option<PowerUserKind>,
    /// Lazily-created [`InputState`] for the text editors.
    text_state: Option<Entity<InputState>>,
    /// Draft copy of the Workflow steps under edit.
    workflow_draft: Vec<WorkflowStep>,
    _state_obs: Subscription,
}

impl FunctionRowView {
    /// Create the view.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.observe_global::<AppState>(|_view, cx| cx.notify());
        Self {
            selected_key: None,
            hovered_key: None,
            active_editor: None,
            text_state: None,
            workflow_draft: Vec::new(),
            _state_obs: state_obs,
        }
    }

    /// Select a key (or deselect with `None`), opening/closing the panel.
    pub(crate) fn select_key(&mut self, idx: Option<usize>, cx: &mut Context<Self>) {
        // Changing selection also drops any open editor + its drafts.
        if self.selected_key != idx {
            self.active_editor = None;
            self.text_state = None;
            self.workflow_draft.clear();
        }
        self.selected_key = idx;
        cx.notify();
    }

    /// Toggle a key selection from a click on either its callout or key hit
    /// target.
    pub(crate) fn click_key(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.select_key(next_selection_after_click(self.selected_key, idx), cx);
    }

    #[allow(dead_code, reason = "public accessor for the selection state")]
    pub(crate) fn selected_key(&self) -> Option<usize> {
        self.selected_key
    }

    pub(crate) fn set_hovered_key(&mut self, idx: Option<usize>, cx: &mut Context<Self>) {
        if self.hovered_key != idx {
            self.hovered_key = idx;
            cx.notify();
        }
    }

    pub(crate) fn open_editor(&mut self, kind: PowerUserKind, cx: &mut Context<Self>) {
        self.active_editor = Some(kind);
        self.text_state = None;
        self.workflow_draft.clear();
        cx.notify();
    }

    pub(crate) fn close_editor(&mut self, cx: &mut Context<Self>) {
        self.active_editor = None;
        self.text_state = None;
        self.workflow_draft.clear();
        cx.notify();
    }

    pub(crate) fn text_state(&self) -> Option<Entity<InputState>> {
        self.text_state.clone()
    }

    pub(crate) fn new_text_state(
        &mut self,
        seed: String,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let state = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(tr!(placeholder));
            if !seed.is_empty() {
                s.set_value(seed, window, cx);
            }
            s
        });
        self.text_state = Some(state.clone());
        state
    }

    pub(crate) fn workflow_draft(&self) -> &[WorkflowStep] {
        &self.workflow_draft
    }

    pub(crate) fn push_workflow_step(&mut self, step: WorkflowStep, cx: &mut Context<Self>) {
        self.workflow_draft.push(step);
        cx.notify();
    }

    pub(crate) fn remove_workflow_step(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx < self.workflow_draft.len() {
            self.workflow_draft.remove(idx);
            cx.notify();
        }
    }
}

impl Render for FunctionRowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let (asset, bindings): (Option<ResolvedAsset>, Vec<(KeyTrigger, Action)>) = cx
            .try_global::<AppState>()
            .map(|s| {
                (
                    s.current_record().and_then(|r| r.asset.clone()),
                    s.keyboard_bindings
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                )
            })
            .unwrap_or_default();

        let points = key_points(asset.as_ref());
        let slots: Vec<KeySlot> = FUNCTION_KEYS
            .iter()
            .enumerate()
            .map(|(idx, (label, keycode))| {
                let trigger = KeyTrigger {
                    keycode: *keycode,
                    modifiers: KeyModifiers::default(),
                };
                let bound = bindings
                    .iter()
                    .find(|(k, _)| *k == trigger)
                    .map(|(_, a)| a.clone());
                KeySlot {
                    idx,
                    label,
                    trigger,
                    x_frac: points[idx].x_frac,
                    y_frac: points[idx].y_frac,
                    bound,
                }
            })
            .collect();

        let selected = self.selected_key;
        let hovered = self.hovered_key;
        let active_editor = self.active_editor;
        if let (Some(selected_idx), Some(kind)) = (selected, active_editor)
            && let Some(slot) = slots.get(selected_idx)
        {
            let current_action = bindings
                .iter()
                .find(|(trigger, _)| trigger == &slot.trigger)
                .map(|(_, action)| action);
            match kind {
                PowerUserKind::Workflow => {
                    if self.workflow_draft.is_empty() {
                        self.workflow_draft = workflow_editor_seed(current_action);
                    }
                }
                _ => {
                    if self.text_state.is_none() {
                        self.new_text_state(
                            text_editor_seed(current_action, kind),
                            text_editor_placeholder(kind),
                            window,
                            cx,
                        );
                    }
                }
            }
        }
        let text_state = self.text_state.clone();
        let workflow_draft = self.workflow_draft.clone();
        let view = cx.entity();

        // The whole row animates as one: when a key is selected the right-side
        // panel grows in and the keyboard nudges left to make room.
        v_flex().w_full().items_center().child(inspector_row(
            slots,
            asset,
            selected,
            hovered,
            active_editor,
            text_state,
            workflow_draft,
            &view,
            &pal,
            window,
            cx,
        ))
    }
}

/// One function-row key with its resolved layout + binding.
#[derive(Clone)]
struct KeySlot {
    idx: usize,
    label: &'static str,
    trigger: KeyTrigger,
    x_frac: f32,
    y_frac: f32,
    bound: Option<Action>,
}

/// The two-pane row: keyboard photo + (when a key is selected) the side panel.
fn inspector_row(
    slots: Vec<KeySlot>,
    asset: Option<ResolvedAsset>,
    selected: Option<usize>,
    hovered: Option<usize>,
    active_editor: Option<PowerUserKind>,
    text_state: Option<Entity<InputState>>,
    workflow_draft: Vec<WorkflowStep>,
    view: &Entity<FunctionRowView>,
    pal: &Palette,
    window: &mut Window,
    cx: &mut Context<FunctionRowView>,
) -> impl IntoElement {
    let keyboard = keyboard_pane(slots.clone(), asset.as_ref(), selected, hovered, view, pal);

    // When nothing is selected, just the keyboard, full width.
    if selected.is_none() {
        return h_flex()
            .w_full()
            .justify_center()
            .child(keyboard)
            .into_any_element();
    }

    let panel = config_panel(
        selected.unwrap(),
        &slots,
        active_editor,
        text_state,
        workflow_draft,
        view,
        pal,
        window,
        cx,
    );

    // The panel grows in from width 0 → PANEL_W over SLIDE_MS, easing in/out,
    // always on the right as a stable inspector.
    let animated_panel = div().overflow_hidden().child(panel).with_animation(
        "panel-slide",
        Animation::new(std::time::Duration::from_millis(SLIDE_MS)).with_easing(ease_in_out),
        |el, delta| el.w(px(PANEL_W * delta)),
    );

    h_flex()
        .w_full()
        .gap_5()
        .items_center()
        .justify_center()
        .child(keyboard)
        .child(animated_panel)
        .into_any_element()
}

/// The keyboard photo with callout bubbles above each function key, leader
/// lines, and invisible click-targets over the real keys.
fn keyboard_pane(
    slots: Vec<KeySlot>,
    asset: Option<&ResolvedAsset>,
    selected: Option<usize>,
    hovered: Option<usize>,
    view: &Entity<FunctionRowView>,
    pal: &Palette,
) -> impl IntoElement {
    let img_path = asset.map(|a| a.image_path.clone());
    let view_clone = view.clone();

    div()
        .relative()
        .w(px(KEYBOARD_W))
        .h(px(KEYBOARD_TOTAL_H))
        .child(
            div()
                .absolute()
                .top(px(CALLOUT_BAND_H))
                .left(px(0.))
                .child(image_or_fallback(img_path, KEYBOARD_W, pal)),
        )
        .child(keyboard_leader_canvas(slots.clone(), selected, hovered))
        .children(slots.iter().cloned().map(|s| {
            let highlighted = key_is_highlighted(s.idx, selected, hovered);
            key_callout(s, highlighted, &view_clone, pal)
        }))
        // Click-targets overlay, centered on each key's marker point.
        .child(
            div()
                .absolute()
                .top(px(CALLOUT_BAND_H))
                .left(px(0.))
                .w(px(KEYBOARD_W))
                .h(px(KEYBOARD_IMG_H))
                .children(slots.into_iter().map(|s| {
                    let highlighted = key_is_highlighted(s.idx, selected, hovered);
                    key_click_target(s, highlighted, &view_clone, pal)
                })),
        )
}

/// One callout bubble above a function key.
fn key_callout(
    slot: KeySlot,
    highlighted: bool,
    view: &Entity<FunctionRowView>,
    pal: &Palette,
) -> AnyElement {
    let idx = slot.idx;
    let left = callout_left_px(slot.x_frac, KEYBOARD_W, KEY_CALLOUT_W);
    let top = callout_top_px(idx);
    let view_hover = view.clone();
    let view_click = view.clone();
    let binding = binding_label(slot.bound.as_ref());
    let binding_icon = slot.bound.as_ref().map(action_icon_path);

    v_flex()
        .id(("key-callout", idx))
        .absolute()
        .top(px(top))
        .left(px(left))
        .w(px(KEY_CALLOUT_W))
        .h(px(KEY_CALLOUT_H))
        .px_1()
        .justify_center()
        .items_center()
        .gap(px(1.))
        .rounded_md()
        .border_1()
        .border_color(if highlighted {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.border
        })
        .bg(if highlighted {
            theme::accent_tint()
        } else {
            pal.surface_hover
        })
        .cursor_pointer()
        .hover(move |s| {
            s.bg(if highlighted {
                theme::accent_tint_hover()
            } else {
                pal.surface
            })
        })
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if highlighted {
                    rgb(ACCENT_BLUE).into()
                } else {
                    pal.text_primary
                })
                .child(slot.label),
        )
        .child(
            h_flex()
                .items_center()
                .justify_center()
                .gap(px(2.))
                .max_w(px(KEY_CALLOUT_W - 8.))
                .when_some(binding_icon, |row, icon| {
                    row.child(svg().path(icon).size(px(9.)).flex_none().text_color(
                        if highlighted {
                            rgb(ACCENT_BLUE).into()
                        } else {
                            pal.text_muted
                        },
                    ))
                })
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_xs()
                        .text_color(if highlighted {
                            rgb(ACCENT_BLUE).into()
                        } else {
                            pal.text_muted
                        })
                        .child(binding),
                ),
        )
        .on_hover(move |hovered, _window, cx| {
            let next = (*hovered).then_some(idx);
            view_hover.update(cx, |v, vcx| v.set_hovered_key(next, vcx));
        })
        .on_click(move |_ev, _window, cx| {
            view_click.update(cx, |v, vcx| v.click_key(idx, vcx));
        })
        .into_any_element()
}

/// One invisible click-target over a function key. Selecting it opens the
/// panel; hover/selection draws only a subtle keycap ring on the photo.
fn key_click_target(
    slot: KeySlot,
    highlighted: bool,
    view: &Entity<FunctionRowView>,
    _pal: &Palette,
) -> AnyElement {
    let idx = slot.idx;
    let x_frac = slot.x_frac;
    let y_frac = slot.y_frac;
    let view_hover = view.clone();
    let view_click = view.clone();
    let left = key_target_left_px(x_frac, KEY_TARGET_W);
    let top = key_target_top_px(y_frac, KEY_TARGET_H);

    div()
        .id(("key-target", idx))
        .absolute()
        .top(px(top))
        .left(px(left))
        .w(px(KEY_TARGET_W))
        .h(px(KEY_TARGET_H))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(highlighted, |el| {
            el.child(
                div()
                    .w_full()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(KEY_HOTSPOT_DOT))
                            .h(px(KEY_HOTSPOT_DOT))
                            .rounded_full()
                            .border_1()
                            .border_color(gpui::Hsla::from(rgb(ACCENT_BLUE)))
                            .bg(gpui::Hsla::from(rgb(ACCENT_BLUE))),
                    )
                    .rounded_full()
                    .border_1()
                    .border_color(theme::accent_tint_hover())
                    .bg(theme::accent_tint()),
            )
        })
        .on_hover(move |hovered, _window, cx| {
            let next = (*hovered).then_some(idx);
            view_hover.update(cx, |v, vcx| v.set_hovered_key(next, vcx));
        })
        .on_click(move |_ev, _window, cx| {
            view_click.update(cx, |v, vcx| v.click_key(idx, vcx));
        })
        .into_any_element()
}

fn binding_label(action: Option<&Action>) -> gpui::SharedString {
    action
        .map(|a| match a {
            Action::CustomShortcut(combo) => combo.rendered_label().into(),
            _ => tr!(a.label()).into(),
        })
        .unwrap_or_else(|| tr!("Off").into())
}

fn keyboard_leader_canvas(
    slots: Vec<KeySlot>,
    selected: Option<usize>,
    hovered: Option<usize>,
) -> impl IntoElement {
    let guides: Vec<(usize, f32, f32)> =
        slots.iter().map(|s| (s.idx, s.x_frac, s.y_frac)).collect();
    canvas(
        move |_bounds, _, _| (guides, selected, hovered),
        move |bounds, payload, window, _app| {
            let (guides, selected, hovered) = payload;
            paint_keyboard_leaders(bounds, guides, selected, hovered, window);
        },
    )
    .absolute()
    .inset_0()
    .w(px(KEYBOARD_W))
    .h(px(KEYBOARD_TOTAL_H))
}

fn paint_keyboard_leaders(
    bounds: Bounds<gpui::Pixels>,
    guides: Vec<(usize, f32, f32)>,
    selected: Option<usize>,
    hovered: Option<usize>,
    window: &mut Window,
) {
    for (idx, x_frac, y_frac) in guides {
        let highlighted = key_is_highlighted(idx, selected, hovered);
        let key_x = x_frac * KEYBOARD_W;
        let key_y = CALLOUT_BAND_H + (y_frac * KEYBOARD_IMG_H);
        let callout_x = callout_left_px(x_frac, KEYBOARD_W, KEY_CALLOUT_W) + KEY_CALLOUT_W / 2.;
        let callout_bottom = callout_top_px(idx) + KEY_CALLOUT_H;
        let start = bounds.origin + point(px(callout_x), px(callout_bottom));
        let elbow = bounds.origin + point(px(callout_x), px(CALLOUT_BAND_H - 14.));
        let end = bounds.origin + point(px(key_x), px(key_y));

        let mut path = PathBuilder::stroke(if highlighted { px(2.) } else { px(1.) });
        path.move_to(start);
        path.line_to(elbow);
        path.line_to(end);
        if let Ok(path) = path.build() {
            if highlighted {
                window.paint_path(path, rgb(ACCENT_BLUE));
            } else {
                window.paint_path(path, hsla(0., 0., 0.55, 0.35));
            }
        }
    }
}

fn next_selection_after_click(current: Option<usize>, clicked: usize) -> Option<usize> {
    (current != Some(clicked)).then_some(clicked)
}

fn key_is_highlighted(idx: usize, selected: Option<usize>, hovered: Option<usize>) -> bool {
    selected == Some(idx) || hovered == Some(idx)
}

fn callout_left_px(x_frac: f32, image_w: f32, callout_w: f32) -> f32 {
    (x_frac * image_w - callout_w / 2.0).clamp(0.0, image_w - callout_w)
}

fn key_target_left_px(x_frac: f32, target_w: f32) -> f32 {
    (x_frac * KEYBOARD_W - target_w / 2.0).clamp(0.0, KEYBOARD_W - target_w)
}

fn key_target_top_px(y_frac: f32, target_h: f32) -> f32 {
    (y_frac * KEYBOARD_IMG_H - target_h / 2.0).clamp(0.0, KEYBOARD_IMG_H - target_h)
}

fn callout_top_px(idx: usize) -> f32 {
    if callout_lane_is_lower(idx) {
        KEY_CALLOUT_TOP_LOWER
    } else {
        KEY_CALLOUT_TOP_UPPER
    }
}

fn callout_lane_is_lower(idx: usize) -> bool {
    idx % 2 == 0
}

/// The scrollable config panel for the selected key. Lists the same action
/// catalog the mouse picker uses, plus a Power User section. Renders the rows
/// directly (no popover) in a tall card.
fn config_panel(
    selected_idx: usize,
    slots: &[KeySlot],
    active_editor: Option<PowerUserKind>,
    text_state: Option<Entity<InputState>>,
    workflow_draft: Vec<WorkflowStep>,
    view: &Entity<FunctionRowView>,
    pal: &Palette,
    _window: &mut Window,
    cx: &mut Context<FunctionRowView>,
) -> impl IntoElement {
    let slot = &slots[selected_idx];
    let trigger = slot.trigger.clone();
    let key_name = trigger.to_string();

    // If an editor is active, render it instead of the list.
    if let Some(kind) = active_editor {
        return crate::keyboard_model::editors::editor_card(
            trigger,
            kind,
            text_state,
            workflow_draft,
            view,
            *pal,
            cx,
        );
    }

    let current = cx
        .try_global::<AppState>()
        .and_then(|s| s.keyboard_bindings.get(&trigger).cloned());

    let view_for_pick = view.clone();
    let trigger_for_pick = trigger.clone();
    let on_pick: PickFn = Rc::new(move |action, _window, cx| {
        cx.update_global::<AppState, _>(|state, _| {
            state.commit_keyboard_binding(trigger_for_pick.clone(), Some(action));
        });
        view_for_pick.update(cx, |_, vcx| vcx.notify());
    });

    let rows = panel_action_rows(current.as_ref(), &on_pick, view, pal);

    menu_card(*pal)
        .w(px(PANEL_W))
        .max_h(px(500.))
        .child(title_header(&key_name, pal))
        .child(divider(*pal))
        .child(scroll_list("key-panel-scroll", rows))
        .into_any_element()
}

/// The panel's title — shows which key is selected, e.g. "F1".
fn title_header(key_name: &str, pal: &Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .justify_between()
        .px_2()
        .pb_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(pal.text_muted)
                .child(tr!("Bind %{name}", name => key_name)),
        )
}

/// The action rows + a Power User section, mirroring the picker's list but
/// adapted for the panel context (no popover dismissal).
fn panel_action_rows(
    current: Option<&Action>,
    on_pick: &PickFn,
    view: &Entity<FunctionRowView>,
    pal: &Palette,
) -> Vec<AnyElement> {
    let mut children = action_rows("panel-action", current, on_pick, *pal);
    children.push(section_header(&tr!("Power User"), *pal));

    let power_user_actions: &[(PowerUserKind, &str, &'static str)] = &[
        (
            PowerUserKind::TypeText,
            "Type Text…",
            "action-icons/keyboard.svg",
        ),
        (
            PowerUserKind::RunAppleScript,
            "Run AppleScript…",
            "action-icons/terminal.svg",
        ),
        (
            PowerUserKind::RunShellCommand,
            "Run Shell Command…",
            "action-icons/terminal.svg",
        ),
        (
            PowerUserKind::Workflow,
            "Workflow…",
            "action-icons/list-checks.svg",
        ),
    ];

    for (idx, (kind, label, icon_path)) in power_user_actions.iter().enumerate() {
        let kind = *kind;
        let view = view.clone();
        let selected = matches!(
            (current, kind),
            (Some(Action::TypeText(_)), PowerUserKind::TypeText)
                | (
                    Some(Action::RunAppleScript(_)),
                    PowerUserKind::RunAppleScript
                )
                | (
                    Some(Action::RunShellCommand(_)),
                    PowerUserKind::RunShellCommand
                )
                | (Some(Action::Workflow(_)), PowerUserKind::Workflow)
        );
        children.push(
            menu_row(format!("panel-power-{idx}"), *pal, selected)
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            svg()
                                .path(*icon_path)
                                .size_4()
                                .flex_none()
                                .text_color(pal.text_muted),
                        )
                        .child(div().child((*label).to_string())),
                )
                .when(selected, |s| {
                    s.child(
                        gpui_component::Icon::new(gpui_component::IconName::Check)
                            .size_3()
                            .text_color(rgb(ACCENT_BLUE)),
                    )
                })
                .on_click(move |_ev, _window, cx| {
                    view.update(cx, |v, vcx| v.open_editor(kind, vcx));
                })
                .into_any_element(),
        );
    }
    children
}

#[derive(Clone, Copy, Debug)]
struct KeyPoint {
    x_frac: f32,
    y_frac: f32,
}

/// Resolve key marker points as fractions [0..1] of the rendered image. Prefer
/// asset metadata's top-row markers; fall back to even spacing on the same row.
fn key_points(asset: Option<&ResolvedAsset>) -> Vec<KeyPoint> {
    if let Some(a) = asset {
        let key_markers = sorted_marker_points(a, &["device_keys_image", "device_buttons_image"]);
        let easy_switch_markers = sorted_marker_points(a, &["device_easyswitch_image"]);

        if key_markers.len() >= 16 && easy_switch_markers.len() >= 3 {
            let mut out = Vec::with_capacity(FUNCTION_KEYS.len());
            out.push(synthesized_esc_point(key_markers[0]));
            out.extend(
                key_markers[..12]
                    .iter()
                    .copied()
                    .map(calibrated_marker_point),
            );
            out.extend(
                easy_switch_markers[..3]
                    .iter()
                    .copied()
                    .map(calibrated_marker_point),
            );
            out.extend(
                key_markers[key_markers.len() - 4..]
                    .iter()
                    .copied()
                    .map(calibrated_marker_point),
            );
            if out.len() == FUNCTION_KEYS.len() {
                return out;
            }
        }

        if key_markers.len() >= FUNCTION_KEYS.len() - 1 {
            let f1_to_f19 = &key_markers[..FUNCTION_KEYS.len() - 1];
            let mut out = Vec::with_capacity(FUNCTION_KEYS.len());
            out.push(synthesized_esc_point(f1_to_f19[0]));
            out.extend(f1_to_f19.iter().copied().map(calibrated_marker_point));
            return out;
        }
    }
    fallback_key_points()
}

#[cfg(test)]
fn key_x_fractions(asset: Option<&ResolvedAsset>) -> Vec<f32> {
    key_points(asset)
        .into_iter()
        .map(|point| point.x_frac)
        .collect()
}

fn sorted_marker_points(asset: &ResolvedAsset, image_keys: &[&str]) -> Vec<KeyPoint> {
    let mut markers: Vec<KeyPoint> = asset
        .metadata
        .images
        .iter()
        .filter(|img| image_keys.contains(&img.key.as_str()))
        .flat_map(|img| img.assignments.iter())
        .map(|asg| KeyPoint {
            x_frac: asg.marker.x / 100.0,
            y_frac: asg.marker.y / 100.0,
        })
        .collect();
    markers.sort_by(|a, b| {
        a.x_frac
            .partial_cmp(&b.x_frac)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    markers
}

fn synthesized_esc_point(first_function_key: KeyPoint) -> KeyPoint {
    KeyPoint {
        x_frac: synthesized_esc_x(first_function_key.x_frac),
        y_frac: calibrated_marker_point(first_function_key).y_frac,
    }
}

fn calibrated_marker_point(raw: KeyPoint) -> KeyPoint {
    KeyPoint {
        x_frac: (raw.x_frac + FRONT_MARKER_X_OFFSET_FRAC).clamp(0.0, 1.0),
        y_frac: (raw.y_frac + FRONT_MARKER_Y_OFFSET_FRAC).clamp(0.0, 1.0),
    }
}

fn synthesized_esc_x(first_function_key_x: f32) -> f32 {
    (first_function_key_x - 0.045).max(0.02)
}

fn fallback_key_x_fractions() -> Vec<f32> {
    let step = (EVEN_SPACING_END - EVEN_SPACING_START) / (FUNCTION_KEYS.len() - 1) as f32;
    (0..FUNCTION_KEYS.len())
        .map(|i| EVEN_SPACING_START + (i as f32) * step)
        .collect()
}

fn fallback_key_points() -> Vec<KeyPoint> {
    fallback_key_x_fractions()
        .into_iter()
        .map(|x_frac| KeyPoint {
            x_frac,
            y_frac: FALLBACK_KEY_Y_FRAC,
        })
        .collect()
}

/// The keyboard image, or a labeled placeholder when no asset resolved.
fn image_or_fallback(
    img_path: Option<std::path::PathBuf>,
    img_w: f32,
    pal: &Palette,
) -> AnyElement {
    match img_path {
        Some(path) if path.exists() => img(path)
            .w(px(img_w))
            .h(px(KEYBOARD_IMG_H))
            .into_any_element(),
        Some(_) | None => div()
            .w(px(img_w))
            .h(px(160.))
            .rounded_md()
            .border_1()
            .border_color(pal.border)
            .bg(pal.surface)
            .flex()
            .items_center()
            .justify_center()
            .text_color(pal.text_muted)
            .child(tr!("No keyboard image available"))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_assets::{Assignment, Direction, ImageEntry, Metadata, Origin, Point};
    use openlogi_core::device::DeviceKind;
    use std::path::PathBuf;

    #[test]
    fn clicking_the_selected_key_closes_the_panel() {
        assert_eq!(next_selection_after_click(None, 3), Some(3));
        assert_eq!(next_selection_after_click(Some(3), 3), None);
        assert_eq!(next_selection_after_click(Some(3), 4), Some(4));
    }

    #[test]
    fn hover_or_selection_highlights_a_key() {
        assert!(key_is_highlighted(2, Some(2), None));
        assert!(key_is_highlighted(2, None, Some(2)));
        assert!(key_is_highlighted(2, Some(2), Some(7)));
        assert!(!key_is_highlighted(2, Some(1), Some(7)));
    }

    #[test]
    fn function_row_covers_esc_through_f19() {
        let labels: Vec<&str> = FUNCTION_KEYS.iter().map(|(label, _)| *label).collect();

        assert_eq!(FUNCTION_KEYS.len(), 20);
        assert_eq!(labels.first(), Some(&"Esc"));
        assert_eq!(labels.last(), Some(&"F19"));
        assert!(labels.contains(&"F13"));
        assert!(labels.contains(&"F19"));
    }

    #[test]
    fn fallback_key_positions_cover_the_full_top_row() {
        let positions = key_x_fractions(None);

        assert_eq!(positions.len(), 20);
        assert_eq!(positions.first().copied(), Some(EVEN_SPACING_START));
        assert_eq!(positions.last().copied(), Some(EVEN_SPACING_END));
    }

    #[test]
    fn mx_keys_markers_merge_function_and_easy_switch_groups() {
        let key_markers = vec![
            9.0, 13.4, 17.8, 22.3, 26.7, 31.15, 35.55, 40.05, 44.55, 49.1, 53.5, 57.9, 62.35, 81.5,
            85.9, 90.3, 94.7,
        ];
        let easy_switch_markers = vec![67.5, 71.92, 76.3];
        let asset = asset_with_markers(&key_markers, &easy_switch_markers);

        let positions = key_x_fractions(Some(&asset));

        assert_eq!(positions.len(), 20);
        assert_approx_eq(positions[0], 0.045);
        assert_approx_eq(positions[1], 0.11);
        assert_approx_eq(positions[12], 0.599);
        assert_approx_eq(positions[13], 0.695);
        assert_approx_eq(positions[15], 0.783);
        assert_approx_eq(positions[16], 0.835);
        assert_approx_eq(positions[19], 0.967);
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "positions should stay in physical left-to-right order"
        );
    }

    #[test]
    fn mx_keys_markers_preserve_key_center_points() {
        let key_markers = vec![
            9.0, 13.4, 17.8, 22.3, 26.7, 31.15, 35.55, 40.05, 44.55, 49.1, 53.5, 57.9, 62.35, 81.5,
            85.9, 90.3, 94.7,
        ];
        let easy_switch_markers = vec![67.5, 71.92, 76.3];
        let asset = asset_with_markers(&key_markers, &easy_switch_markers);

        let points = key_points(Some(&asset));

        assert_eq!(points.len(), 20);
        assert_approx_eq(points[19].x_frac, 0.967);
        assert_approx_eq(points[19].y_frac, 0.153);
        assert_approx_eq(key_target_top_px(points[19].y_frac, 30.0), 18.66);
    }

    #[test]
    fn callout_left_edge_is_clamped_to_keyboard_width() {
        assert_eq!(callout_left_px(0.0, 700.0, 64.0), 0.0);
        assert_eq!(callout_left_px(0.5, 700.0, 64.0), 318.0);
        assert_eq!(callout_left_px(1.0, 700.0, 64.0), 636.0);
    }

    #[test]
    fn function_key_callouts_stagger_even_lower_odd_upper() {
        assert!(callout_top_px(0) > callout_top_px(1));
        assert_eq!(callout_top_px(0), callout_top_px(2));
        assert_eq!(callout_top_px(1), callout_top_px(3));
    }

    #[test]
    fn staggered_function_key_callout_rows_fit_the_keyboard_width() {
        let lower_count = FUNCTION_KEYS
            .iter()
            .enumerate()
            .filter(|(idx, _)| callout_lane_is_lower(*idx))
            .count();
        let upper_count = FUNCTION_KEYS.len() - lower_count;
        assert!(
            KEY_CALLOUT_W * lower_count as f32 <= KEYBOARD_W,
            "lower callout lane overlaps before spacing is considered"
        );
        assert!(
            KEY_CALLOUT_W * upper_count as f32 <= KEYBOARD_W,
            "upper callout lane overlaps before spacing is considered"
        );
    }

    fn asset_with_markers(key_markers: &[f32], easy_switch_markers: &[f32]) -> ResolvedAsset {
        ResolvedAsset {
            depot: "mx_keys_s_for_mac".to_string(),
            display_name: "MX Keys S for Mac".to_string(),
            kind: DeviceKind::Keyboard,
            image_path: PathBuf::from("/tmp/mx-keys.png"),
            hero_image_path: None,
            glow: None,
            metadata: Metadata {
                images: vec![
                    ImageEntry {
                        key: "device_keys_image".to_string(),
                        origin: Origin {
                            width: 1872,
                            height: 728,
                        },
                        assignments: assignments_from_markers(key_markers),
                    },
                    ImageEntry {
                        key: "device_easyswitch_image".to_string(),
                        origin: Origin {
                            width: 1872,
                            height: 728,
                        },
                        assignments: assignments_from_markers(easy_switch_markers),
                    },
                ],
            },
            png_width: 1872,
            png_height: 728,
        }
    }

    fn assignments_from_markers(markers: &[f32]) -> Vec<Assignment> {
        markers
            .iter()
            .enumerate()
            .map(|(idx, x)| Assignment {
                slot_name: format!("slot-{idx}"),
                marker: Point { x: *x, y: 13.0 },
                label: Direction { x: -1, y: -1 },
            })
            .collect()
    }

    fn assert_approx_eq(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.0001,
            "expected {expected}, got {actual}"
        );
    }
}
