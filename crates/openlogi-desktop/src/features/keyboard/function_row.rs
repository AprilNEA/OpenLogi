//! The keyboard function-row remapper view — the Keys tab body.
//!
//! A two-pane inspector model (the "pro-tool" layout): the keyboard photo sits
//! beside a row of mouse-style callout bubbles, and clicking a function key
//! **selects** it (no popover). A tall, scrollable config panel slides in on the
//! right while the keyboard physically makes room. Only one key is selected at a
//! time.
//!
//! Each key has up to two layers, switched above the keyboard: its F-key, bound
//! globally (`AppState`'s keyboard map, committed via
//! [`AppState::commit_keyboard_binding`]), and — when the keyboard's image
//! names the key's HID++ control and the keyboard reports it divertable — its
//! hotkey, the printed function it sends without Fn, bound per device like a
//! mouse button. The panel lists the same action catalog the mouse picker
//! uses, plus a Power User section.

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
    SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, hsla,
    point, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_component::{Selectable as _, h_flex, input::InputState, v_flex};
use openlogi_core::binding::{Action, ButtonId, WorkflowStep};
use openlogi_core::config::{FunctionKey, KeyModifiers, KeyTrigger};

use super::editors::{
    PowerUserKind, text_editor_placeholder, text_editor_seed, workflow_editor_seed,
};
use super::target::{KeyTarget, hotkey_binding};
use crate::app::{glow_canvas, keyboard_glow};
use crate::features::binding_editor::{
    PickFn, action_icon_path, action_rows, compact_panel, divider, editor_scroll_list,
    editor_section,
};
use crate::features::mouse::geometry::asset_dimensions_for_png;
use crate::services::assets::{GlowGeometry, ResolvedAsset};
use crate::state::{AppState, StateEvent};
use crate::ui::action::localized_action_label;
use crate::ui::components::{MenuRow, PresetChip};
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};
use gpui::ease_in_out;
use gpui::{Animation, AnimationExt, img};

mod key_points;

use key_points::key_points;
#[cfg(test)]
use key_points::{EVEN_SPACING_END, EVEN_SPACING_START, key_x_fractions};

/// The full programmable top row: Esc, then F1-F19 — each key carries its
/// legend and the [`KeyTrigger`] keycode it binds. MX Keys-class boards expose
/// all 20; boards with a shorter F-row (a G513 has F1-F12) surface a prefix of
/// this list, sized by the asset's key markers — see [`key_points()`].
const FUNCTION_KEYS: [FunctionKey; 20] = FunctionKey::ALL;

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

/// The function-row remapper view.
pub struct FunctionRowView {
    /// The single selected key, or `None` when nothing is selected (no panel
    /// shown).
    selected: Option<KeySelection>,
    /// The hovered function-row key index, shared by callout bubbles, key hit
    /// zones, and leader lines.
    hovered_key: Option<usize>,
    /// Which of the keys' layers the row shows and edits. `None` until the
    /// user picks one: then hotkeys when the keyboard has any.
    layer: Option<KeyLayer>,
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
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::BindingsChanged(_)));
        Self {
            selected: None,
            hovered_key: None,
            layer: None,
            active_editor: None,
            text_state: None,
            workflow_draft: Vec::new(),
            _state_obs: state_obs,
        }
    }

    /// Select a key (or deselect with `None`), opening/closing the panel.
    fn select(&mut self, selection: Option<KeySelection>, cx: &mut Context<Self>) {
        // Changing selection also drops any open editor + its drafts.
        if self.selected != selection {
            self.active_editor = None;
            self.text_state = None;
            self.workflow_draft.clear();
        }
        self.selected = selection;
        cx.notify();
    }

    /// Toggle `clicked` from a click on a key's callout, its hit target on
    /// the photo, or a hotkey chip: clicking the selected key closes the panel.
    pub(crate) fn click(&mut self, clicked: KeySelection, cx: &mut Context<Self>) {
        self.select(next_selection_after_click(self.selected, clicked), cx);
    }

    /// Show and edit `layer`, closing any open key: the same slot is a
    /// different binding on the other layer.
    pub(crate) fn set_layer(&mut self, layer: KeyLayer, cx: &mut Context<Self>) {
        if self.layer != Some(layer) {
            self.layer = Some(layer);
            self.select(None, cx);
        }
    }

    /// The binding the selection edits on the current device.
    ///
    /// A stale selection can outlive a device switch to a shorter F-row or a
    /// keyboard without that hotkey; it is dropped instead of editing a key the
    /// current device doesn't have.
    fn resolve_target(&mut self, row: &KeyRow) -> Option<KeyTarget> {
        let target = match self.selected? {
            KeySelection::Function(idx) => row
                .slots
                .get(idx)
                .map(|slot| KeyTarget::Function(slot.trigger.clone())),
            KeySelection::Hotkey(button) => {
                row.has_hotkey(button).then_some(KeyTarget::Hotkey(button))
            }
        };
        if target.is_none() {
            self.selected = None;
            self.active_editor = None;
            self.text_state = None;
            self.workflow_draft.clear();
        }
        target
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
        placeholder: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let state = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(placeholder);
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
        let glow = state.and_then(|state| {
            state
                .current_record()
                .and_then(|record| keyboard_glow(state, record))
        });

        let viewport_h = f32::from(window.viewport_size().height);
        let render_size = keyboard_render_size(asset, viewport_h);
        let image_path = asset.map(|asset| asset.image_path.clone());
        let row = state.map_or_else(KeyRow::default, |state| {
            KeyRow::new(state, &key_points(asset))
        });
        let layer = self.layer.unwrap_or_else(|| {
            if row.has_hotkeys() {
                KeyLayer::Hotkey
            } else {
                KeyLayer::Function
            }
        });

        let target = self.resolve_target(&row);
        let selected = self.selected.and_then(|selection| row.slot_of(selection));
        let selected_strip_key = match self.selected {
            Some(KeySelection::Hotkey(button)) if selected.is_none() => Some(button),
            _ => None,
        };
        let hovered = self.hovered_key;
        let active_editor = self.active_editor;
        if let (Some(target), Some(kind)) = (&target, active_editor) {
            let current_action = AppState::try_read(cx).and_then(|state| target.current(state));
            let current_action = current_action.as_ref();
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
                            text_editor_placeholder(kind),
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
        let show_layers = row.has_hotkeys();
        let strip = (layer == KeyLayer::Hotkey && !row.unplaced.is_empty()).then(|| HotkeyStrip {
            slots: row.unplaced.clone(),
            selected: selected_strip_key,
            width: render_size.0,
            view: view.clone(),
        });
        let keyboard = KeyboardPane::new(row.slots, image_path, glow, render_size, view.clone())
            .layer(layer)
            .selected(selected)
            .hovered(hovered);
        let keyboard_column = v_flex()
            .items_center()
            .gap_3()
            .when(show_layers, |column| {
                column.child(layer_switch(layer, render_size.0, &view))
            })
            .child(keyboard)
            .children(strip);
        let panel = target.map(|target| self.config_panel(target, &view, cx));

        // The whole row animates as one: when a key is selected the right-side
        // panel grows in and the keyboard nudges left to make room.
        v_flex()
            .w_full()
            .items_center()
            .child(InspectorRow::new(keyboard_column).panel(panel))
    }
}

/// The "Function keys | Hotkeys" switch above a keyboard that has both.
fn layer_switch(layer: KeyLayer, width: f32, view: &Entity<FunctionRowView>) -> impl IntoElement {
    let layers = [KeyLayer::Function, KeyLayer::Hotkey];
    let view = view.clone();
    h_flex().w(px(width)).child(
        ButtonGroup::new("key-layer")
            .outline()
            .child(
                Button::new("key-layer-function")
                    .label(tr!("keyboard.function_keys"))
                    .selected(layer == KeyLayer::Function),
            )
            .child(
                Button::new("key-layer-hotkey")
                    .label(tr!("keyboard.hotkeys"))
                    .selected(layer == KeyLayer::Hotkey),
            )
            .on_click(move |indices, _window, cx| {
                let Some(layer) = indices.first().and_then(|index| layers.get(*index)) else {
                    return;
                };
                view.update(cx, |v, vcx| v.set_layer(*layer, vcx));
            }),
    )
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

/// One function-row key with its resolved layout and both layers' bindings.
#[derive(Clone)]
struct KeySlot {
    idx: usize,
    label: &'static str,
    trigger: KeyTrigger,
    x_frac: f32,
    y_frac: f32,
    binding: gpui::SharedString,
    binding_icon: Option<&'static str>,
    /// The key's hotkey layer, when it has one on this keyboard.
    hotkey: Option<SlotHotkey>,
}

/// A key's hotkey: the divertable control it sends without Fn, and what it is
/// bound to on the selected keyboard.
#[derive(Clone)]
struct SlotHotkey {
    button: ButtonId,
    binding: SharedString,
    binding_icon: Option<&'static str>,
}

impl KeySlot {
    /// What clicking this key selects on `layer`, or `None` when the key has
    /// nothing on that layer (Esc on the hotkey layer).
    fn selection(&self, layer: KeyLayer) -> Option<KeySelection> {
        match layer {
            KeyLayer::Function => Some(KeySelection::Function(self.idx)),
            KeyLayer::Hotkey => self
                .hotkey
                .as_ref()
                .map(|hotkey| KeySelection::Hotkey(hotkey.button)),
        }
    }

    /// The binding summary and icon shown in the key's callout on `layer`.
    fn binding_on(&self, layer: KeyLayer) -> (SharedString, Option<&'static str>) {
        match (layer, &self.hotkey) {
            (KeyLayer::Function, _) => (self.binding.clone(), self.binding_icon),
            (KeyLayer::Hotkey, Some(hotkey)) => (hotkey.binding.clone(), hotkey.binding_icon),
            (KeyLayer::Hotkey, None) => ("—".into(), None),
        }
    }
}

/// Which of a key's two functions the row shows and edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyLayer {
    /// The F-key, bound globally across keyboards.
    Function,
    /// The printed hotkey, bound per keyboard.
    Hotkey,
}

/// What the Keys panel has selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeySelection {
    /// A function-row key's F-key, by index into the rendered slots (0 = Esc).
    Function(usize),
    /// One of the keyboard's divertable hotkeys, on the photo or in the strip.
    Hotkey(ButtonId),
}

/// The selected keyboard's keys: the photo's slots with both layers, and the
/// divertable hotkeys the photo has no marker for.
#[derive(Default)]
struct KeyRow {
    slots: Vec<KeySlot>,
    unplaced: Vec<HotkeySlot>,
}

impl KeyRow {
    fn new(state: &AppState, points: &[key_points::KeyPoint]) -> Self {
        let divertable = state
            .current_record()
            .and_then(|record| record.capabilities)
            .map(|caps| caps.keyboard_keys)
            .unwrap_or_default();
        let keyboard_bindings = state.keyboard_bindings();
        let slots: Vec<KeySlot> = FUNCTION_KEYS
            .iter()
            .zip(points)
            .enumerate()
            .map(|(idx, (key, point))| {
                let trigger = KeyTrigger {
                    keycode: key.keycode(),
                    modifiers: KeyModifiers::default(),
                };
                let bound = keyboard_bindings.get(&trigger);
                let hotkey = point
                    .control
                    .and_then(ButtonId::for_keyboard_control)
                    .filter(|button| divertable.contains(*button))
                    .map(|button| {
                        let bound = hotkey_binding(state, button);
                        SlotHotkey {
                            button,
                            binding: binding_label(bound),
                            binding_icon: bound.map(action_icon_path),
                        }
                    });
                KeySlot {
                    idx,
                    label: key.label(),
                    trigger,
                    x_frac: point.x_frac,
                    y_frac: point.y_frac,
                    binding: binding_label(bound),
                    binding_icon: bound.map(action_icon_path),
                    hotkey,
                }
            })
            .collect();
        let unplaced = divertable
            .iter()
            .filter(|button| {
                !slots.iter().any(|slot| {
                    slot.hotkey
                        .as_ref()
                        .is_some_and(|hotkey| hotkey.button == *button)
                })
            })
            .map(|button| HotkeySlot {
                button,
                name: tr!(button.translation_key()),
                binding: binding_label(hotkey_binding(state, button)),
            })
            .collect();
        Self { slots, unplaced }
    }

    /// Whether the keyboard has any hotkey to bind, placed or not.
    fn has_hotkeys(&self) -> bool {
        !self.unplaced.is_empty() || self.slots.iter().any(|slot| slot.hotkey.is_some())
    }

    fn has_hotkey(&self, button: ButtonId) -> bool {
        self.unplaced.iter().any(|slot| slot.button == button)
            || self.slots.iter().any(|slot| {
                slot.hotkey
                    .as_ref()
                    .is_some_and(|hotkey| hotkey.button == button)
            })
    }

    /// The photo slot `selection` highlights, if it sits on the photo.
    fn slot_of(&self, selection: KeySelection) -> Option<usize> {
        self.slots
            .iter()
            .find(|slot| {
                [KeyLayer::Function, KeyLayer::Hotkey]
                    .into_iter()
                    .any(|layer| slot.selection(layer) == Some(selection))
            })
            .map(|slot| slot.idx)
    }
}

/// A divertable hotkey the keyboard's photo has no marker for.
#[derive(Clone)]
struct HotkeySlot {
    button: ButtonId,
    name: SharedString,
    binding: SharedString,
}

/// The keyboard's hotkeys that have no place on the photo, as selectable
/// chips under it. Selecting one opens the same config panel as a key.
#[derive(IntoElement)]
struct HotkeyStrip {
    slots: Vec<HotkeySlot>,
    selected: Option<ButtonId>,
    width: f32,
    view: Entity<FunctionRowView>,
}

impl RenderOnce for HotkeyStrip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let view = self.view;
        v_flex()
            .w(px(self.width))
            .gap_1()
            .child(editor_section(tr!("keyboard.hotkeys").to_string(), pal))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .children(self.slots.into_iter().map(|slot| {
                        let selected = self.selected == Some(slot.button);
                        // The catalog position is the key's stable identity.
                        let id = ButtonId::KEYBOARD_KEYS
                            .iter()
                            .position(|key| *key == slot.button)
                            .unwrap_or_default();
                        let view = view.clone();
                        let button = slot.button;
                        PresetChip::new(("hotkey-chip", id))
                            .selected(selected)
                            .child(
                                Button::new(("hotkey-select", id))
                                    .compact()
                                    .ghost()
                                    .h_full()
                                    .label(format!("{}: {}", slot.name, slot.binding))
                                    .selected(selected)
                                    .on_click(move |_event, _window, cx| {
                                        view.update(cx, |v, vcx| {
                                            v.click(KeySelection::Hotkey(button), vcx);
                                        });
                                    }),
                            )
                    })),
            )
    }
}

/// The two-pane row: keyboard photo (with its hotkey strip) + an optional
/// side panel.
#[derive(IntoElement)]
struct InspectorRow {
    keyboard: gpui::Div,
    panel: Option<gpui::Div>,
}

impl InspectorRow {
    fn new(keyboard: gpui::Div) -> Self {
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
    layer: KeyLayer,
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
            layer: KeyLayer::Function,
            selected: None,
            hovered: None,
            view,
        }
    }

    #[must_use]
    fn layer(mut self, layer: KeyLayer) -> Self {
        self.layer = layer;
        self
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
        let layer = self.layer;
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
                        layer,
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
                    key_click_target(slot, layer, highlighted, (img_w, img_h), &view_clone)
                })),
            )
    }
}

/// One callout bubble in the band above the keyboard.
#[derive(IntoElement)]
struct KeyCallout {
    slot: KeySlot,
    layer: KeyLayer,
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
        let (binding, binding_icon) = self.slot.binding_on(self.layer);
        let selection = self.slot.selection(self.layer);
        let highlighted = self.highlighted && selection.is_some();

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
            .when_some(selection, |callout, _| {
                callout.cursor_pointer().hover(move |s| {
                    s.bg(if highlighted {
                        theme::accent_tint_hover()
                    } else {
                        pal.control_hover
                    })
                })
            })
            // A key with nothing on this layer stays in place, so the row keeps
            // its shape, but reads as inert.
            .when(selection.is_none(), |callout| callout.opacity(0.45))
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
                let next = (*hovered && selection.is_some()).then_some(idx);
                view_hover.update(cx, |v, vcx| v.set_hovered_key(next, vcx));
            })
            .on_click(move |_ev, _window, cx| {
                if let Some(selection) = selection {
                    view_click.update(cx, |v, vcx| v.click(selection, vcx));
                }
            })
    }
}

/// One invisible click-target over a function key. Selecting it opens the
/// panel; hover/selection draws only a subtle keycap ring on the photo.
fn key_click_target(
    slot: KeySlot,
    layer: KeyLayer,
    highlighted: bool,
    (img_w, img_h): (f32, f32),
    view: &Entity<FunctionRowView>,
) -> impl IntoElement {
    let idx = slot.idx;
    let x_frac = slot.x_frac;
    let y_frac = slot.y_frac;
    let selection = slot.selection(layer);
    let highlighted = highlighted && selection.is_some();
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
        .when(selection.is_some(), Styled::cursor_pointer)
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
            let next = (*hovered && selection.is_some()).then_some(idx);
            view_hover.update(cx, |v, vcx| v.set_hovered_key(next, vcx));
        })
        .on_click(move |_ev, _window, cx| {
            if let Some(selection) = selection {
                view_click.update(cx, |v, vcx| v.click(selection, vcx));
            }
        })
}

fn binding_label(action: Option<&Action>) -> gpui::SharedString {
    match action {
        Some(action) => localized_action_label(action),
        None => tr!("common.off"),
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

fn next_selection_after_click<T: Copy + PartialEq>(current: Option<T>, clicked: T) -> Option<T> {
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
        target: KeyTarget,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let pal = theme::palette(cx);
        let key_name = target.name();

        // If an editor is active, render it instead of the list.
        if let Some(kind) = self.active_editor {
            return super::editors::editor_card(
                target,
                kind,
                self.text_state.clone(),
                self.workflow_draft.clone(),
                view,
                pal,
            );
        }

        let current = AppState::try_read(cx).and_then(|state| target.current(state));

        let view_for_pick = view.clone();
        let on_pick: PickFn = Rc::new(move |action, _window, cx| {
            AppState::apply(cx, |state| target.commit(state, action));
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
                .child(tr!("actions.bind_control", name => key_name)),
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
    let mut children = action_rows("panel-action", current, on_pick, *pal);

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
            .child(editor_section(tr!("actions.power_user").to_string(), *pal))
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
            .child(tr!("keyboard.no_keyboard_image_available"))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests;
