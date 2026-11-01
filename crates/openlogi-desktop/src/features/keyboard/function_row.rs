//! The keyboard key remapper view — the Keys tab body.
//!
//! A two-pane inspector model (the "pro-tool" layout): the keyboard photo sits
//! beside a row of mouse-style callout bubbles, and clicking a key **selects**
//! it (no popover). A tall, scrollable config panel slides in on the right
//! while the keyboard physically makes room. Only one key is selected at a
//! time.
//!
//! Which keys appear is the asset's call (see [`key_points::key_surface`]): a
//! depot with control markers shows the keyboard's own HID++ controls on the
//! photo, each bound per device through [`AppState::commit_binding`] and
//! diverted by the agent while bound; a keypad whose depot marks its keys,
//! or whose render is the stock one OpenLogi measured, has its keys drawn on
//! the device render itself, the way Options+ shows it; any other keypad
//! shows its own keycaps drawn from the device model beside the render, bound
//! the same per-device way; any other markerless device shows the OS-hook F-row,
//! bound globally through [`AppState::commit_keyboard_binding`]. The panel
//! lists the same action catalog the mouse picker uses, plus a Power User
//! section.

#![expect(
    clippy::needless_pass_by_value,
    reason = "GPUI builders take owned Copy palette values"
)]

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FontWeight, Hsla,
    InteractiveElement, IntoElement, ParentElement, PathBuilder, Render, RenderOnce, Role,
    SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, hsla,
    point, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Selectable as _, h_flex, input::InputState, v_flex};
use openlogi_core::binding::{Action, ActionRingIcon, ButtonId, KeypadPage, WorkflowStep};

use super::editors::{
    PowerUserKind, text_editor_placeholder, text_editor_seed, workflow_editor_seed,
};
use crate::app::{glow_canvas, keyboard_glow};
use crate::features::binding_editor::{
    PickFn, action_icon_path, action_rows, compact_panel, divider, editor_scroll_list,
    editor_section,
};
use crate::features::mouse::geometry::asset_dimensions_for_png;
use crate::features::profiles::profile_name;
use crate::services::assets::{GlowGeometry, ResolvedAsset};
use crate::state::{AppState, DeviceKey, DeviceRecord, StateEvent, StateEvents};
use crate::ui::action::localized_action_label;
use crate::ui::components::MenuRow;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};
use gpui::ease_in_out;
use gpui::{Animation, AnimationExt, img};

mod callout_layout;
mod key_points;
mod keypad;

use callout_layout::{CalloutLayout, CardRect, callout_layout, side_bands_width};
pub(crate) use key_points::KeyTarget;
#[cfg(test)]
use key_points::{EVEN_SPACING_END, EVEN_SPACING_START, key_x_fractions};
use key_points::{KeySurface, key_surface, keypad_canvas};
use keypad::{KeypadPane, KeypadPhotoPane, LastPage, keypad_page_bar, keypad_photo_render_size};

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
/// Horizontal chrome around the Keys tab body: the device rail and the tab's
/// padding.
const KEYS_HORIZONTAL_RESERVE: f32 = crate::ui::theme::DETAIL_RAIL_W + 48.;
/// Floor on the render height so a tiny window still shows a usable model.
const KEYBOARD_MIN_IMG_H: f32 = 160.;
const KEY_CALLOUT_H: f32 = 48.;
const KEY_CALLOUT_TOP_UPPER: f32 = 2.;
const KEY_CALLOUT_TOP_LOWER: f32 = 56.;
const KEY_TARGET_W: f32 = 30.;
const KEY_TARGET_H: f32 = 30.;
const KEY_HOTSPOT_DOT: f32 = 12.;

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
    /// Whether the Open Application editor rejected a blank path.
    application_invalid: bool,
    /// The keypad binding page the grid keys are being edited on — this
    /// editor's own choice, independent of the page the keypad is showing.
    keypad_page: KeypadPage,
    /// The device and app profile being edited: another device starts back
    /// on page 1, and either change drops the open editor.
    edit_scope: EditScope,
    _state_obs: Subscription,
}

/// Which device's bindings, in which app profile, the view edits — beside
/// the key and the page, what a draft was started for.
#[derive(Debug, Default, PartialEq, Eq)]
struct EditScope {
    device: Option<DeviceKey>,
    /// The open app profile, `None` for the device's global one.
    profile: Option<String>,
}

impl FunctionRowView {
    /// Create the view.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::BindingsChanged(_)));
        Self {
            selected_key: None,
            hovered_key: None,
            active_editor: None,
            text_state: None,
            workflow_draft: Vec::new(),
            application_invalid: false,
            keypad_page: KeypadPage::FIRST,
            edit_scope: EditScope::default(),
            _state_obs: state_obs,
        }
    }

    /// Select a key (or deselect with `None`), opening/closing the panel.
    pub(crate) fn select_key(&mut self, idx: Option<usize>, cx: &mut Context<Self>) {
        // Changing selection also drops any open editor + its drafts.
        if self.selected_key != idx {
            self.drop_editor();
        }
        self.selected_key = idx;
        cx.notify();
    }

    /// Close the open editor and discard its drafts. Every change of the key
    /// being edited — another key, page, device, or app profile — calls
    /// this, or a draft started for one would save onto the next.
    fn drop_editor(&mut self) {
        self.active_editor = None;
        self.text_state = None;
        self.workflow_draft.clear();
        self.application_invalid = false;
    }

    /// Edit the grid keys on `page`, dropping any open editor when it moves.
    fn move_keypad_page(&mut self, page: KeypadPage) {
        if self.keypad_page != page {
            self.keypad_page = page;
            self.drop_editor();
        }
    }

    /// Toggle a key selection from a click on either its callout or key hit
    /// target.
    pub(crate) fn click_key(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.select_key(next_selection_after_click(self.selected_key, idx), cx);
    }

    pub(crate) fn set_hovered_key(&mut self, idx: Option<usize>, cx: &mut Context<Self>) {
        if self.hovered_key != idx {
            self.hovered_key = idx;
            cx.notify();
        }
    }

    /// Follow the selected device and the open app profile — dropping a
    /// draft started for another, and starting a new device on page 1 — keep
    /// the edited page on the device's pages (clamped when pages were
    /// removed), and answer the device's last page and whether removing it
    /// would drop bindings (so removal asks).
    fn sync_edit_scope(&mut self, state: Option<&AppState>) -> LastPage {
        let scope = EditScope {
            device: state
                .and_then(AppState::current_record)
                .map(DeviceRecord::device_key),
            profile: state.and_then(AppState::editing_app).map(str::to_owned),
        };
        if self.edit_scope != scope {
            if self.edit_scope.device != scope.device {
                self.move_keypad_page(KeypadPage::FIRST);
            }
            self.drop_editor();
            self.edit_scope = scope;
        }
        let last = state.map_or(KeypadPage::FIRST, AppState::current_keypad_pages);
        self.move_keypad_page(self.keypad_page.min(last));
        // Every profile's keys, not just the open one's: removal drops them all.
        let binds_keys = state.is_some_and(|state| state.current_keypad_page_binds(last));
        LastPage {
            page: last,
            binds_keys,
        }
    }

    /// Edit the grid keys on `page`.
    pub(crate) fn set_keypad_page(&mut self, page: KeypadPage, cx: &mut Context<Self>) {
        if self.keypad_page != page {
            self.move_keypad_page(page);
            cx.notify();
        }
    }

    pub(crate) fn open_editor(&mut self, kind: PowerUserKind, cx: &mut Context<Self>) {
        self.active_editor = Some(kind);
        self.text_state = None;
        self.workflow_draft.clear();
        self.application_invalid = false;
        cx.notify();
    }

    pub(crate) fn close_editor(&mut self, cx: &mut Context<Self>) {
        self.drop_editor();
        cx.notify();
    }

    pub(crate) fn mark_application_invalid(&mut self, cx: &mut Context<Self>) {
        self.application_invalid = true;
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
        // Without a record there is no device surface to trust, and the F-row
        // fallback is as honest as anything that shows.
        let known_keypad = state.and_then(AppState::current_keypad);
        let glow = state.and_then(|state| {
            state
                .current_record()
                .and_then(|record| keyboard_glow(state, record))
        });

        let viewport = window.viewport_size();
        let viewport_h = f32::from(viewport.height);
        let image_path = asset.map(|asset| asset.image_path.clone());
        let surface = key_surface(asset, known_keypad);
        let keypad = matches!(surface, KeySurface::Keypad(_));
        // A keypad on its photo draws its keys on the render itself: the
        // keyboard's callout band cannot lay eleven bubbles across a
        // portrait render, and the keys sit under the user's eyes already.
        let keypad_photo = matches!(surface, KeySurface::Photo(_)) && known_keypad.is_some();
        // The schematic keeps photo and keycaps on one alignment line.
        let photo = asset
            .filter(|asset| asset.png_height > 0)
            .map_or((0., 0.), |asset| {
                asset_dimensions_for_png(asset, keypad_canvas().1, 360.)
            });
        let last_page = self.sync_edit_scope(state);
        let pal = theme::palette(cx);
        let slots = key_slots_from_layouts(surface, state, self.keypad_page);
        let render_size = if keypad_photo {
            keypad_photo_render_size(asset, viewport_h)
        } else {
            let markers: Vec<(f32, f32)> = slots
                .iter()
                .map(|slot| (slot.x_frac, slot.y_frac))
                .collect();
            keyboard_render_size(
                asset,
                viewport_h,
                keyboard_max_width(f32::from(viewport.width), side_bands_width(&markers)),
            )
        };

        // A stale selection can outlive a device switch to a shorter F-row;
        // drop it instead of indexing past the new slot list.
        if self.selected_key.is_some_and(|idx| idx >= slots.len()) {
            self.selected_key = None;
            self.drop_editor();
        }
        let selected = self.selected_key;
        let hovered = self.hovered_key;
        let active_editor = self.active_editor;
        if let (Some(selected_idx), Some(kind)) = (selected, active_editor)
            && let Some(slot) = slots.get(selected_idx)
        {
            let current_action = state.and_then(|state| bound_action(state, &slot.target));
            match kind {
                PowerUserKind::Workflow => {
                    if self.workflow_draft.is_empty() {
                        self.workflow_draft = workflow_editor_seed(current_action.as_ref());
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
                            text_editor_seed(current_action.as_ref(), kind),
                            text_editor_placeholder(kind),
                            window,
                            cx,
                        );
                    }
                }
            }
        }
        let view = cx.entity();
        // The same selection, panel, and editor machinery on either surface:
        // the keyboard's photo with callouts, or the keypad's photo beside
        // drawn keycaps.
        let device = if keypad {
            KeypadPane::new(slots.clone(), image_path, photo, view.clone())
                .selected(selected)
                .hovered(hovered)
                .into_any_element()
        } else if keypad_photo {
            KeypadPhotoPane::new(slots.clone(), image_path, render_size, view.clone())
                .selected(selected)
                .hovered(hovered)
                .into_any_element()
        } else {
            KeyboardPane::new(slots.clone(), image_path, glow, render_size, view.clone())
                .selected(selected)
                .hovered(hovered)
                .into_any_element()
        };
        let panel = selected.map(|selected| self.config_panel(selected, &slots, &view, cx));
        let page_bar = (keypad || keypad_photo)
            .then(|| keypad_page_bar(self.keypad_page, last_page, &view, &pal));

        // The whole row animates as one: when a key is selected the right-side
        // panel grows in and the keyboard nudges left to make room.
        v_flex()
            .w_full()
            .items_center()
            .gap_3()
            .children(page_bar)
            .child(InspectorRow::new(device).panel(panel))
    }
}

fn binding_line(
    binding: SharedString,
    icon: Option<&'static str>,
    max_w: f32,
    highlighted: bool,
    pal: &Palette,
) -> gpui::Div {
    h_flex()
        .items_center()
        .justify_center()
        .gap(px(2.))
        .max_w(px(max_w))
        .when_some(icon, |row, icon| {
            row.child(
                svg()
                    .path(icon)
                    .size(px(9.))
                    .flex_none()
                    .text_color(if highlighted {
                        rgb(ACCENT_BLUE).into()
                    } else {
                        pal.text_muted
                    }),
            )
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
        )
}

/// The widest the render may get. A board without cards beside it keeps
/// [`KEYBOARD_W`]. One with them gives up as much width as keeps the whole
/// pane inside the Keys tab, but never more than the pane would have
/// overflowed with the render alone, so the band never makes it wider.
fn keyboard_max_width(viewport_w: f32, side_bands_w: f32) -> f32 {
    let room = (viewport_w - KEYS_HORIZONTAL_RESERVE).max(KEYBOARD_W);
    KEYBOARD_W.min(room - side_bands_w)
}

/// The keyboard render size: the actual PNG aspect at up to `max_w` wide,
/// shrunk to fit the viewport height. Sizing off the real aspect keeps
/// `ObjectFit::Contain` from letterboxing and keeps the marker overlays
/// registered with the rendered keys — the G513 render (with wrist rest) is
/// nearly twice as tall as an MX Keys render at the same width.
fn keyboard_render_size(asset: Option<&ResolvedAsset>, viewport_h: f32, max_w: f32) -> (f32, f32) {
    let Some(asset) = asset.filter(|a| a.png_height > 0) else {
        return FALLBACK_KEYBOARD_SIZE;
    };
    let target_h = (viewport_h - KEYS_VERTICAL_RESERVE - CALLOUT_BAND_H).max(KEYBOARD_MIN_IMG_H);
    asset_dimensions_for_png(asset, target_h, max_w)
}

/// One key with its resolved layout + binding.
#[derive(Clone)]
struct KeySlot {
    idx: usize,
    label: SharedString,
    target: KeyTarget,
    x_frac: f32,
    y_frac: f32,
    binding: SharedString,
    binding_icon: Option<&'static str>,
    /// The glyph the keypad's LCD shows for this key's binding — the agent
    /// paints exactly this icon on the device, so the render mirrors it.
    lcd_icon: Option<&'static str>,
}

/// One [`KeySlot`] per displayed key, each carrying the action it is bound
/// to right now — the panes and the config panel render the same truth
/// from the same source.
/// The tab's keys with their current bindings, grid keys read on `page`.
fn key_slots_from_layouts(
    surface: KeySurface,
    state: Option<&AppState>,
    page: KeypadPage,
) -> Vec<KeySlot> {
    let key_actions = state.map(AppState::keypad_key_actions).unwrap_or_default();
    surface
        .slots()
        .into_iter()
        .enumerate()
        .map(|(idx, mut layout)| {
            layout.target = layout.target.on_page(page);
            let bound = match &layout.target {
                // The face the keypad shows, which a long press can set
                // without a press action.
                KeyTarget::Grid(slot) => key_actions.get(&ButtonId::Grid(*slot)).cloned(),
                target => state.and_then(|state| bound_action(state, target)),
            };
            KeySlot {
                idx,
                label: layout.legend.into(),
                x_frac: layout.point.x_frac,
                y_frac: layout.point.y_frac,
                binding: binding_label(bound.as_ref()),
                binding_icon: bound.as_ref().map(action_icon_path),
                lcd_icon: bound
                    .as_ref()
                    .map(|action| ActionRingIcon::for_action(action).asset_path()),
                target: layout.target,
            }
        })
        .collect()
}

/// The action `target` is bound to in the selected device's open profile:
/// a per-device binding (a control or keypad key) or the global F-row map
/// (a function key). `None` when the key is on its native function. A
/// control bound to [`Action::None`] is suppressed, so that is its action; a
/// keypad key is only ever observed, so [`Action::None`] there does nothing.
pub(super) fn bound_action(state: &AppState, target: &KeyTarget) -> Option<Action> {
    match target {
        KeyTarget::Control(button) => state.button_bindings().get(button).cloned(),
        KeyTarget::Grid(slot) => state
            .button_bindings()
            .get(&ButtonId::Grid(*slot))
            .filter(|action| **action != Action::None)
            .cloned(),
        KeyTarget::FunctionKey(trigger) => state.keyboard_bindings().get(trigger).cloned(),
    }
}

/// Persist `action` for `target`: controls and keypad keys bind per device
/// (and per open app profile) like any mouse button; a function key binds
/// globally. `None` removes the assignment in this scope (inheriting the
/// device default in an app profile); for a function key it clears the global
/// trigger.
pub(super) fn commit_key_action(
    state: &mut AppState,
    target: &KeyTarget,
    action: Option<Action>,
) -> StateEvents {
    match target {
        KeyTarget::Control(button) => match action {
            Some(action) => state.commit_binding(*button, action),
            None => state.clear_binding(*button),
        },
        KeyTarget::Grid(slot) => match action {
            Some(action) => state.commit_binding(ButtonId::Grid(*slot), action),
            None => state.clear_binding(ButtonId::Grid(*slot)),
        },
        KeyTarget::FunctionKey(trigger) => state.commit_keyboard_binding(trigger.clone(), action),
    }
}

/// The explicit action in the scope edited by the inspector. In an app profile,
/// an inherited action is not selected; `None` selects the default-profile row.
fn selected_key_action(state: &AppState, target: &KeyTarget) -> Option<Action> {
    let button = match target {
        KeyTarget::Control(button) => *button,
        KeyTarget::Grid(slot) => ButtonId::Grid(*slot),
        KeyTarget::FunctionKey(_) => return bound_action(state, target),
    };
    if state.editing_app().is_some() {
        state
            .editing_app_overrides()
            .and_then(|overrides| overrides.get(&button))
            .cloned()
    } else {
        bound_action(state, target)
    }
}

/// The two-pane row: keyboard photo (or keypad keycaps) + an optional side
/// panel.
#[derive(IntoElement)]
struct InspectorRow {
    keyboard: AnyElement,
    panel: Option<gpui::Div>,
}

impl InspectorRow {
    fn new(keyboard: impl Into<AnyElement>) -> Self {
        Self {
            keyboard: keyboard.into(),
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
        let markers: Vec<(f32, f32)> = self
            .slots
            .iter()
            .map(|slot| (slot.x_frac, slot.y_frac))
            .collect();
        let layout = callout_layout(&markers, img_w, img_h);
        let image_left = layout.image_left;

        div()
            .relative()
            .w(px(layout.pane_w))
            .h(px(CALLOUT_BAND_H + img_h))
            .child(
            div()
                .absolute()
                .top(px(CALLOUT_BAND_H))
                .left(px(image_left))
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
            .child(keyboard_leader_canvas(&layout, selected, hovered, img_h))
            .children({
                let view_for_callouts = view_clone.clone();
                let cards = layout.cards.clone();
                self.slots.iter().cloned().map(move |slot| {
                    let highlighted = key_is_highlighted(slot.idx, selected, hovered);
                    KeyCallout {
                        card: cards[slot.idx],
                        slot,
                        highlighted,
                        view: view_for_callouts.clone(),
                    }
                })
            })
            // Click-targets overlay, centered on each key's marker point.
            .child(
                div()
                    .absolute()
                    .top(px(CALLOUT_BAND_H))
                    .left(px(image_left))
                    .w(px(img_w))
                    .h(px(img_h))
                    .children(self.slots.into_iter().map(|slot| {
                    let highlighted = key_is_highlighted(slot.idx, selected, hovered);
                    key_click_target(slot, highlighted, (img_w, img_h), &view_clone)
                })),
            )
    }
}

/// One callout bubble, above its key or beside it (see [`callout_layout`]).
#[derive(IntoElement)]
struct KeyCallout {
    slot: KeySlot,
    card: CardRect,
    highlighted: bool,
    view: Entity<FunctionRowView>,
}

impl RenderOnce for KeyCallout {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let idx = self.slot.idx;
        let CardRect { left, top, width } = self.card;
        let view_hover = self.view.clone();
        let view_click = self.view;
        let binding = self.slot.binding;
        let binding_icon = self.slot.binding_icon;
        let highlighted = self.highlighted;
        let full_label = self.slot.label.clone();

        v_flex()
            .id(("key-callout", idx))
            // The bubble itself truncates the key name to one line (#1687 —
            // a long translation, e.g. German's compound names, wrapped to
            // several lines and overflowed into neighbouring callouts); the
            // tooltip is where the untruncated name stays readable.
            .tooltip(move |window, cx| Tooltip::new(full_label.clone()).build(window, cx))
            .absolute()
            .top(px(top))
            .left(px(left))
            .w(px(width))
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
                    .max_w(px(width - 8.))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_caption()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if highlighted {
                        rgb(ACCENT_BLUE).into()
                    } else {
                        pal.text_primary
                    })
                    .child(self.slot.label.clone()),
            )
            .child(binding_line(
                binding,
                binding_icon,
                width - 8.,
                highlighted,
                &pal,
            ))
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
        None => tr!("common.off"),
    }
}

fn keyboard_leader_canvas(
    layout: &CalloutLayout,
    selected: Option<usize>,
    hovered: Option<usize>,
    img_h: f32,
) -> impl IntoElement {
    let leaders = layout.leaders.clone();
    canvas(
        move |_bounds, _, _| (leaders, selected, hovered),
        move |bounds, payload, window, _app| {
            let (leaders, selected, hovered) = payload;
            paint_keyboard_leaders(bounds, &leaders, selected, hovered, window);
        },
    )
    .absolute()
    .inset_0()
    .w(px(layout.pane_w))
    .h(px(CALLOUT_BAND_H + img_h))
}

fn paint_keyboard_leaders(
    bounds: Bounds<gpui::Pixels>,
    leaders: &[Vec<(f32, f32)>],
    selected: Option<usize>,
    hovered: Option<usize>,
    window: &mut Window,
) {
    for (idx, corners) in leaders.iter().enumerate() {
        let highlighted = key_is_highlighted(idx, selected, hovered);
        let mut path = PathBuilder::stroke(if highlighted { px(2.) } else { px(1.) });
        for (i, &(x, y)) in corners.iter().enumerate() {
            let corner = bounds.origin + point(px(x), px(y));
            if i == 0 {
                path.move_to(corner);
            } else {
                path.line_to(corner);
            }
        }
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

fn key_target_left_px(x_frac: f32, img_w: f32, target_w: f32) -> f32 {
    (x_frac * img_w - target_w / 2.0).clamp(0.0, img_w - target_w)
}

fn key_target_top_px(y_frac: f32, img_h: f32, target_h: f32) -> f32 {
    (y_frac * img_h - target_h / 2.0).clamp(0.0, img_h - target_h)
}

fn binding_after_pick(current: Option<&Action>, picked: Action) -> Option<Action> {
    (current != Some(&picked)).then_some(picked)
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
        let target = slot.target.clone();
        let key_name = slot.label.clone();

        // If an editor is active, render it instead of the list.
        if let Some(kind) = self.active_editor {
            return super::editors::editor_card(
                target,
                kind,
                self.text_state.clone(),
                self.workflow_draft.clone(),
                self.application_invalid,
                view,
                pal,
            );
        }

        let state = AppState::try_read(cx);
        let current = state.and_then(|state| selected_key_action(state, &target));
        let uses_default = current.is_none();
        // A control or keypad key binds inside the open app profile, like a
        // mouse button.
        let app = match target {
            KeyTarget::Control(_) | KeyTarget::Grid(_) => {
                state.and_then(|state| state.editing_app().map(|app| profile_name(state, app)))
            }
            KeyTarget::FunctionKey(_) => None,
        };

        let view_for_pick = view.clone();
        let target_for_pick = target.clone();
        let current_for_pick = current.clone();
        let on_pick: PickFn = Rc::new(move |action, _window, cx| {
            AppState::apply(cx, |state| {
                commit_key_action(
                    state,
                    &target_for_pick,
                    binding_after_pick(current_for_pick.as_ref(), action),
                )
            });
            view_for_pick.update(cx, |_, vcx| vcx.notify());
        });

        let mut rows = Vec::new();
        // Unbound is not a catalog action. A control rests on its firmware
        // function (#1172); a function key rests on the key itself. The row
        // clears the assignment, inheriting the device default in an app profile.
        let view_for_native = view.clone();
        let target_for_native = target.clone();
        rows.push(
            v_flex().child(
                MenuRow::new("panel-native")
                    .debug_selector(|| "key-panel-native".into())
                    .selected(uses_default)
                    .role(Role::MenuItem)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                svg()
                                    .path("action-icons/keyboard.svg")
                                    .size_4()
                                    .flex_none()
                                    .text_color(pal.text_muted),
                            )
                            .child(div().child(if app.is_some() {
                                tr!("profiles.use_the_default_profile")
                            } else {
                                tr!("keyboard.native_function")
                            })),
                    )
                    .when(uses_default, |row| {
                        row.child(
                            div()
                                .debug_selector(|| "key-panel-native-check".into())
                                .flex_none()
                                .size_3()
                                .child(
                                    gpui_component::Icon::new(gpui_component::IconName::Check)
                                        .size_3()
                                        .text_color(rgb(ACCENT_BLUE)),
                                ),
                        )
                    })
                    .on_click(move |_ev, _window, cx| {
                        AppState::apply(cx, |state| {
                            commit_key_action(state, &target_for_native, None)
                        });
                        view_for_native.update(cx, |_, vcx| vcx.notify());
                    }),
            ),
        );
        rows.extend(panel_action_rows(current.as_ref(), &on_pick, view, &pal));

        compact_panel(pal)
            .w(px(PANEL_W))
            .max_h(px(500.))
            .child(title_header(&key_name, app.as_deref(), &pal))
            .child(divider(pal))
            .child(editor_scroll_list("key-panel-scroll", rows))
    }
}

/// The panel's title — which key is selected, e.g. "F1" or "Screen Capture
/// Key", and the app profile it binds in when one is open.
fn title_header(key_name: &SharedString, app: Option<&str>, pal: &Palette) -> impl IntoElement {
    let title = match app {
        Some(app) => tr!("actions.bind_control_in_app", name => key_name.clone(), app => app),
        None => tr!("actions.bind_control", name => key_name.clone()),
    };
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
                .child(title),
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

    let power_user_actions: &[(PowerUserKind, SharedString, &'static str)] = &[
        (
            PowerUserKind::OpenApplication,
            tr!("action_ring.open_application_or_folder"),
            "action-icons/grid-3x3.svg",
        ),
        (
            PowerUserKind::TypeText,
            tr!("actions.type_text"),
            "action-icons/keyboard.svg",
        ),
        (
            PowerUserKind::RunAppleScript,
            tr!("actions.run_applescript"),
            "action-icons/terminal.svg",
        ),
        (
            PowerUserKind::RunShellCommand,
            tr!("actions.run_shell_command"),
            "action-icons/terminal.svg",
        ),
        (
            PowerUserKind::Workflow,
            tr!("actions.workflow"),
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
                    let selected = power_user_selected(current, kind);
                    MenuRow::new(format!("panel-power-{idx}"))
                        .selected(selected)
                        .role(Role::MenuItem)
                        .aria_label(label.clone())
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
                                .child(div().child(label.clone())),
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

/// Whether `current` is the action the `kind` editor makes, so its row reads
/// as the key's binding.
fn power_user_selected(current: Option<&Action>, kind: PowerUserKind) -> bool {
    matches!(
        (current, kind),
        (
            Some(Action::OpenApplication(_)),
            PowerUserKind::OpenApplication
        ) | (Some(Action::TypeText(_)), PowerUserKind::TypeText)
            | (
                Some(Action::RunAppleScript(_)),
                PowerUserKind::RunAppleScript
            )
            | (
                Some(Action::RunShellCommand(_)),
                PowerUserKind::RunShellCommand
            )
            | (Some(Action::Workflow(_)), PowerUserKind::Workflow)
    )
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
