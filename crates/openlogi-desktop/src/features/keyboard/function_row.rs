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

#![expect(
    clippy::needless_pass_by_value,
    reason = "GPUI builders take owned Copy palette values"
)]
// Not `expect`: these fire inside `assert_eq!`, and rustc does not credit an
// expectation with a lint raised in a macro expansion.
#![allow(
    clippy::float_cmp,
    reason = "test and product compute the callout px through the same path"
)]

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FontWeight, Hsla,
    InteractiveElement, IntoElement, ParentElement, PathBuilder, Render, RenderOnce, Role,
    StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, hsla, point,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{Selectable as _, h_flex, input::InputState, v_flex};
use openlogi_core::binding::{Action, WorkflowStep};
use openlogi_core::config::{KeyModifiers, KeyTrigger};

use super::editors::{
    PowerUserKind, text_editor_placeholder, text_editor_seed, workflow_editor_seed,
};
use crate::app::{glow_canvas, keyboard_glow};
use crate::features::mouse::geometry::asset_dimensions_for_png;
use crate::features::mouse::picker::{
    ActionCatalogKind, PickFn, action_icon_path, action_rows, compact_panel, divider,
    editor_scroll_list, editor_section,
};
use crate::services::assets::{GlowGeometry, ResolvedAsset};
use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::action::localized_action_label;
use crate::ui::components::MenuRow;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};
use gpui::ease_in_out;
use gpui::{Animation, AnimationExt, img};

/// The full programmable top row: Esc, then F1-F19. Each entry is the display
/// label (on the key) + the [`KeyTrigger`] keycode it binds. MX Keys-class
/// boards expose all 20; boards with a shorter F-row (a G513 has F1-F12)
/// surface a prefix of this list, sized by the asset's key markers — see
/// [`key_points`].
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
/// Maximum keyboard render width in the Keys inspector.
const KEYBOARD_W: f32 = 700.;
/// Render size when no asset resolved: the placeholder box.
const FALLBACK_KEYBOARD_SIZE: (f32, f32) = (KEYBOARD_W, 220.);
/// Space above the keyboard reserved for function-key callouts.
const CALLOUT_BAND_H: f32 = 118.;
/// Vertical chrome around the keyboard pane (header, tab strip, screen
/// padding, footer) — the viewport height minus this and the callout band is
/// what the render may occupy before it scales down to fit.
const KEYS_VERTICAL_RESERVE: f32 = 224.;
/// Floor on the render height so a tiny window still shows a usable model.
const KEYBOARD_MIN_IMG_H: f32 = 160.;
const KEY_CALLOUT_W: f32 = 60.;
const KEY_CALLOUT_H: f32 = 48.;
const KEY_CALLOUT_TOP_UPPER: f32 = 4.;
const KEY_CALLOUT_TOP_LOWER: f32 = 50.;
const KEY_TARGET_W: f32 = 30.;
const KEY_TARGET_H: f32 = 30.;
const KEY_HOTSPOT_DOT: f32 = 12.;
const FALLBACK_KEY_Y_FRAC: f32 = 0.153;
/// Legacy pixel-marker depots (G513 family) mark F1-F12 but not Esc. Esc sits
/// this many key pitches left of F1 on that chassis (measured on the render).
const ESC_LEFT_OF_F1_PITCHES: f32 = 1.55;
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
        let state_obs = cx.subscribe(&AppState::global(cx), |_view, _, event: &StateEvent, cx| {
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

    #[expect(dead_code, reason = "public accessor for the selection state")]
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
        let state = AppState::try_read(cx);
        let asset = state.and_then(|state| state.current_record()?.asset.as_ref());
        let bindings = state.map(AppState::keyboard_bindings);
        let glow = state.and_then(|state| {
            state
                .current_record()
                .and_then(|record| keyboard_glow(state, record))
        });

        let viewport_h = f32::from(window.viewport_size().height);
        let render_size = keyboard_render_size(asset, viewport_h);
        let points = key_points(asset);
        let image_path = asset.map(|asset| asset.image_path.clone());
        let slots: Vec<KeySlot> = FUNCTION_KEYS
            .iter()
            .zip(points.iter())
            .enumerate()
            .map(|(idx, ((label, keycode), point))| {
                let trigger = KeyTrigger {
                    keycode: *keycode,
                    modifiers: KeyModifiers::default(),
                };
                let bound = bindings.and_then(|bindings| bindings.get(&trigger));
                KeySlot {
                    idx,
                    label,
                    trigger,
                    x_frac: point.x_frac,
                    y_frac: point.y_frac,
                    binding: binding_label(bound),
                    binding_icon: bound.map(action_icon_path),
                }
            })
            .collect();

        // A stale selection can outlive a device switch to a shorter F-row;
        // drop it instead of indexing past the new slot list.
        if self.selected_key.is_some_and(|idx| idx >= slots.len()) {
            self.selected_key = None;
            self.active_editor = None;
            self.text_state = None;
            self.workflow_draft.clear();
        }
        let selected = self.selected_key;
        let hovered = self.hovered_key;
        let active_editor = self.active_editor;
        if let (Some(selected_idx), Some(kind)) = (selected, active_editor)
            && let Some(slot) = slots.get(selected_idx)
        {
            let current_action = bindings.and_then(|bindings| bindings.get(&slot.trigger));
            match kind {
                PowerUserKind::Workflow => {
                    if self.workflow_draft.is_empty() {
                        self.workflow_draft = workflow_editor_seed(current_action);
                    }
                }
                _ => {
                    if let Some(state) = self.text_state.clone() {
                        crate::ui::components::localize_placeholder(
                            &state,
                            tr!(text_editor_placeholder(kind)),
                            window,
                            cx,
                        );
                    } else {
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
        let view = cx.entity();
        let keyboard =
            KeyboardPane::new(slots.clone(), image_path, glow, render_size, view.clone())
                .selected(selected)
                .hovered(hovered);
        let panel = selected.map(|selected| self.config_panel(selected, &slots, &view, cx));

        // The whole row animates as one: when a key is selected the right-side
        // panel grows in and the keyboard nudges left to make room.
        v_flex()
            .w_full()
            .items_center()
            .child(InspectorRow::new(keyboard).panel(panel))
    }
}

/// The keyboard render size: the actual PNG aspect at up to [`KEYBOARD_W`]
/// wide, shrunk to fit the viewport height. Sizing off the real aspect keeps
/// `ObjectFit::Contain` from letterboxing and keeps the marker overlays
/// registered with the rendered keys — the G513 render (with wrist rest) is
/// nearly twice as tall as an MX Keys render at the same width.
fn keyboard_render_size(asset: Option<&ResolvedAsset>, viewport_h: f32) -> (f32, f32) {
    let Some(asset) = asset.filter(|a| a.png_height > 0) else {
        return FALLBACK_KEYBOARD_SIZE;
    };
    let target_h = (viewport_h - KEYS_VERTICAL_RESERVE - CALLOUT_BAND_H).max(KEYBOARD_MIN_IMG_H);
    asset_dimensions_for_png(asset, target_h, KEYBOARD_W)
}

/// One function-row key with its resolved layout + binding.
#[derive(Clone)]
struct KeySlot {
    idx: usize,
    label: &'static str,
    trigger: KeyTrigger,
    x_frac: f32,
    y_frac: f32,
    binding: gpui::SharedString,
    binding_icon: Option<&'static str>,
}

/// The two-pane row: keyboard photo + an optional side panel.
#[derive(IntoElement)]
struct InspectorRow {
    keyboard: KeyboardPane,
    panel: Option<gpui::Div>,
}

impl InspectorRow {
    fn new(keyboard: KeyboardPane) -> Self {
        Self {
            keyboard,
            panel: None,
        }
    }

    #[must_use]
    fn panel(mut self, panel: Option<gpui::Div>) -> Self {
        self.panel = panel;
        self
    }
}

impl RenderOnce for InspectorRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .justify_center()
            .child(self.keyboard)
            .when_some(self.panel, |row, panel| {
                // The panel grows in from width 0 → PANEL_W over SLIDE_MS,
                // easing in/out, always on the right as a stable inspector.
                let animated_panel = div().overflow_hidden().child(panel).with_animation(
                    "panel-slide",
                    Animation::new(std::time::Duration::from_millis(SLIDE_MS))
                        .with_easing(ease_in_out),
                    |element, delta| element.w(px(PANEL_W * delta)),
                );
                row.gap_5().child(animated_panel)
            })
    }
}

/// The keyboard photo with callout bubbles above each function key, leader
/// lines, and invisible click-targets over the real keys.
#[derive(IntoElement)]
struct KeyboardPane {
    slots: Vec<KeySlot>,
    image_path: Option<std::path::PathBuf>,
    glow: Option<(Arc<GlowGeometry>, Hsla)>,
    render_size: (f32, f32),
    selected: Option<usize>,
    hovered: Option<usize>,
    view: Entity<FunctionRowView>,
}

impl KeyboardPane {
    fn new(
        slots: Vec<KeySlot>,
        image_path: Option<std::path::PathBuf>,
        glow: Option<(Arc<GlowGeometry>, Hsla)>,
        render_size: (f32, f32),
        view: Entity<FunctionRowView>,
    ) -> Self {
        Self {
            slots,
            image_path,
            glow,
            render_size,
            selected: None,
            hovered: None,
            view,
        }
    }

    #[must_use]
    fn selected(mut self, selected: impl Into<Option<usize>>) -> Self {
        self.selected = selected.into();
        self
    }

    #[must_use]
    fn hovered(mut self, hovered: impl Into<Option<usize>>) -> Self {
        self.hovered = hovered.into();
        self
    }
}

impl RenderOnce for KeyboardPane {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (img_w, img_h) = self.render_size;
        let img_path = self.image_path;
        let view_clone = self.view;
        let selected = self.selected;
        let hovered = self.hovered;
        let pal = theme::palette(cx);

        div()
            .relative()
            .w(px(img_w))
            .h(px(CALLOUT_BAND_H + img_h))
            .child(
            div()
                .absolute()
                .top(px(CALLOUT_BAND_H))
                .left(px(0.))
                .w(px(img_w))
                .h(px(img_h))
                // The keyboard's RGB paints *behind* the render, so the opaque
                // keys occlude it and the colour only reads through the
                // inter-key gaps — same treatment as the home gallery and the
                // mouse model.
                    .when_some(self.glow, |this, (geom, color)| {
                    this.child(glow_canvas(geom, color))
                })
                    .child(image_or_fallback(img_path, img_w, img_h, &pal)),
            )
            .child(keyboard_leader_canvas(
                self.slots.clone(),
                selected,
                hovered,
                (img_w, img_h),
            ))
            .children({
                let count = self.slots.len();
                let view_for_callouts = view_clone.clone();
                self.slots.iter().cloned().map(move |slot| {
                    let highlighted = key_is_highlighted(slot.idx, selected, hovered);
                    KeyCallout {
                        slot,
                        count,
                        highlighted,
                        img_w,
                        view: view_for_callouts.clone(),
                    }
                })
            })
            // Click-targets overlay, centered on each key's marker point.
            .child(
                div()
                    .absolute()
                    .top(px(CALLOUT_BAND_H))
                    .left(px(0.))
                    .w(px(img_w))
                    .h(px(img_h))
                    .children(self.slots.into_iter().map(|slot| {
                    let highlighted = key_is_highlighted(slot.idx, selected, hovered);
                    key_click_target(slot, highlighted, (img_w, img_h), &view_clone)
                })),
            )
    }
}

/// One callout bubble in the band above the keyboard.
#[derive(IntoElement)]
struct KeyCallout {
    slot: KeySlot,
    count: usize,
    highlighted: bool,
    img_w: f32,
    view: Entity<FunctionRowView>,
}

impl RenderOnce for KeyCallout {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let idx = self.slot.idx;
        let left = callout_left_px(idx, self.count, self.img_w, KEY_CALLOUT_W);
        let top = callout_top_px(idx);
        let view_hover = self.view.clone();
        let view_click = self.view;
        let binding = self.slot.binding;
        let binding_icon = self.slot.binding_icon;
        let highlighted = self.highlighted;

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
                pal.control
            })
            .cursor_pointer()
            .hover(move |s| {
                s.bg(if highlighted {
                    theme::accent_tint_hover()
                } else {
                    pal.control_hover
                })
            })
            .child(
                div()
                    .text_caption()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if highlighted {
                        rgb(ACCENT_BLUE).into()
                    } else {
                        pal.text_primary
                    })
                    .child(self.slot.label),
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
                            .text_caption()
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
    }
}

/// One invisible click-target over a function key. Selecting it opens the
/// panel; hover/selection draws only a subtle keycap ring on the photo.
fn key_click_target(
    slot: KeySlot,
    highlighted: bool,
    (img_w, img_h): (f32, f32),
    view: &Entity<FunctionRowView>,
) -> impl IntoElement {
    let idx = slot.idx;
    let x_frac = slot.x_frac;
    let y_frac = slot.y_frac;
    let view_hover = view.clone();
    let view_click = view.clone();
    let left = key_target_left_px(x_frac, img_w, KEY_TARGET_W);
    let top = key_target_top_px(y_frac, img_h, KEY_TARGET_H);

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
}

fn binding_label(action: Option<&Action>) -> gpui::SharedString {
    match action {
        Some(action) => localized_action_label(action),
        None => tr!("Off"),
    }
}

fn keyboard_leader_canvas(
    slots: Vec<KeySlot>,
    selected: Option<usize>,
    hovered: Option<usize>,
    (img_w, img_h): (f32, f32),
) -> impl IntoElement {
    let guides: Vec<(usize, f32, f32)> =
        slots.iter().map(|s| (s.idx, s.x_frac, s.y_frac)).collect();
    canvas(
        move |_bounds, _, _| (guides, selected, hovered),
        move |bounds, payload, window, _app| {
            let (guides, selected, hovered) = payload;
            paint_keyboard_leaders(bounds, guides, selected, hovered, (img_w, img_h), window);
        },
    )
    .absolute()
    .inset_0()
    .w(px(img_w))
    .h(px(CALLOUT_BAND_H + img_h))
}

fn paint_keyboard_leaders(
    bounds: Bounds<gpui::Pixels>,
    guides: Vec<(usize, f32, f32)>,
    selected: Option<usize>,
    hovered: Option<usize>,
    (img_w, img_h): (f32, f32),
    window: &mut Window,
) {
    let count = guides.len();
    for (idx, x_frac, y_frac) in guides {
        let highlighted = key_is_highlighted(idx, selected, hovered);
        let key_x = x_frac * img_w;
        let key_y = CALLOUT_BAND_H + (y_frac * img_h);
        let callout_x = callout_center_x(idx, count, img_w);
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

/// Callout bubbles lay out *evenly* across the pane instead of over their
/// keys: a dense F-row (a G513 packs Esc-F12 into half the render width)
/// would otherwise stack the bubbles into an overlapping wall. The leader
/// lines fan from each bubble down to its true key position.
#[expect(
    clippy::cast_precision_loss,
    reason = "idx/count index the function row — at most a couple of dozen keys"
)]
fn callout_center_x(idx: usize, count: usize, image_w: f32) -> f32 {
    let margin = KEY_CALLOUT_W / 2.0 + 4.0;
    if count <= 1 {
        return image_w / 2.0;
    }
    margin + (idx as f32) * (image_w - 2.0 * margin) / ((count - 1) as f32)
}

fn callout_left_px(idx: usize, count: usize, image_w: f32, callout_w: f32) -> f32 {
    (callout_center_x(idx, count, image_w) - callout_w / 2.0).clamp(0.0, image_w - callout_w)
}

fn key_target_left_px(x_frac: f32, img_w: f32, target_w: f32) -> f32 {
    (x_frac * img_w - target_w / 2.0).clamp(0.0, img_w - target_w)
}

fn key_target_top_px(y_frac: f32, img_h: f32, target_h: f32) -> f32 {
    (y_frac * img_h - target_h / 2.0).clamp(0.0, img_h - target_h)
}

fn callout_top_px(idx: usize) -> f32 {
    if callout_lane_is_lower(idx) {
        KEY_CALLOUT_TOP_LOWER
    } else {
        KEY_CALLOUT_TOP_UPPER
    }
}

fn callout_lane_is_lower(idx: usize) -> bool {
    idx.is_multiple_of(2)
}

/// The scrollable config panel for the selected key. Lists the same action
/// catalog the mouse picker uses, plus a Power User section. Renders the rows
/// directly (no popover) in a tall card.
impl FunctionRowView {
    fn config_panel(
        &self,
        selected_idx: usize,
        slots: &[KeySlot],
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let pal = theme::palette(cx);
        let slot = &slots[selected_idx];
        let trigger = slot.trigger.clone();
        let key_name = trigger.to_string();

        // If an editor is active, render it instead of the list.
        if let Some(kind) = self.active_editor {
            return super::editors::editor_card(
                trigger,
                kind,
                self.text_state.clone(),
                self.workflow_draft.clone(),
                view,
                pal,
            );
        }

        let current = AppState::try_read(cx)
            .and_then(|state| state.keyboard_bindings().get(&trigger).cloned());

        let view_for_pick = view.clone();
        let trigger_for_pick = trigger.clone();
        let on_pick: PickFn = Rc::new(move |action, _window, cx| {
            AppState::update(cx, |state, cx| {
                let key = state.current_record().map(DeviceRecord::device_key);
                state.commit_keyboard_binding(trigger_for_pick.clone(), Some(action));
                if let Some(key) = key {
                    cx.emit(StateEvent::BindingsChanged(key));
                }
            });
            view_for_pick.update(cx, |_, vcx| vcx.notify());
        });

        let rows = panel_action_rows(current.as_ref(), &on_pick, view, &pal);

        compact_panel(pal)
            .w(px(PANEL_W))
            .max_h(px(500.))
            .child(title_header(&key_name, &pal))
            .child(divider(pal))
            .child(editor_scroll_list("key-panel-scroll", rows))
    }
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
                .text_caption()
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
) -> Vec<gpui::Div> {
    // A keypress cannot keep a hold alive — same Instant catalog as a swipe slot.
    let mut children = action_rows(
        "panel-action",
        current,
        ActionCatalogKind::Instant,
        on_pick,
        *pal,
    );

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

    children.push(
        v_flex()
            .child(editor_section(tr!("Power User").to_string(), *pal))
            .children(power_user_actions.iter().enumerate().map(
                |(idx, (kind, label, icon_path))| {
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
                    MenuRow::new(format!("panel-power-{idx}"))
                        .selected(selected)
                        .role(Role::MenuItem)
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
                },
            )),
    );
    children
}

#[derive(Clone, Copy, Debug)]
struct KeyPoint {
    x_frac: f32,
    y_frac: f32,
}

/// Resolve key marker points as fractions [0..1] of the rendered image, along
/// with how many top-row keys the board exposes (`points.len()` — the visible
/// prefix of [`FUNCTION_KEYS`]). Prefer asset metadata's top-row markers —
/// percent-based on MX Keys-class depots, pixel-based on legacy keyboard
/// depots (G513) — and fall back to even spacing on the same row.
fn key_points(asset: Option<&ResolvedAsset>) -> Vec<KeyPoint> {
    if let Some(a) = asset {
        if let Some(points) = legacy_pixel_key_points(a) {
            return points;
        }
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

/// Key points from a legacy pixel-marker depot (the G513 family), or `None`
/// when the asset isn't one.
///
/// Legacy `metadata*.json` files mark each F-key's cap-face centre in
/// *absolute pixels* of the authored canvas (`origin`), not percentages. The
/// markers only apply when that canvas is the render we actually cached —
/// the same depot also ships marker sets authored against other variants'
/// renders (the G513's `metadata.json` belongs to the G512 banner render) —
/// so a depot whose `origin` doesn't match the PNG is rejected rather than
/// misplacing every callout.
fn legacy_pixel_key_points(asset: &ResolvedAsset) -> Option<Vec<KeyPoint>> {
    let img = asset
        .metadata
        .images
        .iter()
        .find(|img| img.key == "device_image" && !img.assignments.is_empty())?;
    if img.origin.width != asset.png_width || img.origin.height != asset.png_height {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "depot image dimensions are a few thousand pixels at most"
    )]
    let (w, h) = (img.origin.width as f32, img.origin.height as f32);

    let mut markers: Vec<KeyPoint> = img
        .assignments
        .iter()
        .map(|asg| asg.marker)
        // Percent-schema depots never exceed 100 on either axis; anything
        // beyond is a pixel coordinate. Mixed files don't exist in the wild,
        // but a percent marker slipping through would land off by 27x.
        .filter(|m| m.x > 100. || m.y > 100.)
        .map(|m| KeyPoint {
            x_frac: (m.x / w).clamp(0.0, 1.0),
            y_frac: (m.y / h).clamp(0.0, 1.0),
        })
        .collect();
    if markers.len() < 2 || markers.len() > FUNCTION_KEYS.len() - 1 {
        return None;
    }
    markers.sort_by(|a, b| {
        a.x_frac
            .partial_cmp(&b.x_frac)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // The depots mark F1..Fn but never Esc; place it left of F1 by the F-row's
    // own key pitch so it stays registered at any render size.
    let pitch = median_pitch(&markers)?;
    let first = markers[0];
    let esc = KeyPoint {
        x_frac: (first.x_frac - ESC_LEFT_OF_F1_PITCHES * pitch).max(0.0),
        y_frac: first.y_frac,
    };

    let mut out = Vec::with_capacity(markers.len() + 1);
    out.push(esc);
    out.extend(markers);
    Some(out)
}

/// Median gap between adjacent marker x positions — the F-row's key pitch.
/// The median rides out the wider inter-cluster gaps (F4→F5, F8→F9).
fn median_pitch(sorted_markers: &[KeyPoint]) -> Option<f32> {
    let mut gaps: Vec<f32> = sorted_markers
        .windows(2)
        .map(|pair| pair[1].x_frac - pair[0].x_frac)
        .filter(|gap| *gap > 0.)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by(f32::total_cmp);
    Some(gaps[gaps.len() / 2])
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

#[expect(
    clippy::cast_precision_loss,
    reason = "FUNCTION_KEYS is a fixed table of a dozen entries"
)]
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

/// The keyboard image, or a labeled placeholder when no asset resolved. The
/// element is sized to the PNG's own aspect (see [`keyboard_render_size`]), so
/// the contain-fit paints edge to edge and the marker overlays stay registered.
fn image_or_fallback(
    img_path: Option<std::path::PathBuf>,
    img_w: f32,
    img_h: f32,
    pal: &Palette,
) -> AnyElement {
    match img_path {
        Some(path) if path.exists() => img(path).w(px(img_w)).h(px(img_h)).into_any_element(),
        Some(_) | None => div()
            .w(px(img_w))
            .h(px(160.))
            .rounded_md()
            .border_1()
            .border_color(pal.border)
            .bg(pal.panel)
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
        assert_approx_eq(key_target_top_px(points[19].y_frac, 220.0, 30.0), 18.66);
    }

    /// The G513 family's `metadata_full.json`: `device_image` markers in
    /// absolute pixels of the authored canvas, which matches the cached
    /// render. F1-F12 come from the markers; Esc is synthesized one chassis
    /// offset left of F1.
    #[test]
    fn g513_pixel_markers_resolve_esc_plus_f1_to_f12() {
        let marker_xs = [
            285., 405., 525., 645., 840., 960., 1080., 1200., 1395., 1515., 1635., 1755.,
        ];
        let asset = legacy_asset(&marker_xs, 290., (2760, 1600), (2760, 1600));

        let points = key_points(Some(&asset));

        assert_eq!(points.len(), 13, "Esc + F1-F12, no phantom F13-F19");
        assert_approx_eq(points[1].x_frac, 285. / 2760.);
        assert_approx_eq(points[12].x_frac, 1755. / 2760.);
        // Esc: 1.55 key pitches (median gap 120px) left of F1.
        assert_approx_eq(points[0].x_frac, (285. - 1.55 * 120.) / 2760.);
        for point in &points {
            assert_approx_eq(point.y_frac, 290. / 1600.);
        }
        assert!(
            points
                .windows(2)
                .all(|pair| pair[0].x_frac < pair[1].x_frac),
            "points stay in physical left-to-right order"
        );
    }

    /// The same depot's `metadata.json` is authored against a *different*
    /// render (the G512 banner). Its origin doesn't match the cached PNG, so
    /// the markers must be rejected in favour of the even-spacing fallback
    /// rather than misplacing every callout.
    #[test]
    fn pixel_markers_for_a_different_render_fall_back_to_even_spacing() {
        let marker_xs = [370., 525., 680., 835., 1090., 1250., 1400., 1555.];
        let asset = legacy_asset(&marker_xs, 300., (3598, 1315), (2760, 1600));

        let points = key_points(Some(&asset));

        assert_eq!(points.len(), FUNCTION_KEYS.len());
        assert_approx_eq(points[0].x_frac, EVEN_SPACING_START);
        assert_approx_eq(points[19].x_frac, EVEN_SPACING_END);
    }

    #[test]
    fn render_size_follows_the_png_aspect_up_to_the_width_cap() {
        // MX Keys-class render (1872x728): width-bound at a roomy viewport.
        let mx = legacy_asset(&[], 0., (1872, 728), (1872, 728));
        let (w, h) = keyboard_render_size(Some(&mx), 900.);
        assert_approx_eq(w, 700.);
        assert!((h - 700. * 728. / 1872.).abs() < 0.01);

        // G513 render (2760x1600) is far taller at the same width.
        let g513 = legacy_asset(&[], 0., (2760, 1600), (2760, 1600));
        let (w, h) = keyboard_render_size(Some(&g513), 900.);
        assert_approx_eq(w, 700.);
        assert!((h - 700. * 1600. / 2760.).abs() < 0.01);

        // A short viewport shrinks the render instead of overflowing it.
        let (w, h) = keyboard_render_size(Some(&g513), 500.);
        assert_approx_eq(h, KEYBOARD_MIN_IMG_H);
        assert!((w - KEYBOARD_MIN_IMG_H * 2760. / 1600.).abs() < 0.01);

        assert_eq!(keyboard_render_size(None, 900.), FALLBACK_KEYBOARD_SIZE);
    }

    #[test]
    fn callouts_spread_evenly_from_margin_to_margin() {
        let margin = KEY_CALLOUT_W / 2.0 + 4.0;
        assert_approx_eq(callout_center_x(0, 13, 700.0), margin);
        assert_approx_eq(callout_center_x(12, 13, 700.0), 700.0 - margin);
        assert_approx_eq(callout_center_x(0, 1, 700.0), 350.0);
        assert!(callout_left_px(0, 13, 700.0, KEY_CALLOUT_W) >= 0.0);
        assert!(callout_left_px(12, 13, 700.0, KEY_CALLOUT_W) <= 700.0 - KEY_CALLOUT_W);
    }

    /// Bubbles share a stagger lane with every second key; same-lane
    /// neighbours must never overlap for any board size the row can show.
    #[test]
    fn same_lane_callouts_never_overlap() {
        for count in [13usize, 20] {
            for idx in 0..count.saturating_sub(2) {
                let gap = callout_center_x(idx + 2, count, KEYBOARD_W)
                    - callout_center_x(idx, count, KEYBOARD_W);
                assert!(
                    gap >= KEY_CALLOUT_W,
                    "lane neighbours {idx}/{} overlap at count {count}: gap {gap}",
                    idx + 2
                );
            }
        }
    }

    #[test]
    fn function_key_callouts_stagger_even_lower_odd_upper() {
        assert!(callout_top_px(0) > callout_top_px(1));
        assert_eq!(callout_top_px(0), callout_top_px(2));
        assert_eq!(callout_top_px(1), callout_top_px(3));
    }

    #[test]
    #[expect(
        clippy::cast_precision_loss,
        reason = "lane counts are bounded by FUNCTION_KEYS"
    )]
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

    /// A legacy pixel-marker asset: `device_image` assignments in absolute
    /// pixels of an `origin` canvas, over a render of `png` dimensions.
    fn legacy_asset(
        marker_xs: &[f32],
        marker_y: f32,
        origin: (u32, u32),
        png: (u32, u32),
    ) -> ResolvedAsset {
        let assignments = marker_xs
            .iter()
            .map(|x| Assignment {
                slot_name: String::new(),
                marker: Point { x: *x, y: marker_y },
                label: Direction { x: -1, y: -1 },
            })
            .collect();
        ResolvedAsset {
            depot: "g513".to_string(),
            display_name: "G513".to_string(),
            kind: Some(DeviceKind::Keyboard),
            image_path: PathBuf::from("/tmp/g513.png"),
            hero_image_path: None,
            glow: None,
            metadata: Metadata {
                images: vec![ImageEntry {
                    key: "device_image".to_string(),
                    origin: Origin {
                        width: origin.0,
                        height: origin.1,
                    },
                    assignments,
                }],
            },
            png_width: png.0,
            png_height: png.1,
        }
    }

    fn asset_with_markers(key_markers: &[f32], easy_switch_markers: &[f32]) -> ResolvedAsset {
        ResolvedAsset {
            depot: "mx_keys_s_for_mac".to_string(),
            display_name: "MX Keys S for Mac".to_string(),
            kind: Some(DeviceKind::Keyboard),
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
