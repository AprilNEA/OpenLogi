use crate::ui::theme::FocusRingStyle as _;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::mem;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FocusHandle, Focusable, Hsla,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, RenderOnce, SharedString, Size,
    StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, hsla, img,
    prelude::FluentBuilder as _, px, rgb, size,
};
use gpui_kit::base::{Button as BaseButton, ElementExt as _};
use gpui_kit::component::{
    h_flex,
    input::{InputEvent, InputState},
    v_flex,
};
use openlogi_core::binding::{Action, Binding, ButtonId, GamingLayout, GestureDirection};

use super::editor_mode::{EditorMode, PickerDrafts};
use super::force_sensing::{ForceSensingFacts, ForceSensingSlider, PressForce};
use super::geometry::{
    LABEL_H, LabelDistribution, asset_dimensions_for_png, asset_has_button_labels,
    asset_hotspots_for_png, fallback_hotspots, labels_fit, labels_from_hotspots, side_view_width,
};
use super::hotspots::{Hotspot, MOUSE_MODEL_SIZE, MouseControlId};
use super::inspector::{
    ActionPickerContext, BindingInspectorData, ShortcutError, binding_inspector,
};
use super::leader_lines::{Geometry as LeaderGeometry, Label, paint as paint_leader_lines};
use crate::app::{glow_canvas, keyboard_glow};
use crate::features::binding_editor::{Recording, RecordsShortcut, ShortcutRecorder};
use crate::features::keyboard::editors::{PowerUserKind, text_editor_placeholder};
use crate::features::profiles::{profile_canvas_status, profile_name};
use crate::services::assets::{GlowGeometry, ResolvedAsset};
use crate::state::{AppState, BindingLayer, DeviceKey, DeviceRecord, StateEvent};
use crate::ui::theme::{self, ACCENT_BLUE};

const SIDE_GAP: f32 = 24.;
/// The narrowest a label card gets, however short its values.
const LABEL_MIN_W: f32 = 156.;
/// Below this content width the labels all go on the left: two columns of
/// cards would squeeze the render.
const TWO_SIDED_LABEL_MIN_W: f32 = 700.;
/// The narrowest the render gets to make room for cards wide enough for
/// their values.
const MODEL_MIN_IMAGE_W: f32 = 240.;
/// The widest a label card gets: two columns of them in the widest content
/// still leave the render [`MODEL_MIN_IMAGE_W`]. A value longer than its card
/// wraps instead.
const LABEL_MAX_W: f32 = (MODEL_CONTENT_MAX_W - MODEL_MIN_IMAGE_W) / 2. - SIDE_GAP;
const _: () = assert!(
    TWO_SIDED_LABEL_MIN_W >= 2. * (LABEL_MIN_W + SIDE_GAP) + MODEL_MIN_IMAGE_W,
    "two columns of the narrowest cards must leave the render its minimum width"
);

const CARD_EDGE_INSET: f32 = SIDE_GAP;

const HOTSPOT_DOT: f32 = 12.;
/// Left out for a device without an Actions Ring tab.
const NO_ACTIONS_RING: &[Action] = &[Action::ShowActionsRing];

/// The held actions, which only a button press can run.
fn held_actions(offer_gshift: bool) -> &'static [Action] {
    const ACTIONS: [Action; 2] = [Action::DpiShift, Action::GShift];
    if offer_gshift {
        &ACTIONS
    } else {
        &ACTIONS[..1]
    }
}

/// Vertical space occupied by the device bar, profile context, and canvas
/// padding: the estimate of the canvas area until it is first measured.
const MODEL_VERTICAL_RESERVE: f32 = 154.;

mod labels;
use labels::{CardSize, MAX_VALUE_LINES, ValueMetrics, binding_label_for_control, label_control};
/// Floor for the scaled model height. Below this the evenly-slotted side labels
/// (≈[`LABEL_H`](super::geometry::LABEL_H) each) start to overlap; the window's minimum height is sized to
/// keep the viewport above [`MODEL_VERTICAL_RESERVE`] + this.
const MODEL_MIN_H: f32 = 360.;

/// Max width the model (side gutter + image) may occupy, matching the
/// `buttons_tab` content cap so a wide keyboard image never overflows the panel.
const MODEL_CONTENT_MAX_W: f32 = 760.;
/// Horizontal chrome the model can't draw into (the buttons-tab padding): the
/// estimate of the canvas area until it is first measured.
const MODEL_HORIZONTAL_RESERVE: f32 =
    crate::ui::theme::DETAIL_RAIL_W + super::inspector::INSPECTOR_W + 48.;
/// Floor for the model's available width on a narrow window.
const MODEL_MIN_CONTENT_W: f32 = 200.;

struct MouseWorkspaceData<'a> {
    asset: Option<&'a ResolvedAsset>,
    layout: Option<&'static GamingLayout>,
    active: Option<MouseControlId>,
    bindings: &'a BTreeMap<ButtonId, Action>,
    gesture_maps: &'a BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    glow: Option<(Arc<GlowGeometry>, Hsla)>,
    thumbwheel: bool,
    dpi_gestures: bool,
    virtual_thumbwheel: bool,
    wheel_hold: &'a BTreeSet<ButtonId>,
    editing_app: Option<String>,
    layer: BindingLayer,
    overridden: Option<Cow<'a, BTreeMap<ButtonId, Binding>>>,
    /// Actions the picker offers beyond the catalog.
    extra_actions: &'static [Action],
    /// Catalog actions the picker leaves out.
    hidden_actions: &'static [Action],
}

impl<'a> MouseWorkspaceData<'a> {
    fn read(cx: &'a App) -> Option<Self> {
        AppState::try_read(cx).map(|state| Self {
            asset: state
                .current_record()
                .and_then(|record| record.asset.as_ref()),
            layout: state.current_button_layout(),
            active: state
                .active_button()
                .map(MouseControlId::from_active_button),
            bindings: state.button_bindings(),
            gesture_maps: state.gesture_bindings(),
            glow: state
                .current_record()
                .and_then(|record| keyboard_glow(state, record)),
            thumbwheel: state
                .current_record()
                .and_then(|record| record.capabilities)
                .is_some_and(|capabilities| capabilities.thumbwheel),
            dpi_gestures: state
                .current_record()
                .and_then(|record| record.capabilities)
                .is_some_and(|capabilities| capabilities.dpi_gestures),
            virtual_thumbwheel: state
                .current_record()
                .and_then(|record| record.capabilities)
                .is_some_and(|capabilities| capabilities.virtual_thumbwheel),
            wheel_hold: state.wheel_hold_buttons(),
            editing_app: if state.binding_layer() == BindingLayer::GShift {
                Some(tr!("actions.g_shift").to_string())
            } else {
                state.editing_app().map(|app| profile_name(state, app))
            },
            layer: state.binding_layer(),
            overridden: state.editing_app_overrides(),
            extra_actions: held_actions(
                state.current_gaming_layout().is_some()
                    && state.binding_layer() == BindingLayer::Normal,
            ),
            hidden_actions: if state
                .current_record()
                .is_none_or(crate::app::offers_actions_ring)
            {
                &[]
            } else {
                NO_ACTIONS_RING
            },
        })
    }

    fn empty() -> Self {
        static NO_BINDINGS: BTreeMap<ButtonId, Action> = BTreeMap::new();
        static NO_GESTURE_MAPS: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>> =
            BTreeMap::new();
        static NO_WHEEL_HOLD: BTreeSet<ButtonId> = BTreeSet::new();
        Self {
            asset: None,
            layout: None,
            active: None,
            bindings: &NO_BINDINGS,
            gesture_maps: &NO_GESTURE_MAPS,
            glow: None,
            thumbwheel: false,
            dpi_gestures: false,
            virtual_thumbwheel: false,
            wheel_hold: &NO_WHEEL_HOLD,
            editing_app: None,
            layer: BindingLayer::Normal,
            overridden: None,
            extra_actions: &[],
            hidden_actions: &[],
        }
    }
}

/// Interactive mouse model with button hotspots.
pub struct MouseModelView {
    focus_handle: FocusHandle,
    current_device_key: Option<DeviceKey>,
    hovered: Option<MouseControlId>,
    mode: EditorMode,
    action_search: Entity<InputState>,
    pub(super) custom_shortcut_input: Entity<InputState>,
    pub(super) custom_application_input: Entity<InputState>,
    pub(super) custom_command_input: Entity<InputState>,
    /// The Record button's area; a recording lasts while focus stays in it.
    shortcut_record_focus: FocusHandle,
    /// The canvas area as last laid out, which the model and its cards fit
    /// inside. `None` until the first frame measures it.
    canvas_room: Option<Size<Pixels>>,
    _state_obs: Subscription,
}

impl MouseModelView {
    /// Create the mouse model view.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let action_search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("actions.search_actions")));
        cx.subscribe(&action_search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let custom_shortcut_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("action_ring.shortcut_e_g_cmd_plus_shift_plus_p"))
        });
        cx.subscribe(&custom_shortcut_input, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.edit_picker_drafts(|drafts| drafts.invalid.shortcut = None);
                cx.notify();
            }
        })
        .detach();
        let custom_application_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("action_ring.application_folder_path_or_url"))
        });
        cx.subscribe(
            &custom_application_input,
            |view, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view.edit_picker_drafts(|drafts| drafts.invalid.application = false);
                    cx.notify();
                }
            },
        )
        .detach();
        let custom_command_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(text_editor_placeholder(PowerUserKind::RunShellCommand))
        });
        cx.subscribe(&custom_command_input, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.edit_picker_drafts(|drafts| drafts.invalid.command = false);
                cx.notify();
            }
        })
        .detach();
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::ForegroundChanged
                    | StateEvent::BindingsChanged(_)
                    | StateEvent::LightingChanged(_)
                    | StateEvent::ForceSensingChanged(_)
            )
        });
        Self {
            focus_handle: cx.focus_handle(),
            current_device_key: None,
            hovered: None,
            mode: EditorMode::Idle,
            action_search,
            custom_shortcut_input,
            custom_application_input,
            custom_command_input,
            shortcut_record_focus: cx.focus_handle(),
            canvas_room: None,
            _state_obs: state_obs,
        }
    }

    /// Edit `control` in the inspector, closing the picker; selecting the
    /// control already edited changes nothing. Callers must `cx.notify()`.
    pub(super) fn select(&mut self, control: MouseControlId) {
        self.mode = mem::take(&mut self.mode).select(control);
    }

    /// Show `direction` in the selected button's gesture inspector, closing
    /// the picker. Callers must `cx.notify()`.
    pub(super) fn select_gesture_direction(&mut self, direction: GestureDirection) {
        self.mode = mem::take(&mut self.mode).select_gesture_direction(direction);
    }

    /// Open the action picker on the selection with every custom-action
    /// draft cleared, so a shortcut, application or command typed for one
    /// button doesn't reappear for another. Callers must `cx.notify()`.
    pub(super) fn open_action_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for input in [
            &self.action_search,
            &self.custom_shortcut_input,
            &self.custom_application_input,
            &self.custom_command_input,
        ] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.mode = mem::take(&mut self.mode).open_picker();
    }

    /// Close the action picker, ending any recording. Callers must
    /// `cx.notify()`.
    pub(super) fn close_action_picker(&mut self) {
        self.mode = mem::take(&mut self.mode).close_picker();
    }

    /// Start recording the custom shortcut, or stop the recording in
    /// progress.
    pub(super) fn toggle_shortcut_recording(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let area = &self.shortcut_record_focus;
        self.mode = mem::take(&mut self.mode)
            .toggle_recording(|| ShortcutRecorder::start(area, window, cx));
        cx.notify();
    }

    /// Change the open picker's drafts; nothing while it is closed. Callers
    /// must `cx.notify()`.
    pub(super) fn edit_picker_drafts(&mut self, edit: impl FnOnce(&mut PickerDrafts)) {
        if let Some(drafts) = self.mode.drafts_mut() {
            edit(drafts);
        }
    }

    /// The press-force slider, while the haptic panel's inspector shows it.
    pub(super) fn press_force(&self) -> Option<&ForceSensingSlider> {
        self.mode.press_force()
    }

    /// The action picker's state, as the inspector reads it.
    fn action_picker_context<'a>(&'a self, view: &'a Entity<Self>) -> ActionPickerContext<'a> {
        let drafts = self.mode.drafts().copied();
        let PickerDrafts {
            shortcut_mode,
            invalid,
        } = drafts.unwrap_or_default();
        ActionPickerContext {
            open: drafts.is_some(),
            search: &self.action_search,
            shortcut_input: &self.custom_shortcut_input,
            application_input: &self.custom_application_input,
            command_input: &self.custom_command_input,
            shortcut_mode,
            shortcut_recording: self.mode.is_recording(),
            shortcut_record_focus: &self.shortcut_record_focus,
            invalid,
            view,
        }
    }

    fn reset_for_device(&mut self, device_key: Option<DeviceKey>) {
        if self.current_device_key == device_key {
            return;
        }
        self.current_device_key = device_key;
        self.hovered = None;
        self.mode = EditorMode::Idle;
    }

    /// Keep the canvas area measured in the last frame, and lay the model out
    /// again when it changed.
    fn set_canvas_room(&mut self, room: Size<Pixels>, cx: &mut Context<Self>) {
        if self.canvas_room != Some(room) {
            self.canvas_room = Some(room);
            cx.notify();
        }
    }

    /// Lay the model and its cards out in the canvas area as last measured,
    /// or before the first measurement, the viewport less the chrome around
    /// the tab. The cards are sized for what `value` says each control does.
    fn fitted_layout(
        &self,
        asset: Option<&ResolvedAsset>,
        layout: Option<&GamingLayout>,
        thumbwheel: bool,
        value: impl Fn(MouseControlId) -> SharedString,
        window: &Window,
    ) -> ModelLayout {
        let viewport = window.viewport_size();
        let room = self.canvas_room.unwrap_or_else(|| {
            size(
                viewport.width - px(MODEL_HORIZONTAL_RESERVE),
                viewport.height - px(MODEL_VERTICAL_RESERVE),
            )
        });
        let values = ValueMetrics::measure(
            model_hotspots(asset, layout, thumbwheel, MOUSE_MODEL_SIZE)
                .into_iter()
                .map(|hotspot| value(hotspot.id)),
            window,
        );
        model_layout(
            asset,
            layout,
            size(f32::from(room.width), f32::from(room.height)),
            f32::from(viewport.width),
            thumbwheel,
            values,
        )
    }
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

impl MouseModelView {
    /// Re-stamp every action-picker input's placeholder after a language
    /// switch, split out of `render` to keep it under clippy's line budget.
    fn localize_action_picker_inputs(&self, window: &mut Window, cx: &mut Context<Self>) {
        crate::ui::components::localize_placeholder(
            &self.action_search,
            tr!("actions.search_actions"),
            window,
            cx,
        );
        crate::ui::components::localize_placeholder(
            &self.custom_shortcut_input,
            tr!("action_ring.shortcut_e_g_cmd_plus_shift_plus_p"),
            window,
            cx,
        );
        crate::ui::components::localize_placeholder(
            &self.custom_application_input,
            tr!("action_ring.application_folder_path_or_url"),
            window,
            cx,
        );
    }

    /// Follow a device change, then keep a selected haptic panel's
    /// press-force slider in step with the device. Runs before `render` reads
    /// the state for the frame, since the slider sync needs the context
    /// mutably.
    fn sync_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<ForceSensingFacts> {
        self.reset_for_device(
            AppState::try_read(cx)
                .and_then(AppState::current_record)
                .map(DeviceRecord::device_key),
        );
        let press_force = self.mode.press_force_mut()?;
        let facts = ForceSensingFacts::current(cx);
        ForceSensingSlider::sync(press_force, facts.as_ref(), window, cx);
        facts
    }
}

impl RecordsShortcut for MouseModelView {
    /// A recorded chord fills the shortcut field, and Add commits it in the
    /// chosen mode. A key the recorder cannot turn into a chord shows the
    /// field's error and leaves the field as it was.
    fn shortcut_recorded(
        &mut self,
        recording: Recording,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mode = mem::take(&mut self.mode).stop_recording();
        if let Some(drafts) = self.mode.drafts_mut() {
            match recording {
                Recording::Chord(combo) => {
                    self.custom_shortcut_input.update(cx, |input, cx| {
                        input.set_value(combo.rendered_label(), window, cx);
                    });
                    drafts.invalid.shortcut = None;
                }
                Recording::Cancelled => {}
                Recording::Unsupported => {
                    drafts.invalid.shortcut = Some(ShortcutError::Unrecognized);
                }
                Recording::ShiftedSymbol => {
                    drafts.invalid.shortcut = Some(ShortcutError::ShiftedSymbol);
                }
            }
        }
        cx.notify();
    }
}

impl Render for MouseModelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.localize_action_picker_inputs(window, cx);
        let force_sensing = self.sync_selection(window, cx);
        let MouseWorkspaceData {
            asset,
            layout,
            active,
            bindings,
            gesture_maps,
            glow,
            thumbwheel,
            dpi_gestures,
            virtual_thumbwheel,
            wheel_hold,
            editing_app,
            layer,
            overridden,
            extra_actions,
            hidden_actions,
        } = MouseWorkspaceData::read(cx).unwrap_or_else(MouseWorkspaceData::empty);

        // The G-Shift layer shows a button it overrides as that action, not
        // as the normal layer's gestures.
        let gesture_buttons: Vec<ButtonId> = gesture_maps
            .keys()
            .copied()
            .filter(|button| {
                layer == BindingLayer::Normal
                    || !overridden
                        .as_ref()
                        .is_some_and(|overrides| overrides.contains_key(button))
            })
            .collect();

        let value = |control| binding_label_for_control(control, bindings, &gesture_buttons).text;
        let ModelLayout {
            canvas_w,
            canvas_h,
            mouse_top,
            mouse_left,
            mouse_w,
            mouse_h,
            card,
            hotspots,
            labels,
        } = self.fitted_layout(asset, layout, thumbwheel, value, window);

        let selected = self.mode.control();
        let highlight = self.hovered.or(active).or(selected);
        let view = cx.entity();
        let hovered = self.hovered;
        let profile_status = profile_canvas_status(cx);

        let hotspots_outer = hotspots.clone();
        let labels_outer = labels.clone();
        let leader_canvas = leader_canvas(hotspots, labels, highlight, mouse_left, mouse_w);
        let breathing_art = breathing_art(asset, mouse_left, mouse_w, mouse_h, glow);
        let model = ModelRect {
            left: mouse_left,
            width: mouse_w,
            height: mouse_h,
            card,
        };
        let hotspots_layer =
            hotspots_layer(&hotspots_outer, model, hovered, active, selected, &view);
        let model_band = div()
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
                    selected == Some(label.id),
                    &view,
                )
            }))
            .child(hotspots_layer);
        let canvas = model_canvas(model_band, canvas_w, canvas_h, mouse_top, mouse_h);

        let inspector = binding_inspector(
            &BindingInspectorData {
                selected: self.mode.selection(),
                bindings,
                layout,
                gesture_maps,
                dpi_gestures,
                virtual_thumbwheel,
                wheel_hold,
                editing_app: editing_app.as_deref(),
                layer,
                overridden,
                press_force: PressForce::of(force_sensing.as_ref(), self.press_force()),
                extra_actions,
                hidden_actions,
            },
            self.action_picker_context(&view),
            cx,
        );
        workspace_layout(canvas, profile_status, inspector, &self.focus_handle, view)
    }
}

/// The canvas around `band`, which holds the model and its cards laid out
/// against the model's own `mouse_h`. The cards a crowded side pushes past
/// the model's top or bottom still land inside the canvas, which grows to
/// `canvas_h` to hold them, with the model `mouse_top` down from its top.
fn model_canvas(
    band: gpui::Div,
    canvas_w: f32,
    canvas_h: f32,
    mouse_top: f32,
    mouse_h: f32,
) -> gpui::Div {
    div()
        .relative()
        .w(px(canvas_w))
        .h(px(canvas_h))
        .debug_selector(|| "mouse-canvas".into())
        .child(
            band.absolute()
                .top(px(mouse_top))
                .left(px(0.))
                .w(px(canvas_w))
                .h(px(mouse_h)),
        )
}

/// The canvas area beside the inspector. Its content box is the room the
/// model fits: measured here and handed back to `view` for the next layout.
fn workspace_layout(
    canvas: impl IntoElement,
    profile_status: Option<gpui::Div>,
    inspector: impl IntoElement,
    focus_handle: &FocusHandle,
    view: Entity<MouseModelView>,
) -> impl IntoElement {
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
                        .p_4()
                        .child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .debug_selector(|| "mouse-canvas-room".into())
                                .child(canvas)
                                .on_prepaint(move |bounds, _window, cx| {
                                    view.update(cx, |view, cx| {
                                        view.set_canvas_room(bounds.size, cx);
                                    });
                                }),
                        ),
                ),
        )
        .child(inspector)
}

struct ModelLayout {
    canvas_w: f32,
    /// The model's height plus whatever the cards reach past it.
    canvas_h: f32,
    /// How far the model sits below the canvas top: how far the cards reach
    /// above it.
    mouse_top: f32,
    mouse_left: f32,
    mouse_w: f32,
    mouse_h: f32,
    card: CardSize,
    hotspots: Vec<Hotspot>,
    labels: Vec<Label>,
}

/// Lay the model and its label cards out inside `room`, the canvas area. The
/// cards are as wide as their widest value needs (see [`label_columns`]); the
/// model takes the rest and is scaled to fit it in both axes. A tall mouse is
/// bound by the room's height; a wide keyboard is bound by its width and drops
/// the label gutter so it remains centred.
fn model_layout(
    asset: Option<&ResolvedAsset>,
    layout: Option<&GamingLayout>,
    room: Size<f32>,
    viewport_w: f32,
    thumbwheel: bool,
    values: ValueMetrics,
) -> ModelLayout {
    let target_h = room.height.clamp(MODEL_MIN_H, MOUSE_MODEL_SIZE.1);
    // The cards may use the room's whole height, not just the model's: a
    // narrow window shrinks the model by its width and leaves room beside it.
    let room_h = room.height.max(MODEL_MIN_H);
    let has_labels =
        asset.is_none_or(|asset| asset_has_button_labels(asset, layout)) && viewport_w >= 960.;
    let content_w = room.width.clamp(MODEL_MIN_CONTENT_W, MODEL_CONTENT_MAX_W);
    let (label_distribution, card_w) = label_columns(content_w, values.fit_w[0] + values.chrome_w);
    let gutter = card_w + SIDE_GAP;
    let left_gutter = if has_labels { gutter } else { 0. };
    let right_gutter = if has_labels && label_distribution == LabelDistribution::BothSides {
        gutter
    } else {
        0.
    };
    let max_image_w = (content_w - left_gutter - right_gutter).max(MODEL_MIN_CONTENT_W / 2.);
    let (mouse_w, mouse_h) = model_size(asset, target_h, max_image_w);
    let hotspots = model_hotspots(asset, layout, thumbwheel, (mouse_w, mouse_h));
    let card = card_size(card_w, values, hotspots.len(), label_distribution, room_h);
    let labels = if has_labels {
        labels_from_hotspots(
            &hotspots,
            mouse_w,
            mouse_h,
            room_h,
            label_distribution,
            card.h,
        )
    } else {
        Vec::new()
    };
    let cards_top = labels
        .iter()
        .map(|label| label.y - card.h / 2.)
        .fold(0., f32::min);
    let cards_bottom = labels
        .iter()
        .map(|label| label.y + card.h / 2.)
        .fold(mouse_h, f32::max);

    ModelLayout {
        canvas_w: left_gutter + mouse_w + right_gutter,
        canvas_h: cards_bottom - cards_top,
        mouse_top: -cards_top,
        mouse_left: left_gutter,
        mouse_w,
        mouse_h,
        card,
        hotspots,
        labels,
    }
}

/// Which sides the label cards take in `content_w`, and how wide they are:
/// `needed_w`, what the widest value needs on one line, kept between
/// [`LABEL_MIN_W`] and [`LABEL_MAX_W`] and narrow enough to leave the render
/// [`MODEL_MIN_IMAGE_W`].
fn label_columns(content_w: f32, needed_w: f32) -> (LabelDistribution, f32) {
    let (distribution, columns) = if content_w >= TWO_SIDED_LABEL_MIN_W {
        (LabelDistribution::BothSides, 2.)
    } else {
        (LabelDistribution::LeftOnly, 1.)
    };
    let room_w = (content_w - MODEL_MIN_IMAGE_W) / columns - SIDE_GAP;
    (
        distribution,
        needed_w.min(room_w).clamp(LABEL_MIN_W, LABEL_MAX_W),
    )
}

/// The cards `card_w` wide, with as many value lines as the longest value
/// needs at that width — up to [`MAX_VALUE_LINES`] — as long as `count`
/// cards that tall still fit `room_h` with their gaps. Past that a value
/// keeps the lines that fit and ends in an ellipsis.
fn card_size(
    card_w: f32,
    values: ValueMetrics,
    count: usize,
    distribution: LabelDistribution,
    room_h: f32,
) -> CardSize {
    let needed = values
        .lines_at(card_w - values.chrome_w)
        .unwrap_or(MAX_VALUE_LINES);
    let one_line = CardSize {
        w: card_w,
        h: LABEL_H,
        value_lines: 1,
    };
    // Each line makes every card taller: the most that still fit.
    std::iter::successors(Some(one_line), |card| {
        Some(CardSize {
            h: card.h + values.line_h,
            value_lines: card.value_lines + 1,
            ..*card
        })
    })
    .take(needed)
    .take_while(|card| card.value_lines == 1 || labels_fit(count, distribution, card.h, room_h))
    .last()
    .unwrap_or(one_line)
}

/// The model's size fit inside a `max_w` × `target_h` box at the render's
/// aspect, or the synthetic silhouette's when there is no render.
fn model_size(asset: Option<&ResolvedAsset>, target_h: f32, max_w: f32) -> (f32, f32) {
    if let Some(a) = asset {
        asset_dimensions_for_png(a, target_h, max_w)
    } else {
        let scale = (target_h / MOUSE_MODEL_SIZE.1).min(max_w / MOUSE_MODEL_SIZE.0);
        (MOUSE_MODEL_SIZE.0 * scale, MOUSE_MODEL_SIZE.1 * scale)
    }
}

/// The model's hotspots at its `(w, h)` size (see [`model_size`]). A render
/// recomputes them from its markers; the synthetic silhouette's authored
/// coordinates are scaled to that size.
fn model_hotspots(
    asset: Option<&ResolvedAsset>,
    layout: Option<&GamingLayout>,
    thumbwheel: bool,
    (w, h): (f32, f32),
) -> Vec<Hotspot> {
    if let Some(a) = asset {
        asset_hotspots_for_png(a, layout, w, h)
    } else {
        let scale = w / MOUSE_MODEL_SIZE.0;
        fallback_hotspots(layout, thumbwheel)
            .into_iter()
            .map(|hs| Hotspot {
                x: hs.x * scale,
                y: hs.y * scale,
                w: hs.w * scale,
                h: hs.h * scale,
                ..hs
            })
            .collect()
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
    glow: Option<(Arc<GlowGeometry>, Hsla)>,
) -> impl IntoElement {
    let device_art: AnyElement = match asset {
        Some(a) => {
            let side_w = side_view_width(a, mouse_h);
            h_flex()
                .when_some(a.side_view.as_ref(), |this, side| {
                    this.child(img(side.image_path.clone()).w(px(side_w)).h(px(mouse_h)))
                })
                .child(
                    img(a.image_path.clone())
                        .w(px(mouse_w - side_w))
                        .h(px(mouse_h)),
                )
                .into_any_element()
        }
        None => Silhouette {
            w: mouse_w,
            h: mouse_h,
        }
        .into_any_element(),
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

/// The model's place in the canvas, and the size of the cards beside it.
#[derive(Clone, Copy)]
struct ModelRect {
    left: f32,
    width: f32,
    height: f32,
    card: CardSize,
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

/// Shape-based silhouette used when no asset is cached for the device.
///
/// Its `rounded_*` values are illustration proportions — the body shell and the
/// two drawn side buttons — not UI chrome, so they stay fixed rather than
/// tracking the `Palette` radius tokens the way real cards and controls do.
#[derive(IntoElement)]
struct Silhouette {
    w: f32,
    h: f32,
}

impl RenderOnce for Silhouette {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { w, h } = self;
        let pal = theme::palette(cx);
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
}

fn hotspot_control(
    idx: usize,
    hotspot: Hotspot,
    hovered: Option<MouseControlId>,
    active: Option<MouseControlId>,
    selected: bool,
    view: &Entity<MouseModelView>,
) -> gpui::Div {
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
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let highlighted = self.hovered || self.selected;
        let selected = self.selected;
        let view = self.view;
        let click_view = view.clone();
        let hotspot = self.hotspot;
        let btn = hotspot.id;

        BaseButton::new(self.id)
            .selected(selected)
            .accessibility_label(tr!(
                "actions.bind_control",
                name => tr!(btn.translation_key())
            ))
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
            .focus_visible(move |style| style.rounded_full().focus_ring(pal))
            .on_click(move |_event, _window, cx| {
                click_view.update(cx, |this, cx| {
                    this.select(btn);
                    cx.notify();
                });
            })
            .on_hover(move |hovered, _window, cx| {
                set_control_hovered(&view, btn, *hovered, cx);
            })
    }
}

#[cfg(test)]
mod tests;
