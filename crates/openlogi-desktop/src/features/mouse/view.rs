use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FocusHandle, Focusable, Hsla,
    InteractiveElement, IntoElement, ParentElement, Render, RenderOnce,
    StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, hsla, img,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_base::Button as BaseButton;
use gpui_component::{
    Icon, IconName, h_flex,
    input::{InputEvent, InputState},
    v_flex,
};
use openlogi_core::binding::{Action, ButtonId, GestureDirection, default_binding};
use openlogi_ipc::ShortcutRecordingPhase;

use super::geometry::{
    LabelDistribution, asset_dimensions_for_png, asset_has_button_labels, asset_hotspots_for_png,
    default_labels, labels_from_hotspots,
};
use super::hotspots::{Hotspot, MOUSE_MODEL_SIZE, MouseControlId, default_hotspots};
use super::inspector::{BindingInspectorData, binding_inspector};
use super::leader_lines::{Geometry as LeaderGeometry, Label, Side, paint as paint_leader_lines};
use super::picker::{GESTURE_BUTTON_ICON, action_icon_path};
use super::thumbwheel::ThumbwheelPreset;
use crate::app::{glow_canvas, keyboard_glow};
use crate::features::profile_scope::{friendly_app_name, profile_canvas_status};
use crate::services::assets::{GlowGeometry, ResolvedAsset};
use crate::state::{AppState, StateEvent};
use crate::ui::action::localized_action_label;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};

const SIDE_GAP: f32 = 24.;
const LABEL_W: f32 = 156.;
const LABEL_H: f32 = 56.;
const LABEL_GUTTER: f32 = LABEL_W + SIDE_GAP;
const TWO_SIDED_LABEL_MIN_W: f32 = 700.;

const CARD_EDGE_INSET: f32 = SIDE_GAP;

const HOTSPOT_DOT: f32 = 12.;
/// Vertical space occupied by the device bar, profile context, and canvas
/// padding. Normal operation no longer reserves a footer.
const MODEL_VERTICAL_RESERVE: f32 = 154.;
/// Floor for the scaled model height. Below this the evenly-slotted side labels
/// (≈[`LABEL_H`] each) start to overlap; the window's minimum height is sized to
/// keep the viewport above [`MODEL_VERTICAL_RESERVE`] + this.
const MODEL_MIN_H: f32 = 360.;

/// Max width the model (side gutter + image) may occupy, matching the
/// `buttons_tab` content cap so a wide keyboard image never overflows the panel.
const MODEL_CONTENT_MAX_W: f32 = 760.;
/// Horizontal chrome the model can't draw into (the buttons-tab padding).
const MODEL_HORIZONTAL_RESERVE: f32 =
    crate::ui::theme::DETAIL_RAIL_W + super::inspector::INSPECTOR_W + 48.;
/// Floor for the model's available width on a narrow window.
const MODEL_MIN_CONTENT_W: f32 = 200.;

/// Binding destination captured when physical shortcut recording begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ShortcutBindingTarget {
    Button(ButtonId),
    Gesture(ButtonId, GestureDirection),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ShortcutBindingContext {
    device_key: String,
    editing_app: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingShortcutBinding {
    target: ShortcutBindingTarget,
    context: ShortcutBindingContext,
    previous_session_id: Option<u64>,
    session_id: Option<u64>,
}

struct MouseWorkspaceData<'a> {
    asset: Option<&'a ResolvedAsset>,
    active: Option<MouseControlId>,
    bindings: &'a BTreeMap<ButtonId, Action>,
    gesture_maps: &'a BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    glow: Option<(Arc<GlowGeometry>, Hsla)>,
    thumbwheel: bool,
    editing_app: Option<String>,
    overridden: Option<&'a BTreeMap<ButtonId, Action>>,
}

impl<'a> MouseWorkspaceData<'a> {
    fn read(cx: &'a App) -> Option<Self> {
        AppState::try_read(cx).map(|state| Self {
            asset: state
                .current_record()
                .and_then(|record| record.asset.as_ref()),
            active: state.active_button.map(MouseControlId::from_active_button),
            bindings: &state.button_bindings,
            gesture_maps: &state.gesture_bindings,
            glow: state
                .current_record()
                .and_then(|record| keyboard_glow(state, record)),
            thumbwheel: state
                .current_record()
                .and_then(|record| record.capabilities)
                .is_some_and(|capabilities| capabilities.thumbwheel),
            editing_app: state.editing_app().map(|app| {
                state
                    .recent_app_name(app)
                    .map_or_else(|| friendly_app_name(app), str::to_string)
            }),
            overridden: state.editing_app_overrides(),
        })
    }

    fn empty(
        bindings: &'a BTreeMap<ButtonId, Action>,
        gesture_maps: &'a BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    ) -> Self {
        Self {
            asset: None,
            active: None,
            bindings,
            gesture_maps,
            glow: None,
            thumbwheel: false,
            editing_app: None,
            overridden: None,
        }
    }
}

/// Interactive mouse model with button hotspots.
pub struct MouseModelView {
    focus_handle: FocusHandle,
    current_device_key: Option<String>,
    hovered: Option<MouseControlId>,
    selected: Option<MouseControlId>,
    /// The gesture direction whose action is open in the fixed inspector.
    gesture_active_dir: Option<GestureDirection>,
    action_picker_open: bool,
    action_search: Entity<InputState>,
    pending_shortcut: Option<PendingShortcutBinding>,
    _state_obs: Subscription,
}

impl MouseModelView {
    /// Create the mouse model view.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let action_search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("Search actions…")));
        cx.subscribe(&action_search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let state = AppState::global(cx);
        let state_obs = cx.subscribe(&state, |view, _, event: &StateEvent, cx| {
            if matches!(event, StateEvent::ShortcutRecordingChanged) {
                view.handle_shortcut_recording(cx);
            } else if matches!(
                event,
                StateEvent::InventoryChanged
                    | StateEvent::DeviceSelected(_)
                    | StateEvent::BindingsChanged(_)
            ) {
                view.cancel_stale_shortcut(cx);
            }
            let relevant = match event {
                StateEvent::InventoryChanged
                | StateEvent::DeviceSelected(_)
                | StateEvent::ForegroundChanged
                | StateEvent::ShortcutRecordingChanged => true,
                StateEvent::BindingsChanged(key) | StateEvent::LightingChanged(key) => {
                    AppState::try_read(cx)
                        .and_then(AppState::current_record)
                        .is_some_and(|record| record.device_key() == *key)
                }
                _ => false,
            };
            if relevant {
                cx.notify();
            }
        });
        Self {
            focus_handle: cx.focus_handle(),
            current_device_key: None,
            hovered: None,
            selected: None,
            gesture_active_dir: None,
            action_picker_open: false,
            action_search,
            pending_shortcut: None,
            _state_obs: state_obs,
        }
    }

    /// Set (or clear, with `None`) the activated gesture direction. Callers must
    /// `cx.notify()` to re-render.
    pub(crate) fn set_gesture_selected_dir(&mut self, dir: Option<GestureDirection>, cx: &mut App) {
        if self.gesture_active_dir != dir {
            self.cancel_pending_shortcut(cx);
        }
        self.gesture_active_dir = dir;
        self.action_picker_open = false;
    }

    pub(super) fn toggle_action_picker(&mut self, cx: &mut App) {
        self.action_picker_open = !self.action_picker_open;
        if !self.action_picker_open {
            self.cancel_pending_shortcut(cx);
        }
    }

    pub(super) fn close_action_picker(&mut self, cx: &mut App) {
        self.action_picker_open = false;
        self.cancel_pending_shortcut(cx);
    }

    fn reset_for_device(&mut self, device_key: Option<&str>, cx: &mut App) {
        if self.current_device_key.as_deref() == device_key {
            return;
        }
        self.cancel_pending_shortcut(cx);
        self.current_device_key = device_key.map(str::to_string);
        self.hovered = None;
        self.selected = None;
        self.gesture_active_dir = None;
        self.action_picker_open = false;
    }

    fn select(&mut self, control: MouseControlId, cx: &mut App) {
        if self.selected != Some(control) {
            self.cancel_pending_shortcut(cx);
            self.selected = Some(control);
            self.gesture_active_dir = None;
            self.action_picker_open = false;
        }
    }

    pub(super) fn begin_shortcut_recording(&mut self, target: ShortcutBindingTarget, cx: &mut App) {
        let Some(context) = shortcut_binding_context(cx) else {
            return;
        };
        self.pending_shortcut = Some(PendingShortcutBinding {
            target,
            context,
            previous_session_id: AppState::try_read(cx)
                .and_then(AppState::shortcut_recording)
                .map(|recording| recording.session_id),
            session_id: None,
        });
        if let Some(state) = AppState::try_read(cx) {
            state.start_shortcut_recording();
        }
    }

    pub(super) fn shortcut_phase(
        &self,
        target: ShortcutBindingTarget,
        cx: &App,
    ) -> Option<ShortcutRecordingPhase> {
        let pending = self
            .pending_shortcut
            .as_ref()
            .filter(|pending| pending.target == target)?;
        let Some(session_id) = pending.session_id else {
            return Some(ShortcutRecordingPhase::Recording);
        };
        AppState::try_read(cx)
            .and_then(AppState::shortcut_recording)
            .filter(|recording| recording.session_id == session_id)
            .map(|recording| recording.phase.clone())
    }

    pub(crate) fn cancel_pending_shortcut(&mut self, cx: &mut App) {
        if self.pending_shortcut.take().is_some()
            && let Some(state) = AppState::try_read(cx)
        {
            state.cancel_shortcut_recording();
        }
    }

    fn cancel_stale_shortcut(&mut self, cx: &mut App) {
        let stale = self.pending_shortcut.as_ref().is_some_and(|pending| {
            shortcut_binding_context(cx).as_ref() != Some(&pending.context)
                || !self.target_is_selected(pending.target)
        });
        if stale {
            self.cancel_pending_shortcut(cx);
        }
    }

    fn target_is_selected(&self, target: ShortcutBindingTarget) -> bool {
        match target {
            ShortcutBindingTarget::Button(button) => {
                self.selected == Some(MouseControlId::Button(button))
            }
            ShortcutBindingTarget::Gesture(button, direction) => {
                self.selected == Some(MouseControlId::Button(button))
                    && self.gesture_active_dir == Some(direction)
            }
        }
    }

    fn handle_shortcut_recording(&mut self, cx: &mut App) {
        self.cancel_stale_shortcut(cx);
        let Some(pending) = self.pending_shortcut.as_mut() else {
            return;
        };
        let recording = AppState::try_read(cx)
            .and_then(AppState::shortcut_recording)
            .cloned();
        let Some(recording) = recording else {
            if pending.session_id.is_some() {
                self.pending_shortcut = None;
            }
            return;
        };
        match pending.session_id {
            None if recording.session_id == 0
                && recording.phase == ShortcutRecordingPhase::Unavailable =>
            {
                pending.session_id = Some(0);
            }
            None if Some(recording.session_id) != pending.previous_session_id
                && recording.phase == ShortcutRecordingPhase::Recording =>
            {
                pending.session_id = Some(recording.session_id);
            }
            None => return,
            // A snapshot captured before the start acknowledgement can still
            // be delivered by another observer. Ignore older sessions; only a
            // genuinely newer owner supersedes this pending destination.
            Some(session_id) if recording.session_id < session_id => return,
            Some(session_id) if recording.session_id > session_id => {
                self.pending_shortcut = None;
                return;
            }
            Some(_) => {}
        }

        let ShortcutRecordingPhase::Complete(combo) = recording.phase else {
            return;
        };
        let target = pending.target;
        self.pending_shortcut = None;
        self.action_picker_open = false;
        AppState::update_bindings(cx, |state| match target {
            ShortcutBindingTarget::Button(button) => {
                state.commit_binding(button, Action::CustomShortcut(combo));
            }
            ShortcutBindingTarget::Gesture(button, direction) => {
                state.commit_gesture_binding(button, direction, Action::CustomShortcut(combo));
            }
        });
        if let Some(state) = AppState::try_read(cx) {
            state.cancel_shortcut_recording();
        }
    }
}

fn shortcut_binding_context(cx: &App) -> Option<ShortcutBindingContext> {
    let state = AppState::try_read(cx)?;
    Some(ShortcutBindingContext {
        device_key: state.current_record()?.config_key.clone(),
        editing_app: state.editing_app().map(str::to_string),
    })
}

impl Focusable for MouseModelView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn set_control_hovered(
    view: &Entity<MouseModelView>,
    control: MouseControlId,
    hovered: bool,
    cx: &mut App,
) {
    view.update(cx, |this, cx| {
        if hovered {
            this.hovered = Some(control);
        } else if this.hovered == Some(control) {
            this.hovered = None;
        }
        cx.notify();
    });
}

impl Render for MouseModelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let device_key = AppState::try_read(cx)
            .and_then(AppState::current_record)
            .map(|record| record.config_key.clone());
        self.reset_for_device(device_key.as_deref(), cx);

        let (empty_bindings, empty_gesture_maps) = (BTreeMap::new(), BTreeMap::new());
        let MouseWorkspaceData {
            asset,
            active,
            bindings,
            gesture_maps,
            glow,
            thumbwheel,
            editing_app,
            overridden,
        } = MouseWorkspaceData::read(cx)
            .unwrap_or_else(|| MouseWorkspaceData::empty(&empty_bindings, &empty_gesture_maps));

        let gesture_buttons: Vec<ButtonId> = gesture_maps
            .keys()
            .copied()
            .filter(|button| {
                editing_app.is_none()
                    || !overridden.is_some_and(|overrides| overrides.contains_key(button))
            })
            .collect();

        let viewport_h = f32::from(window.viewport_size().height);
        let viewport_w = f32::from(window.viewport_size().width);
        let ModelLayout {
            canvas_w,
            mouse_left,
            mouse_w,
            mouse_h,
            hotspots,
            labels,
        } = model_layout(asset, viewport_w, viewport_h, thumbwheel);
        let canvas_h = mouse_h;

        let highlight = self.hovered.or(active);
        let view = cx.entity();
        let hovered = self.hovered;
        let pal = theme::palette(cx);
        let profile_status = profile_canvas_status(pal, cx);

        let hotspots_outer = hotspots.clone();
        let labels_outer = labels.clone();
        let leader_canvas = leader_canvas(hotspots, labels, highlight, mouse_left, mouse_w);
        let breathing_art = breathing_art(asset, mouse_left, mouse_w, mouse_h, pal, glow);
        let model = ModelRect {
            left: mouse_left,
            width: mouse_w,
            height: mouse_h,
        };
        let hotspots_layer = hotspots_layer(
            &hotspots_outer,
            model,
            hovered,
            active,
            self.selected,
            &view,
        );
        let canvas = div()
            .relative()
            .w(px(canvas_w))
            .h(px(canvas_h))
            .child(breathing_art)
            .child(leader_canvas)
            .children(labels_outer.iter().enumerate().map(|(idx, label)| {
                let binding = binding_label_for_control(label.id, bindings, &gesture_buttons);
                label_control(
                    idx,
                    *label,
                    binding,
                    hovered == Some(label.id) || active == Some(label.id),
                    model,
                    self.selected == Some(label.id),
                    &view,
                )
            }))
            .child(hotspots_layer);

        let inspector = binding_inspector(
            BindingInspectorData {
                selected: self.selected,
                gesture_direction: self.gesture_active_dir,
                action_picker_open: self.action_picker_open,
                bindings,
                gesture_maps,
                editing_app: editing_app.as_deref(),
                overridden,
            },
            &self.action_search,
            &view,
            pal,
            cx,
        );
        workspace_layout(
            canvas.into_any_element(),
            profile_status,
            inspector,
            &self.focus_handle,
        )
    }
}

fn workspace_layout(
    canvas: AnyElement,
    profile_status: Option<AnyElement>,
    inspector: AnyElement,
    focus_handle: &FocusHandle,
) -> AnyElement {
    h_flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .items_stretch()
        .tab_group()
        .track_focus(focus_handle)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_hidden()
                .children(profile_status)
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_4()
                        .child(canvas),
                ),
        )
        .child(inspector)
        .into_any_element()
}

struct ModelLayout {
    canvas_w: f32,
    mouse_left: f32,
    mouse_w: f32,
    mouse_h: f32,
    hotspots: Vec<Hotspot>,
    labels: Vec<Label>,
}

/// Scale the model to fit the content area in both axes. A tall mouse is bound
/// by the viewport height; a wide keyboard is bound by the available width and
/// drops the label gutter so it remains centred.
fn model_layout(
    asset: Option<&ResolvedAsset>,
    viewport_w: f32,
    viewport_h: f32,
    thumbwheel: bool,
) -> ModelLayout {
    let target_h = (viewport_h - MODEL_VERTICAL_RESERVE).clamp(MODEL_MIN_H, MOUSE_MODEL_SIZE.1);
    let has_labels = asset.is_none_or(asset_has_button_labels) && viewport_w >= 960.;
    let content_w =
        (viewport_w - MODEL_HORIZONTAL_RESERVE).clamp(MODEL_MIN_CONTENT_W, MODEL_CONTENT_MAX_W);
    let label_distribution = if has_labels && content_w >= TWO_SIDED_LABEL_MIN_W {
        LabelDistribution::BothSides
    } else {
        LabelDistribution::LeftOnly
    };
    let left_gutter = if has_labels { LABEL_GUTTER } else { 0. };
    let right_gutter = if label_distribution == LabelDistribution::BothSides {
        LABEL_GUTTER
    } else {
        0.
    };
    let max_image_w = (content_w - left_gutter - right_gutter).max(MODEL_MIN_CONTENT_W / 2.);
    let (mouse_w, mouse_h, hotspots, mut labels) =
        scaled_model(asset, target_h, max_image_w, thumbwheel, label_distribution);
    if !has_labels {
        labels.clear();
    }

    ModelLayout {
        canvas_w: left_gutter + mouse_w + right_gutter,
        mouse_left: left_gutter,
        mouse_w,
        mouse_h,
        hotspots,
        labels,
    }
}

/// Model geometry fit inside a `max_w` × `target_h` box. With a real asset the
/// hotspots and labels are recomputed from the scaled dimensions; the synthetic
/// silhouette's authored coordinates are scaled by the same factor. Returns
/// `(mouse_w, mouse_h, hotspots, labels)`.
fn scaled_model(
    asset: Option<&ResolvedAsset>,
    target_h: f32,
    max_w: f32,
    thumbwheel: bool,
    label_distribution: LabelDistribution,
) -> (f32, f32, Vec<Hotspot>, Vec<Label>) {
    if let Some(a) = asset {
        let (w, h) = asset_dimensions_for_png(a, target_h, max_w);
        let hotspots = asset_hotspots_for_png(a, w, h);
        let labels = labels_from_hotspots(&hotspots, h, label_distribution);
        (w, h, hotspots, labels)
    } else {
        let scale = (target_h / MOUSE_MODEL_SIZE.1).min(max_w / MOUSE_MODEL_SIZE.0);
        let hotspots = default_hotspots(thumbwheel)
            .into_iter()
            .map(|hs| Hotspot {
                x: hs.x * scale,
                y: hs.y * scale,
                w: hs.w * scale,
                h: hs.h * scale,
                ..hs
            })
            .collect();
        let labels = default_labels(thumbwheel, label_distribution)
            .into_iter()
            .map(|l| Label {
                y: l.y * scale,
                ..l
            })
            .collect();
        (
            MOUSE_MODEL_SIZE.0 * scale,
            MOUSE_MODEL_SIZE.1 * scale,
            hotspots,
            labels,
        )
    }
}

fn leader_canvas(
    hotspots: Vec<Hotspot>,
    labels: Vec<Label>,
    highlight: Option<MouseControlId>,
    mouse_left: f32,
    mouse_w: f32,
) -> impl IntoElement {
    canvas(
        move |_bounds, _, _| (hotspots, labels, highlight),
        move |bounds, payload, window, _app| {
            let (hotspots, labels, highlight) = payload;
            paint_leader_lines(
                bounds,
                LeaderGeometry {
                    mouse_origin: gpui::point(px(mouse_left), px(0.)),
                    mouse_w,
                    card_edge_inset: CARD_EDGE_INSET,
                },
                &hotspots,
                &labels,
                highlight,
                window,
            );
        },
    )
    .size_full()
}

fn breathing_art(
    asset: Option<&ResolvedAsset>,
    mouse_left: f32,
    mouse_w: f32,
    mouse_h: f32,
    pal: Palette,
    glow: Option<(Arc<GlowGeometry>, Hsla)>,
) -> impl IntoElement {
    let device_art: AnyElement = match asset {
        Some(a) => img(a.image_path.clone())
            .w(px(mouse_w))
            .h(px(mouse_h))
            .into_any_element(),
        None => silhouette(mouse_w, mouse_h, pal).into_any_element(),
    };
    div()
        .absolute()
        .left(px(mouse_left))
        .top(px(0.))
        .w(px(mouse_w))
        .h(px(mouse_h))
        // Paint the keyboard's RGB *behind* the render so the opaque keys occlude
        // it and the colour only reads through the inter-key gaps — light from
        // behind, not specks on top. Same effect as the home gallery, scaled to
        // this render with no pre-baked PNG (#272).
        .when_some(glow, |this, (geom, color)| {
            this.child(glow_canvas(geom, color))
        })
        .child(device_art)
}

#[derive(Clone, Copy)]
struct ModelRect {
    left: f32,
    width: f32,
    height: f32,
}

fn hotspots_layer(
    hotspots: &[Hotspot],
    model: ModelRect,
    hovered: Option<MouseControlId>,
    active: Option<MouseControlId>,
    selected: Option<MouseControlId>,
    view: &Entity<MouseModelView>,
) -> impl IntoElement {
    div()
        .absolute()
        .left(px(model.left))
        .top(px(0.))
        .w(px(model.width))
        .h(px(model.height))
        .children(hotspots.iter().enumerate().map(|(idx, hotspot)| {
            hotspot_control(
                idx,
                *hotspot,
                hovered,
                active,
                selected == Some(hotspot.id),
                view,
            )
        }))
}

/// Position a selectable control card at the label's slot in the side gutter.
/// Selection updates the fixed inspector; labels never own editor overlays.
fn label_control(
    idx: usize,
    label: Label,
    binding: BindingLabel,
    highlighted: bool,
    model: ModelRect,
    selected: bool,
    view: &Entity<MouseModelView>,
) -> AnyElement {
    let x = match label.side {
        Side::Left => model.left - SIDE_GAP - LABEL_W,
        Side::Right => model.left + model.width + SIDE_GAP,
    };
    let view = view.clone();
    let trigger = LabelTrigger {
        id: ("label-trigger", idx).into(),
        label,
        binding,
        highlighted,
        selected,
        view,
    };
    div()
        .absolute()
        .left(px(x))
        .top(px(label.y - LABEL_H / 2.))
        .w(px(LABEL_W))
        .h(px(LABEL_H))
        .child(trigger)
        .into_any_element()
}

struct BindingLabel {
    text: gpui::SharedString,
    /// Vendored action-icon asset path (see [`action_icon_path`]) for the
    /// card's leading glyph, or `None` for the gesture summary / unbound.
    icon: Option<&'static str>,
}

#[derive(IntoElement)]
struct LabelTrigger {
    id: ElementId,
    label: Label,
    binding: BindingLabel,
    highlighted: bool,
    selected: bool,
    view: Entity<MouseModelView>,
}

impl RenderOnce for LabelTrigger {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let highlighted = self.highlighted || self.selected;
        let selected = self.selected;
        let btn = self.label.id;
        let view = self.view;
        let click_view = view.clone();
        let pal = theme::palette(cx);
        let binding_color = if highlighted {
            rgb(ACCENT_BLUE).into()
        } else {
            pal.text_primary
        };
        // Always show the action the button actually performs. Default and
        // customised bindings use the same neutral value colour; only the
        // actively highlighted control takes the accent.
        let binding = self.binding.text;
        let binding_description = binding.clone();
        let binding_icon = self.binding.icon;
        let button_name = tr!(self.label.id.label());
        BaseButton::new(self.id)
            .selected(selected)
            .accessibility_label(tr!("Bind %{name}", name => button_name.clone()))
            .aria_description(binding_description)
            .aria_selected(selected)
            .flex()
            .flex_col()
            .w(px(LABEL_W))
            .h(px(LABEL_H))
            .px_3()
            .justify_center()
            .gap_0p5()
            .rounded(pal.control_radius)
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
            .focus_visible(move |s| {
                s.bg(if highlighted {
                    theme::accent_tint_hover()
                } else {
                    pal.control_hover
                })
                .border_color(rgb(ACCENT_BLUE))
            })
            // Button name — the caption (xs / muted), the same size as the
            // inspector title and category headers it shares the binding flow with.
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(button_name),
            )
            // Current binding — the value (sm), the same size as the action rows
            // it edits.
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    // Leading action icon (same glyph as the picker rows), tinted
                    // with the value so it tracks the default / set / highlighted
                    // state. Absent for the gesture summary / unbound.
                    .when_some(binding_icon, |row, path| {
                        row.child(
                            svg()
                                .path(path)
                                .size_4()
                                .flex_none()
                                .text_color(binding_color),
                        )
                    })
                    .child(
                        // Shrink + ellipsis so a long action name (e.g. "Mission
                        // Control") doesn't push the chevron out of the fixed card.
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_body()
                            .text_color(binding_color)
                            .child(binding),
                    )
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size_3()
                            .text_color(pal.text_muted),
                    ),
            )
            .on_click(move |_event, _window, cx| {
                click_view.update(cx, |this, cx| {
                    this.select(btn, cx);
                    cx.notify();
                });
            })
            .on_hover(move |hovered, _window, cx| {
                set_control_hovered(&view, btn, *hovered, cx);
            })
    }
}

/// The label card's text and icon for one control.
fn binding_label_for_control(
    control: MouseControlId,
    bindings: &std::collections::BTreeMap<ButtonId, Action>,
    gesture_buttons: &[ButtonId],
) -> BindingLabel {
    if control
        .button()
        .is_some_and(|button| gesture_buttons.contains(&button))
    {
        return BindingLabel {
            text: tr!("5 directions"),
            icon: Some(GESTURE_BUTTON_ICON),
        };
    }

    match control {
        MouseControlId::Button(button) => {
            let action = bindings
                .get(&button)
                .cloned()
                .unwrap_or_else(|| default_binding(button));
            BindingLabel {
                text: localized_action_label(&action),
                icon: Some(action_icon_path(&action)),
            }
        }
        MouseControlId::ThumbwheelRotation => {
            let backward = bindings
                .get(&ButtonId::ThumbwheelScrollDown)
                .cloned()
                .unwrap_or_else(|| default_binding(ButtonId::ThumbwheelScrollDown));
            let forward = bindings
                .get(&ButtonId::ThumbwheelScrollUp)
                .cloned()
                .unwrap_or_else(|| default_binding(ButtonId::ThumbwheelScrollUp));
            if let Some(preset) = ThumbwheelPreset::recognize(&backward, &forward) {
                BindingLabel {
                    text: tr!(preset.label()),
                    icon: Some(preset.icon()),
                }
            } else {
                BindingLabel {
                    text: tr!("Custom"),
                    icon: Some("action-icons/chevrons-right.svg"),
                }
            }
        }
    }
}

/// Shape-based silhouette used when no asset is cached for the device.
///
/// Its `rounded_*` values are illustration proportions — the body shell and the
/// two drawn side buttons — not UI chrome, so they stay fixed rather than
/// tracking the `Palette` radius tokens the way real cards and controls do.
fn silhouette(w: f32, h: f32, pal: Palette) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .w(px(w))
        .h(px(h))
        .rounded_3xl()
        .border_1()
        .border_color(pal.text_muted)
        .bg(pal.muted)
        .child(
            div()
                .absolute()
                .left(px(w / 2. - 14.))
                .top(px(90.))
                .w(px(28.))
                .h(px(110.))
                .rounded_md()
                .bg(hsla(0., 0., 0.25, 1.0)),
        )
        .child(
            div()
                .absolute()
                .left(px(w / 2.))
                .top(px(20.))
                .w(px(1.))
                .h(px(240.))
                .bg(pal.border),
        )
        .child(
            div()
                .absolute()
                .left(px(8.))
                .top(px(210.))
                .w(px(34.))
                .h(px(150.))
                .rounded_md()
                .bg(hsla(0., 0., 0.25, 1.0)),
        )
}

fn hotspot_control(
    idx: usize,
    hotspot: Hotspot,
    hovered: Option<MouseControlId>,
    active: Option<MouseControlId>,
    selected: bool,
    view: &Entity<MouseModelView>,
) -> AnyElement {
    let view = view.clone();
    let trigger = HotspotTrigger {
        id: ("hotspot-trigger", idx).into(),
        hotspot,
        hovered: hovered == Some(hotspot.id) || active == Some(hotspot.id),
        view,
        selected,
    };
    div()
        .absolute()
        .left(px(hotspot.x))
        .top(px(hotspot.y))
        .w(px(hotspot.w))
        .h(px(hotspot.h))
        .child(trigger)
        .into_any_element()
}

#[derive(IntoElement)]
struct HotspotTrigger {
    id: ElementId,
    hotspot: Hotspot,
    hovered: bool,
    view: Entity<MouseModelView>,
    selected: bool,
}

impl RenderOnce for HotspotTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let highlighted = self.hovered || self.selected;
        let selected = self.selected;
        let view = self.view;
        let click_view = view.clone();
        let hotspot = self.hotspot;
        let btn = hotspot.id;

        BaseButton::new(self.id)
            .selected(selected)
            .accessibility_label(tr!("Bind %{name}", name => tr!(btn.label())))
            .aria_selected(selected)
            .flex()
            .items_center()
            .justify_center()
            .w(px(hotspot.w))
            .h(px(hotspot.h))
            .child(
                div()
                    .w(px(HOTSPOT_DOT))
                    .h(px(HOTSPOT_DOT))
                    .rounded_full()
                    .border_1()
                    .border_color(if highlighted {
                        gpui::Hsla::from(rgb(ACCENT_BLUE))
                    } else {
                        hsla(0., 0., 0.95, 0.85)
                    })
                    .bg(if highlighted {
                        gpui::Hsla::from(rgb(ACCENT_BLUE))
                    } else {
                        hsla(0., 0., 0.18, 0.85)
                    }),
            )
            .focus_visible(|style| {
                style
                    .rounded_full()
                    .border_2()
                    .border_color(rgb(ACCENT_BLUE))
            })
            .on_click(move |_event, _window, cx| {
                click_view.update(cx, |this, cx| {
                    this.select(btn, cx);
                    cx.notify();
                });
            })
            .on_hover(move |hovered, _window, cx| {
                set_control_hovered(&view, btn, *hovered, cx);
            })
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use openlogi_core::binding::KeyCombo;
    use openlogi_core::config::Config;
    use openlogi_core::device::{
        Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
        ReceiverInfo,
    };
    use openlogi_ipc::{ShortcutRecording, ShortcutRecordingPhase};

    use super::*;
    use crate::services::assets::AssetResolver;
    use crate::state::ConfigPersistence;

    fn install_app_state(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let cache = AssetResolver::new();
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::with_runtime(
                    Config::ephemeral(),
                    &[],
                    &[],
                    &cache,
                    &[],
                    ConfigPersistence::MemoryOnly,
                    commands,
                )
            });
            AppState::set_global(state, cx);
        });
    }

    fn known_mouse_inventory() -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "MX Master 3S".to_string(),
                vendor_id: 0x046d,
                product_id: 0xb023,
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
                codename: Some("MX Master 3S".to_string()),
                wpid: None,
                kind: DeviceKind::Mouse,
                online: true,
                battery: None,
                model_info: Some(DeviceModelInfo {
                    entity_count: 1,
                    serial_number: None,
                    unit_id: [0xa3, 0x93, 0xca, 0xe0],
                    transports: DeviceTransports::default(),
                    model_ids: [0xb034, 0, 0],
                    extended_model_id: 2,
                }),
                capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
            }],
        }
    }

    fn install_known_mouse_state(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let cache = AssetResolver::new();
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::with_runtime(
                    Config::ephemeral(),
                    &[known_mouse_inventory()],
                    &[],
                    &cache,
                    &[],
                    ConfigPersistence::MemoryOnly,
                    commands,
                )
            });
            AppState::set_global(state, cx);
        });
    }

    fn set_recording(cx: &mut App, session_id: u64, phase: ShortcutRecordingPhase) {
        AppState::update(cx, |state, _| {
            state.set_shortcut_recording(Some(ShortcutRecording { session_id, phase }));
        });
    }

    fn publish_recording(cx: &mut App, session_id: u64, phase: ShortcutRecordingPhase) {
        AppState::update(cx, |state, cx| {
            state.set_shortcut_recording(Some(ShortcutRecording { session_id, phase }));
            cx.emit(StateEvent::ShortcutRecordingChanged);
        });
    }

    fn recorded_combo() -> KeyCombo {
        "Cmd+Shift+P".parse().expect("valid test shortcut")
    }

    #[gpui::test]
    fn a_selected_gesture_can_render_in_the_binding_inspector(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        install_app_state(cx);
        let (view, cx) = cx.add_window_view(MouseModelView::new);
        cx.run_until_parked();

        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.set_gesture_selected_dir(Some(GestureDirection::Up), cx);
                let gesture_maps = BTreeMap::from([(
                    ButtonId::MiddleClick,
                    BTreeMap::from([(
                        GestureDirection::Click,
                        default_binding(ButtonId::MiddleClick),
                    )]),
                )]);
                let bindings = BTreeMap::new();
                let entity = cx.entity();

                binding_inspector(
                    BindingInspectorData {
                        selected: Some(MouseControlId::Button(ButtonId::MiddleClick)),
                        gesture_direction: Some(GestureDirection::Up),
                        action_picker_open: false,
                        bindings: &bindings,
                        gesture_maps: &gesture_maps,
                        editing_app: None,
                        overridden: None,
                    },
                    &view.action_search,
                    &entity,
                    theme::palette(cx),
                    cx,
                );
            });
        });
        cx.run_until_parked();
        drop(view);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn selecting_another_control_closes_the_action_picker(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        install_app_state(cx);
        let (view, cx) = cx.add_window_view(MouseModelView::new);
        cx.run_until_parked();

        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.selected = Some(MouseControlId::Button(ButtonId::Back));
                view.action_picker_open = true;

                view.select(MouseControlId::Button(ButtonId::Forward), cx);

                assert!(!view.action_picker_open);
            });
        });
        drop(view);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn completed_recording_commits_the_selected_button_shortcut(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        install_known_mouse_state(cx);
        let (view, cx) = cx.add_window_view(MouseModelView::new);
        cx.run_until_parked();

        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.select(MouseControlId::Button(ButtonId::Back), cx);
                view.begin_shortcut_recording(ShortcutBindingTarget::Button(ButtonId::Back), cx);
            });
            publish_recording(cx, 7, ShortcutRecordingPhase::Recording);
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            publish_recording(cx, 7, ShortcutRecordingPhase::Complete(recorded_combo()));
        });
        cx.run_until_parked();

        cx.update(|_, cx| {
            assert_eq!(
                AppState::try_read(cx).and_then(|state| state.button_bindings.get(&ButtonId::Back)),
                Some(&Action::CustomShortcut(recorded_combo()))
            );
        });
        drop(view);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn completed_recording_commits_the_selected_gesture_shortcut(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        install_known_mouse_state(cx);
        let (view, cx) = cx.add_window_view(MouseModelView::new);
        cx.run_until_parked();

        cx.update(|_, cx| {
            AppState::update_bindings(cx, |state| {
                state.commit_gesture_mode(ButtonId::MiddleClick, true);
            });
            view.update(cx, |view, cx| {
                view.select(MouseControlId::Button(ButtonId::MiddleClick), cx);
                view.set_gesture_selected_dir(Some(GestureDirection::Up), cx);
                view.begin_shortcut_recording(
                    ShortcutBindingTarget::Gesture(ButtonId::MiddleClick, GestureDirection::Up),
                    cx,
                );
            });
            set_recording(cx, 8, ShortcutRecordingPhase::Recording);
            view.update(cx, |view, cx| view.handle_shortcut_recording(cx));
            set_recording(cx, 8, ShortcutRecordingPhase::Complete(recorded_combo()));
            view.update(cx, |view, cx| view.handle_shortcut_recording(cx));

            assert_eq!(
                AppState::try_read(cx)
                    .and_then(|state| state.gesture_bindings.get(&ButtonId::MiddleClick))
                    .and_then(|bindings| bindings.get(&GestureDirection::Up)),
                Some(&Action::CustomShortcut(recorded_combo()))
            );
        });
        drop(view);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_result_from_a_different_recording_session_is_ignored(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        install_known_mouse_state(cx);
        let (view, cx) = cx.add_window_view(MouseModelView::new);
        cx.run_until_parked();

        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.select(MouseControlId::Button(ButtonId::Back), cx);
                view.begin_shortcut_recording(ShortcutBindingTarget::Button(ButtonId::Back), cx);
            });
            set_recording(cx, 9, ShortcutRecordingPhase::Recording);
            view.update(cx, |view, cx| view.handle_shortcut_recording(cx));
            set_recording(cx, 10, ShortcutRecordingPhase::Complete(recorded_combo()));
            view.update(cx, |view, cx| view.handle_shortcut_recording(cx));

            assert!(view.read(cx).pending_shortcut.is_none());
            assert!(!matches!(
                AppState::try_read(cx).and_then(|state| state.button_bindings.get(&ButtonId::Back)),
                Some(Action::CustomShortcut(_))
            ));
        });
        drop(view);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn an_older_snapshot_cannot_displace_the_acknowledged_session(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        install_known_mouse_state(cx);
        let (view, cx) = cx.add_window_view(MouseModelView::new);
        cx.run_until_parked();

        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.select(MouseControlId::Button(ButtonId::Back), cx);
                view.begin_shortcut_recording(ShortcutBindingTarget::Button(ButtonId::Back), cx);
            });
            set_recording(cx, 9, ShortcutRecordingPhase::Recording);
            view.update(cx, |view, cx| view.handle_shortcut_recording(cx));
            set_recording(cx, 8, ShortcutRecordingPhase::Complete(recorded_combo()));
            view.update(cx, |view, cx| view.handle_shortcut_recording(cx));

            assert_eq!(
                view.read(cx)
                    .pending_shortcut
                    .as_ref()
                    .and_then(|pending| pending.session_id),
                Some(9)
            );
            assert!(!matches!(
                AppState::try_read(cx).and_then(|state| state.button_bindings.get(&ButtonId::Back)),
                Some(Action::CustomShortcut(_))
            ));
        });
        drop(view);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_terminal_result_that_predates_start_acknowledgement_is_ignored(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        install_known_mouse_state(cx);
        let (view, cx) = cx.add_window_view(MouseModelView::new);
        cx.run_until_parked();

        cx.update(|_, cx| {
            set_recording(cx, 12, ShortcutRecordingPhase::Complete(recorded_combo()));
            view.update(cx, |view, cx| {
                view.select(MouseControlId::Button(ButtonId::Back), cx);
                view.begin_shortcut_recording(ShortcutBindingTarget::Button(ButtonId::Back), cx);
                view.handle_shortcut_recording(cx);
                assert_eq!(
                    view.pending_shortcut
                        .as_ref()
                        .and_then(|pending| pending.session_id),
                    None
                );
            });

            set_recording(cx, 13, ShortcutRecordingPhase::Recording);
            view.update(cx, |view, cx| {
                view.handle_shortcut_recording(cx);
                assert_eq!(
                    view.pending_shortcut
                        .as_ref()
                        .and_then(|pending| pending.session_id),
                    Some(13)
                );
            });
            assert!(!matches!(
                AppState::try_read(cx).and_then(|state| state.button_bindings.get(&ButtonId::Back)),
                Some(Action::CustomShortcut(_))
            ));
        });
        drop(view);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn changing_profile_while_recording_prevents_a_stale_commit(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        install_known_mouse_state(cx);
        let (view, cx) = cx.add_window_view(MouseModelView::new);
        cx.run_until_parked();

        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.select(MouseControlId::Button(ButtonId::Back), cx);
                view.begin_shortcut_recording(ShortcutBindingTarget::Button(ButtonId::Back), cx);
            });
            set_recording(cx, 11, ShortcutRecordingPhase::Recording);
            view.update(cx, |view, cx| view.handle_shortcut_recording(cx));
            AppState::update(cx, |state, _| {
                state.set_editing_app(Some("com.apple.Safari".to_string()));
            });
            set_recording(cx, 11, ShortcutRecordingPhase::Complete(recorded_combo()));
            view.update(cx, |view, cx| view.handle_shortcut_recording(cx));

            assert!(view.read(cx).pending_shortcut.is_none());
            assert!(!matches!(
                AppState::try_read(cx).and_then(|state| state.button_bindings.get(&ButtonId::Back)),
                Some(Action::CustomShortcut(_))
            ));
        });
        drop(view);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }

    #[test]
    fn active_thumbwheel_directions_highlight_the_paired_control() {
        assert_eq!(
            MouseControlId::from_active_button(ButtonId::ThumbwheelScrollUp),
            MouseControlId::ThumbwheelRotation
        );
        assert_eq!(
            MouseControlId::from_active_button(ButtonId::ThumbwheelScrollDown),
            MouseControlId::ThumbwheelRotation
        );
    }

    #[test]
    fn fallback_model_only_adds_thumbwheel_when_capability_is_measured() {
        let (_, _, without, _) = scaled_model(None, 560., 420., false, LabelDistribution::LeftOnly);
        let (_, _, with, _) = scaled_model(None, 560., 420., true, LabelDistribution::LeftOnly);
        assert_eq!(
            without
                .iter()
                .filter(|hotspot| hotspot.id == MouseControlId::ThumbwheelRotation)
                .count(),
            0
        );
        assert_eq!(
            with.iter()
                .filter(|hotspot| hotspot.id == MouseControlId::ThumbwheelRotation)
                .count(),
            1
        );
    }
}
