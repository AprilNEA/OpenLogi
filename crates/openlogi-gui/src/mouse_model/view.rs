use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{
    Anchor, AnyElement, App, BorrowAppContext as _, Context, ElementId, Entity, Hsla,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, RenderOnce,
    StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, hsla, img,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{Icon, IconName, Selectable, h_flex, popover::Popover, v_flex};

use crate::app::{glow_canvas, keyboard_glow};
use crate::asset::{GlowGeometry, ResolvedAsset};
use crate::data::mouse_buttons::{
    Action, ButtonId, GestureDirection, Hotspot, MOUSE_MODEL_SIZE, default_binding,
};
use crate::mouse_model::button_layout::{MouseButtonLayout, MouseModelPerspective};
use crate::mouse_model::geometry::{
    asset_dimensions_for_png, asset_has_button_labels, asset_hotspots_for_png, labels_from_hotspots,
};
use crate::mouse_model::leader_lines::{
    Geometry as LeaderGeometry, Label, Side, paint as paint_leader_lines,
};
use crate::mouse_model::picker::{
    GESTURE_BUTTON_ICON, action_icon_path, action_picker, gesture_overview,
};
use crate::state::AppState;
use crate::theme::{self, ACCENT_BLUE, Palette, SelectableStyle};

const SIDE_W: f32 = 180.;
const SIDE_GAP: f32 = 24.;
const LABEL_W: f32 = 156.;
const LABEL_H: f32 = 56.;

const CARD_EDGE_INSET: f32 = SIDE_GAP + (SIDE_W - LABEL_W);

const HOTSPOT_DOT: f32 = 12.;

/// Vertical space around the model that it can't draw into: the detail header
/// and footer, the buttons-tab padding, and the gesture selector row above the
/// canvas. The model scales to fit whatever viewport height remains.
const MODEL_VERTICAL_RESERVE: f32 = 224.;
/// Extra vertical budget only when the G502 fallback perspective switcher is
/// actually rendered below the canvas.
const MODEL_PERSPECTIVE_VERTICAL_RESERVE: f32 = 36.;
/// Floor for the scaled model height. Below this the evenly-slotted side labels
/// (≈[`LABEL_H`] each) start to overlap; the window's minimum height is sized to
/// keep the viewport above [`MODEL_VERTICAL_RESERVE`] + this.
const MODEL_MIN_H: f32 = 448.;

/// Max width the model (side gutter + image) may occupy, matching the
/// `buttons_tab` content cap so a wide keyboard image never overflows the panel.
const MODEL_CONTENT_MAX_W: f32 = 760.;
/// Horizontal chrome the model can't draw into (the buttons-tab padding).
const MODEL_HORIZONTAL_RESERVE: f32 = 48.;
/// Floor for the model's available width on a narrow window.
const MODEL_MIN_CONTENT_W: f32 = 320.;

#[derive(Clone, Copy)]
struct ArtGeometry {
    left: f32,
    width: f32,
    height: f32,
}

struct FittedModel {
    canvas_width: f32,
    canvas_height: f32,
    left: f32,
    width: f32,
    height: f32,
    hotspots: Vec<Hotspot>,
    labels: Vec<Label>,
}

#[derive(Default)]
struct MouseModelRenderData {
    asset: Option<ResolvedAsset>,
    layout: MouseButtonLayout,
    active: Option<ButtonId>,
    bindings: BTreeMap<ButtonId, Action>,
    gesture_owner: Option<ButtonId>,
    glow: Option<(Arc<GlowGeometry>, Hsla)>,
}

/// Interactive mouse model with button hotspots.
pub struct MouseModelView {
    hovered: Option<ButtonId>,
    /// Which gesture direction the open gesture menu has activated (so its
    /// level-2 flyout card shows), or `None` for the plus-only state. Scratch UI
    /// state owned here (like [`Self::hovered`]) rather than in window-keyed
    /// state, so the popover's `on_open_change` — which runs outside paint — can
    /// reset it without tripping gpui's render-only guard.
    gesture_active_dir: Option<GestureDirection>,
    model_perspective: MouseModelPerspective,
    _state_obs: Subscription,
}

impl MouseModelView {
    /// Create the mouse model view.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.observe_global::<AppState>(|_view, cx| cx.notify());
        Self {
            hovered: None,
            gesture_active_dir: None,
            model_perspective: MouseModelPerspective::View1,
            _state_obs: state_obs,
        }
    }

    /// The gesture direction whose level-2 flyout is open, if any.
    pub(crate) fn gesture_selected_dir(&self) -> Option<GestureDirection> {
        self.gesture_active_dir
    }

    /// Set (or clear, with `None`) the activated gesture direction. Callers must
    /// `cx.notify()` to re-render.
    pub(crate) fn set_gesture_selected_dir(&mut self, dir: Option<GestureDirection>) {
        self.gesture_active_dir = dir;
    }
}

impl Render for MouseModelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let MouseModelRenderData {
            asset,
            layout,
            active,
            bindings,
            gesture_owner,
            glow,
        } = mouse_model_render_data(cx);
        let uses_authored_perspectives = layout.supports_perspectives();
        let model_asset = model_asset_for_layout(asset.as_ref(), layout);
        let perspective = if uses_authored_perspectives {
            self.model_perspective
        } else {
            MouseModelPerspective::View1
        };

        let model = fit_model_to_window(
            window,
            model_asset,
            layout,
            perspective,
            uses_authored_perspectives,
        );

        let highlight = self.hovered.or(active);
        let view = cx.entity();
        let hovered = self.hovered;
        let pal = theme::palette(cx);

        let hotspots_outer = model.hotspots.clone();
        let labels_outer = model.labels.clone();
        let capable = gesture_capable_buttons(layout, &labels_outer);
        let gesture_owner = gesture_owner.filter(|id| capable.contains(id));
        let leader_canvas = leader_canvas(
            model.hotspots,
            model.labels,
            highlight,
            model.left,
            model.width,
        );
        let breathing_art = breathing_art(
            model_asset,
            layout,
            perspective,
            ArtGeometry {
                left: model.left,
                width: model.width,
                height: model.height,
            },
            pal,
            glow,
        );
        let hotspots_layer = hotspots_layer(
            &hotspots_outer,
            layout,
            model.left,
            model.width,
            model.height,
            hovered,
            active,
            gesture_owner,
            &view,
        );
        let canvas = div()
            .relative()
            .w(px(model.canvas_width))
            .h(px(model.canvas_height))
            .child(breathing_art)
            .child(leader_canvas)
            .children(labels_outer.iter().enumerate().map(|(idx, label)| {
                let binding = binding_label_for(label.id, layout, &bindings, gesture_owner);
                label_popover(
                    idx,
                    *label,
                    binding,
                    highlight == Some(label.id),
                    model.left,
                    model.width,
                    hovered,
                    active,
                    gesture_owner,
                    &view,
                )
            }))
            .child(hotspots_layer);

        // The gesture-button selector sits above the mouse: a single-select of
        // the device's gesture-capable buttons (the HID++ gesture button plus the
        // OS-hook Middle/Back/Forward) makes the one-gesture-button-per-device
        // lock visible and obvious — pick one and its card opens the gesture
        // menu, the rest stay single-action.
        v_flex()
            .w(px(model.canvas_width))
            .gap_4()
            .when(!capable.is_empty(), |col| {
                col.child(gesture_owner_selector(&capable, gesture_owner, &view, pal))
            })
            .child(canvas)
            .when(uses_authored_perspectives, |col| {
                col.child(perspective_selector(perspective, &view, pal))
            })
    }
}

fn mouse_model_render_data(cx: &mut Context<MouseModelView>) -> MouseModelRenderData {
    cx.try_global::<AppState>()
        .map(|s| {
            let record = s.current_record();
            MouseModelRenderData {
                asset: record.and_then(|r| r.asset.clone()),
                layout: record.map_or(MouseButtonLayout::Default, |r| {
                    let name = r.codename.as_deref().unwrap_or(&r.display_name);
                    MouseButtonLayout::for_device(r.model_info.as_ref(), Some(name))
                }),
                active: s.active_button,
                bindings: s.button_bindings.clone(),
                gesture_owner: s.current_gesture_owner(),
                glow: record.and_then(|r| keyboard_glow(s, r)),
            }
        })
        .unwrap_or_default()
}

fn model_asset_for_layout(
    asset: Option<&ResolvedAsset>,
    layout: MouseButtonLayout,
) -> Option<&ResolvedAsset> {
    // G502 needs two authored perspectives; today's asset resolver exposes one
    // buttons image/metadata set, so use the family model until assets grow that shape.
    if layout.supports_perspectives() {
        None
    } else {
        asset
    }
}

fn fit_model_to_window(
    window: &Window,
    asset: Option<&ResolvedAsset>,
    layout: MouseButtonLayout,
    perspective: MouseModelPerspective,
    include_perspective_reserve: bool,
) -> FittedModel {
    let viewport_h = f32::from(window.viewport_size().height);
    let viewport_w = f32::from(window.viewport_size().width);
    let vertical_reserve = if include_perspective_reserve {
        MODEL_VERTICAL_RESERVE + MODEL_PERSPECTIVE_VERTICAL_RESERVE
    } else {
        MODEL_VERTICAL_RESERVE
    };
    let target_h = (viewport_h - vertical_reserve).clamp(MODEL_MIN_H, MOUSE_MODEL_SIZE.1);
    let has_labels = asset.is_none_or(|asset| asset_has_button_labels(asset, layout));
    let gutter = if has_labels { SIDE_W + SIDE_GAP } else { 0. };
    let content_w =
        (viewport_w - MODEL_HORIZONTAL_RESERVE).clamp(MODEL_MIN_CONTENT_W, MODEL_CONTENT_MAX_W);
    let max_image_w = (content_w - gutter).max(MODEL_MIN_CONTENT_W / 2.);
    let (width, height, hotspots, labels) =
        scaled_model(asset, layout, perspective, target_h, max_image_w);
    FittedModel {
        canvas_width: gutter + width,
        canvas_height: height,
        left: gutter,
        width,
        height,
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
    layout: MouseButtonLayout,
    perspective: MouseModelPerspective,
    target_h: f32,
    max_w: f32,
) -> (f32, f32, Vec<Hotspot>, Vec<Label>) {
    if let Some(a) = asset {
        let (w, h) = asset_dimensions_for_png(a, target_h, max_w);
        let hotspots = asset_hotspots_for_png(a, layout, w, h);
        let labels = labels_from_hotspots(&hotspots, h);
        (w, h, hotspots, labels)
    } else {
        let scale = (target_h / MOUSE_MODEL_SIZE.1).min(max_w / MOUSE_MODEL_SIZE.0);
        let hotspots = layout
            .fallback_hotspots(perspective)
            .into_iter()
            .map(|hs| Hotspot {
                x: hs.x * scale,
                y: hs.y * scale,
                w: hs.w * scale,
                h: hs.h * scale,
                ..hs
            })
            .collect();
        let labels = layout
            .fallback_labels(perspective)
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

/// The gesture-capable buttons present on this device, in a stable display
/// order: the HID++ gesture button first, then the OS-hook Middle/Back/Forward.
fn gesture_capable_buttons(layout: MouseButtonLayout, visible_labels: &[Label]) -> Vec<ButtonId> {
    const ORDER: [ButtonId; 4] = [
        ButtonId::GestureButton,
        ButtonId::MiddleClick,
        ButtonId::Back,
        ButtonId::Forward,
    ];
    let label_ids: Vec<ButtonId> = if layout.supports_perspectives() {
        [MouseModelPerspective::View1, MouseModelPerspective::View2]
            .into_iter()
            .flat_map(|perspective| layout.fallback_labels(perspective))
            .map(|label| label.id)
            .collect()
    } else {
        visible_labels.iter().map(|label| label.id).collect()
    };
    ORDER
        .into_iter()
        .filter(|id| label_ids.contains(id))
        .collect()
}

/// Short, context-appropriate name for a gesture-button choice.
fn gesture_owner_label(btn: ButtonId) -> &'static str {
    match btn {
        ButtonId::GestureButton => "Gesture Button",
        ButtonId::MiddleClick => "Middle",
        ButtonId::Back => "Back",
        ButtonId::Forward => "Forward",
        other => other.label(),
    }
}

/// The "Gesture button: ( … )" single-select row above the mouse. The single
/// select makes the one-gesture-button-per-device lock visible; picking a button
/// commits it as the owner (demoting any previous one).
fn gesture_owner_selector(
    capable: &[ButtonId],
    owner: Option<ButtonId>,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap_2()
        .pl(px(SIDE_W + SIDE_GAP))
        .child(
            div()
                .text_xs()
                .text_color(pal.text_muted)
                .child(tr!("Gesture Button")),
        )
        .children(
            capable
                .iter()
                .map(|&btn| owner_chip(Some(btn), owner, view, pal)),
        )
        .child(owner_chip(None, owner, view, pal))
}

/// One selectable chip in [`gesture_owner_selector`]. Clicking commits the new
/// gesture owner via [`AppState::commit_gesture_owner`].
fn owner_chip(
    btn: Option<ButtonId>,
    owner: Option<ButtonId>,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> AnyElement {
    let selected = btn == owner;
    let text = match btn {
        Some(b) => tr!(gesture_owner_label(b)),
        None => tr!("Off"),
    };
    let id_part = btn.map_or(0usize, |b| b as usize + 1);
    let view = view.clone();
    div()
        .id(("gesture-owner", id_part))
        .px_2()
        .py_1()
        .rounded_md()
        .selected_border(selected, pal)
        .selected_fill(selected)
        .text_xs()
        .text_color(if selected {
            pal.text_primary
        } else {
            pal.text_muted
        })
        .when(!selected, |s| s.hover(|s| s.bg(pal.surface_hover)))
        .cursor_pointer()
        .child(text)
        .on_click(move |_event, _window, cx| {
            cx.update_global::<AppState, _>(|state, _| state.commit_gesture_owner(btn));
            view.update(cx, |_, vcx| vcx.notify());
        })
        .into_any_element()
}

fn perspective_selector(
    active: MouseModelPerspective,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> impl IntoElement {
    h_flex().items_center().justify_center().gap_2().children(
        [MouseModelPerspective::View1, MouseModelPerspective::View2]
            .into_iter()
            .map(|perspective| perspective_chip(perspective, active, view, pal)),
    )
}

fn perspective_chip(
    perspective: MouseModelPerspective,
    active: MouseModelPerspective,
    view: &Entity<MouseModelView>,
    pal: Palette,
) -> AnyElement {
    let selected = perspective == active;
    let id = match perspective {
        MouseModelPerspective::View1 => 1usize,
        MouseModelPerspective::View2 => 2usize,
    };
    let view = view.clone();
    div()
        .id(("g502-perspective", id))
        .px_3()
        .py_1()
        .rounded_md()
        .selected_border(selected, pal)
        .selected_fill(selected)
        .text_xs()
        .text_color(if selected {
            pal.text_primary
        } else {
            pal.text_muted
        })
        .when(!selected, |s| s.hover(|s| s.bg(pal.surface_hover)))
        .cursor_pointer()
        .child(tr!(perspective.label()))
        .on_click(move |_event, _window, cx| {
            view.update(cx, |this, cx| {
                this.model_perspective = perspective;
                this.hovered = None;
                cx.notify();
            });
        })
        .into_any_element()
}

fn leader_canvas(
    hotspots: Vec<Hotspot>,
    labels: Vec<Label>,
    highlight: Option<ButtonId>,
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
    layout: MouseButtonLayout,
    perspective: MouseModelPerspective,
    geom: ArtGeometry,
    pal: Palette,
    glow: Option<(Arc<GlowGeometry>, Hsla)>,
) -> impl IntoElement {
    let ArtGeometry {
        left: mouse_left,
        width: mouse_w,
        height: mouse_h,
    } = geom;
    let device_art: AnyElement = match asset {
        Some(a) => img(a.image_path.clone())
            .w(px(mouse_w))
            .h(px(mouse_h))
            .into_any_element(),
        None => silhouette(mouse_w, mouse_h, pal, layout, perspective).into_any_element(),
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

#[allow(
    clippy::too_many_arguments,
    reason = "layout inputs + hover/active/owner state; bundling would just hide the dependency"
)]
fn hotspots_layer(
    hotspots: &[Hotspot],
    layout: MouseButtonLayout,
    mouse_left: f32,
    mouse_w: f32,
    mouse_h: f32,
    hovered: Option<ButtonId>,
    active: Option<ButtonId>,
    gesture_owner: Option<ButtonId>,
    view: &Entity<MouseModelView>,
) -> impl IntoElement {
    div()
        .absolute()
        .left(px(mouse_left))
        .top(px(0.))
        .w(px(mouse_w))
        .h(px(mouse_h))
        .children(hotspots.iter().enumerate().map(|(idx, hotspot)| {
            hotspot_popover(
                idx,
                *hotspot,
                layout.binds_hotspot(hotspot.id),
                hovered,
                active,
                gesture_owner,
                view,
            )
        }))
}

/// Wrap `trigger` in a left-click [`Popover`] hosting the gesture button's
/// custom two-level menu (see [`gesture_overview`]). `appearance(false)` because
/// the menu draws its own card surfaces (plus + flyout); `overlay_closable`
/// stays on so an outside click dismisses and re-clicking the trigger toggles.
/// Closing resets the activated direction (scratch state on the view) so the
/// next open starts on the plus.
fn gesture_overview_popover<Tr>(
    popover_id: impl Into<ElementId>,
    anchor: Anchor,
    trigger: Tr,
    view: Entity<MouseModelView>,
) -> impl IntoElement
where
    Tr: Selectable + IntoElement + 'static,
{
    let view_reset = view.clone();
    Popover::new(popover_id)
        .appearance(false)
        .mouse_button(MouseButton::Left)
        .anchor(anchor)
        .trigger(trigger)
        .on_open_change(move |open, _window, cx| {
            if !*open {
                view_reset.update(cx, |v, vcx| {
                    v.set_gesture_selected_dir(None);
                    vcx.notify();
                });
            }
        })
        .content(move |_state, _window, cx| gesture_overview(&view, cx))
}

/// Position the popover wrapper at the label's slot in the side gutter and
/// host the label card itself. Editable cards wrap the trigger in the same
/// Popover flow as the hotspot dot; native-only cards stay read-only.
#[allow(
    clippy::too_many_arguments,
    reason = "wrapper position + trigger \
state both need this many inputs; bundling would just hide the dependency"
)]
fn label_popover(
    idx: usize,
    label: Label,
    binding: BindingLabel,
    highlighted: bool,
    mouse_left: f32,
    mouse_w: f32,
    hovered: Option<ButtonId>,
    active: Option<ButtonId>,
    gesture_owner: Option<ButtonId>,
    view: &Entity<MouseModelView>,
) -> AnyElement {
    let x = match label.side {
        Side::Left => mouse_left - SIDE_GAP - SIDE_W,
        Side::Right => mouse_left + mouse_w + SIDE_GAP,
    };
    let editable = binding.editable;
    let view = view.clone();
    let trigger = LabelTrigger {
        id: ("label-trigger", idx).into(),
        label,
        binding,
        highlighted: highlighted || hovered == Some(label.id) || active == Some(label.id),
        selected: false,
        view: view.clone(),
    };
    let content: AnyElement = if !editable {
        trigger.into_any_element()
    } else if Some(label.id) == gesture_owner {
        gesture_overview_popover(
            ("label-popover", idx),
            Anchor::TopLeft,
            trigger,
            view.clone(),
        )
        .into_any_element()
    } else {
        Popover::new(("label-popover", idx))
            // `action_picker` draws its own `menu_card` surface, matching the
            // gesture menu — so suppress the framework popover surface.
            .appearance(false)
            .anchor(Anchor::TopLeft)
            .mouse_button(MouseButton::Left)
            .trigger(trigger)
            .content(move |_state, _window, cx| action_picker(label.id, &view, cx))
            .into_any_element()
    };
    div()
        .absolute()
        .left(px(x))
        .top(px(label.y - LABEL_H / 2.))
        .w(px(LABEL_W))
        .h(px(LABEL_H))
        .child(content)
        .into_any_element()
}

struct BindingLabel {
    text: gpui::SharedString,
    is_default: bool,
    /// Vendored action-icon asset path (see [`action_icon_path`]) for the
    /// card's leading glyph, or `None` for the gesture summary / unbound.
    icon: Option<&'static str>,
    editable: bool,
}

fn binding_label_for(
    id: ButtonId,
    layout: MouseButtonLayout,
    bindings: &BTreeMap<ButtonId, Action>,
    gesture_owner: Option<ButtonId>,
) -> BindingLabel {
    if !layout.binds_hotspot(id) {
        return BindingLabel {
            text: tr!("Native"),
            is_default: true,
            icon: None,
            editable: false,
        };
    }
    if Some(id) == gesture_owner {
        return BindingLabel {
            text: tr!("5 directions"),
            is_default: false,
            icon: Some(GESTURE_BUTTON_ICON),
            editable: true,
        };
    }

    // `bindings` is seeded for every `ButtonId::BINDABLE` (agent-core
    // `bindings_for`); fall back to the button's own default to stay total during
    // partial reloads.
    let action = bindings
        .get(&id)
        .cloned()
        .unwrap_or_else(|| default_binding(id));
    BindingLabel {
        text: localized_action_label(&action),
        is_default: action == default_binding(id),
        icon: Some(action_icon_path(&action)),
        editable: true,
    }
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

impl Selectable for LabelTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for LabelTrigger {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let highlighted = self.highlighted || self.selected;
        let btn = self.label.id;
        let view = self.view;
        let pal = theme::palette(cx);
        let binding_color = if highlighted {
            rgb(ACCENT_BLUE).into()
        } else if self.binding.is_default {
            pal.text_muted
        } else {
            pal.text_primary
        };
        // Always show the action the button actually performs; the muted colour
        // (set above for `is_default`) is what signals "not customised" — more
        // informative than the bare word "Default".
        let binding = self.binding.text;
        let binding_icon = self.binding.icon;
        let editable = self.binding.editable;
        v_flex()
            .id(self.id)
            .w(px(LABEL_W))
            .h(px(LABEL_H))
            .px_3()
            .justify_center()
            .gap_0p5()
            .rounded_md()
            .border_1()
            .border_color(if highlighted {
                rgb(ACCENT_BLUE).into()
            } else {
                pal.border
            })
            .bg(if highlighted {
                pal.surface
            } else {
                pal.surface_hover
            })
            .when(editable, |s| s.cursor_pointer().hover(move |s| s.bg(pal.surface)))
            // Button name — the caption (xs / muted), the same size as the
            // popover title and category headers it shares the binding flow with.
            .child(
                div()
                    .text_xs()
                    .text_color(pal.text_muted)
                    .child(tr!(self.label.id.label())),
            )
            // Current binding — the value (sm), the same size as the action rows
            // it edits. Colour, not weight or size, carries the default / set /
            // highlighted state.
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
                            .text_sm()
                            .text_color(binding_color)
                            .child(binding),
                    )
                    .when(editable, |row| {
                        row.child(
                            Icon::new(IconName::ChevronRight)
                                .size_3()
                                .text_color(pal.text_muted),
                        )
                    }),
            )
            .on_hover(move |hovered, _window, cx| {
                let is_hovered = *hovered;
                view.update(cx, |this, cx| {
                    if is_hovered {
                        this.hovered = Some(btn);
                    } else if this.hovered == Some(btn) {
                        this.hovered = None;
                    }
                    cx.notify();
                });
            })
    }
}

fn localized_action_label(action: &Action) -> gpui::SharedString {
    match action {
        Action::SetDpiPreset(index) => {
            tr!("DPI Preset %{index}", index => (index + 1).to_string())
        }
        Action::CustomShortcut(combo) => combo.rendered_label().into(),
        _ => tr!(action.label()),
    }
}

/// Shape-based silhouette used when no asset is cached for the device.
fn silhouette(
    w: f32,
    h: f32,
    pal: Palette,
    layout: MouseButtonLayout,
    perspective: MouseModelPerspective,
) -> impl IntoElement {
    if layout == MouseButtonLayout::G502 && perspective == MouseModelPerspective::View2 {
        g502_side_silhouette(w, h, pal).into_any_element()
    } else {
        default_silhouette(w, h, pal).into_any_element()
    }
}

fn default_silhouette(w: f32, h: f32, pal: Palette) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .w(px(w))
        .h(px(h))
        .rounded_3xl()
        .border_1()
        .border_color(pal.text_muted)
        .bg(pal.surface_hover)
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

fn g502_side_silhouette(w: f32, h: f32, pal: Palette) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .w(px(w))
        .h(px(h))
        .child(
            div()
                .absolute()
                .left(px(w * 0.42))
                .top(px(h * 0.08))
                .w(px(w * 0.26))
                .h(px(h * 0.82))
                .rounded_3xl()
                .border_1()
                .border_color(pal.text_muted)
                .bg(pal.surface_hover),
        )
        .child(
            div()
                .absolute()
                .left(px(w * 0.33))
                .top(px(h * 0.24))
                .w(px(w * 0.16))
                .h(px(h * 0.34))
                .rounded_md()
                .bg(hsla(0., 0., 0.25, 1.0)),
        )
        .child(
            div()
                .absolute()
                .left(px(w * 0.52))
                .top(px(h * 0.14))
                .w(px(w * 0.03))
                .h(px(h * 0.62))
                .rounded_md()
                .bg(pal.border),
        )
}

fn hotspot_popover(
    idx: usize,
    hotspot: Hotspot,
    editable: bool,
    hovered: Option<ButtonId>,
    active: Option<ButtonId>,
    gesture_owner: Option<ButtonId>,
    view: &Entity<MouseModelView>,
) -> AnyElement {
    let view = view.clone();
    let trigger = HotspotTrigger {
        id: ("hotspot-trigger", idx).into(),
        hotspot,
        hovered: hovered == Some(hotspot.id) || active == Some(hotspot.id),
        view: view.clone(),
        selected: false,
        editable,
    };
    // Open the gesture menu only for the button that currently OWNS gestures —
    // matching the side-label path — so a promoted Middle/Back/Forward opens it
    // here too, a demoted gesture button opens the plain picker, and (when gestures
    // are off) no hotspot re-enters the gesture editor.
    let content: AnyElement = if !editable {
        trigger.into_any_element()
    } else if Some(hotspot.id) == gesture_owner {
        gesture_overview_popover(
            ("hotspot-popover", idx),
            Anchor::TopRight,
            trigger,
            view.clone(),
        )
        .into_any_element()
    } else {
        Popover::new(("hotspot-popover", idx))
            // `action_picker` draws its own `menu_card` surface, matching the
            // gesture menu — so suppress the framework popover surface.
            .appearance(false)
            .anchor(Anchor::TopRight)
            .mouse_button(MouseButton::Left)
            .trigger(trigger)
            .content(move |_state, _window, cx| action_picker(hotspot.id, &view, cx))
            .into_any_element()
    };
    div()
        .absolute()
        .left(px(hotspot.x))
        .top(px(hotspot.y))
        .w(px(hotspot.w))
        .h(px(hotspot.h))
        .child(content)
        .into_any_element()
}

#[derive(IntoElement)]
struct HotspotTrigger {
    id: ElementId,
    hotspot: Hotspot,
    hovered: bool,
    view: Entity<MouseModelView>,
    selected: bool,
    editable: bool,
}

impl Selectable for HotspotTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for HotspotTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let highlighted = self.hovered || self.selected;
        let view = self.view;
        let hotspot = self.hotspot;
        let btn = hotspot.id;
        let editable = self.editable;

        div()
            .id(self.id)
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
                    })
                    .opacity(if editable { 1.0 } else { 0.55 }),
            )
            .on_hover(move |hovered, _window, cx| {
                let is_hovered = *hovered;
                view.update(cx, |this, cx| {
                    if is_hovered {
                        this.hovered = Some(btn);
                    } else if this.hovered == Some(btn) {
                        this.hovered = None;
                    }
                    cx.notify();
                });
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gesture_owner_selector_keeps_physical_gesture_button_name() {
        assert_eq!(
            gesture_owner_label(ButtonId::GestureButton),
            "Gesture Button"
        );
    }

    #[test]
    fn g502_gesture_choices_span_both_perspectives() {
        let view_1_labels = MouseButtonLayout::G502.fallback_labels(MouseModelPerspective::View1);

        let capable = gesture_capable_buttons(MouseButtonLayout::G502, &view_1_labels);

        assert!(capable.contains(&ButtonId::MiddleClick));
        assert!(capable.contains(&ButtonId::Back));
        assert!(capable.contains(&ButtonId::Forward));
    }
}
