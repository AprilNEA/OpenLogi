//! Centre-of-screen mouse silhouette with clickable hotspots and side
//! labels connected by leader lines.
//!
//! Per UI.md phases 6 (silhouette + hotspots), 7 (labels + leader lines),
//! and 8 (breathing). When a [`ResolvedAsset`] is supplied by the asset
//! cache the synthetic silhouette is replaced by the real device PNG and
//! the hotspot/label positions come from the Logitech-format
//! `core_metadata.json`. Without an asset, we fall back to the original
//! shape-based silhouette plus [`default_hotspots`] / [`default_labels`].

use std::time::Duration;

use gpui::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, Context, ElementId, Entity, FontWeight,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Render, RenderOnce,
    StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, ease_in_out, hsla,
    img, px, rgb,
};
use gpui_component::{Selectable, popover::Popover, v_flex};

use openlogi_assets::Metadata;

use crate::data::mouse_buttons::{Action, ButtonId, Hotspot, MOUSE_MODEL_SIZE, default_hotspots};
use crate::mouse_model::leader_lines::{
    Geometry as LeaderGeometry, Label, Side, paint as paint_leader_lines,
};
use crate::mouse_model::picker::action_picker;
use crate::state::AppState;
use crate::theme::{ACCENT_BLUE, BORDER, SURFACE_HOVER, TEXT_MUTED, TEXT_PRIMARY};

// Side-gutter geometry. Labels sit on the *left* of the mouse so the right
// half of the window is free for the DPI / gesture config column.
const SIDE_W: f32 = 180.;
const SIDE_GAP: f32 = 24.;
const LABEL_W: f32 = 156.;
const LABEL_H: f32 = 44.;

/// Horizontal distance from the mouse silhouette's edge to the nearer
/// edge of a label card. Leader lines terminate at this offset so they
/// touch the card without crossing into the text.
const CARD_EDGE_INSET: f32 = SIDE_GAP + (SIDE_W - LABEL_W);

/// Vertical amplitude of the breathing loop. Two pixels reads as a soft
/// rise/fall without feeling unstable.
const BREATH_AMPLITUDE: f32 = 2.0;

/// Approx pixel width of each hotspot hit-target. Logitech only gives us a
/// marker point per button, not a rectangle, so we size by hand.
const ASSET_HOTSPOT: f32 = 56.;

pub struct MouseModelView {
    hovered: Option<ButtonId>,
    /// Repaints when the carousel switches devices. Held by value so the
    /// subscription stays alive for the entity's lifetime.
    _state_obs: Subscription,
}

impl MouseModelView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.observe_global::<AppState>(|_view, cx| cx.notify());
        Self {
            hovered: None,
            _state_obs: state_obs,
        }
    }
}

impl Render for MouseModelView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Pull everything that depends on the active device out of AppState
        // up front. Cloning is cheap (small structs, single small Vec) and it
        // releases the global borrow before the GPUI builders below.
        let (asset, active, bindings) = cx
            .try_global::<AppState>()
            .map(|s| {
                (
                    s.current_record().and_then(|r| r.asset.clone()),
                    s.active_button,
                    s.button_bindings.clone(),
                )
            })
            .unwrap_or_default();

        let (mouse_w, mouse_h) = MOUSE_MODEL_SIZE;
        let (mouse_w, mouse_h, hotspots, labels) = match asset.as_ref() {
            Some(a) => {
                let (w, h) = asset_dimensions(&a.metadata, mouse_h);
                let hotspots = asset_hotspots(&a.metadata, w, h);
                let labels = labels_from_hotspots(&hotspots);
                (w, h, hotspots, labels)
            }
            None => (mouse_w, mouse_h, default_hotspots(), default_labels()),
        };

        let canvas_w = SIDE_W + SIDE_GAP + mouse_w;
        let canvas_h = mouse_h;
        let mouse_left = SIDE_W + SIDE_GAP;

        let highlight = self.hovered.or(active);
        let view = cx.entity();
        let hovered = self.hovered;

        // The canvas closure takes ownership of one copy of hotspots/labels;
        // clone here so the outer label_card and hotspot_popover loops can
        // iterate over their own.
        let hotspots_outer = hotspots.clone();
        let labels_outer = labels.clone();
        let leader_canvas = canvas(
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
        .size_full();

        // Either the real device image, or the shape-based silhouette.
        let device_art: AnyElement = match asset.as_ref() {
            Some(a) => img(a.image_path.clone())
                .w(px(mouse_w))
                .h(px(mouse_h))
                .into_any_element(),
            None => silhouette(mouse_w, mouse_h).into_any_element(),
        };

        div()
            .relative()
            .w(px(canvas_w))
            .h(px(canvas_h))
            .child(leader_canvas)
            .children(labels_outer.iter().map(|label| {
                let binding = bindings
                    .get(&label.id)
                    .map_or_else(|| "Unbound".to_string(), Action::label);
                label_card(
                    label,
                    binding,
                    highlight == Some(label.id),
                    mouse_left,
                    mouse_w,
                )
            }))
            .child(
                div()
                    .absolute()
                    .left(px(mouse_left))
                    .top(px(0.))
                    .w(px(mouse_w))
                    .h(px(mouse_h))
                    .child(device_art)
                    .children(hotspots_outer.iter().enumerate().map(|(idx, hotspot)| {
                        hotspot_popover(idx, *hotspot, hovered, active, &view)
                    }))
                    .with_animation(
                        "mouse-breath",
                        Animation::new(Duration::from_secs(4))
                            .repeat()
                            .with_easing(ease_in_out),
                        |this, delta| {
                            let dy = (delta * std::f32::consts::TAU).sin() * BREATH_AMPLITUDE;
                            this.top(px(dy))
                        },
                    ),
            )
    }
}

/// Scale the device image to fit a target height while preserving aspect.
#[allow(
    clippy::cast_precision_loss,
    reason = "device images are < 4096 px on either axis — well within f32 mantissa"
)]
fn asset_dimensions(meta: &Metadata, target_h: f32) -> (f32, f32) {
    let Some(origin) = meta.origin() else {
        return MOUSE_MODEL_SIZE;
    };
    let w = target_h * (origin.width as f32) / (origin.height as f32);
    (w, target_h)
}

/// Convert Logitech's percent-based markers into mouse-local pixel rects.
/// Each marker is a point, so we centre a fixed-size hit area on it.
/// Unknown slot names fall through silently — extending `ButtonId` and
/// `map_slot_name` brings more hotspots online.
///
/// Logi metadata omits primary clicks (Options+ doesn't expose them), so we
/// append fallback hotspots for `LeftClick` / `RightClick` at standard
/// top-of-mouse positions when missing — OpenLogi lets users bind those too.
fn asset_hotspots(meta: &Metadata, mouse_w: f32, mouse_h: f32) -> Vec<Hotspot> {
    let mut hotspots: Vec<Hotspot> = meta
        .assignments()
        .filter_map(|a| {
            let id = map_slot_name(&a.slot_name)?;
            let cx = a.marker.x / 100. * mouse_w;
            let cy = a.marker.y / 100. * mouse_h;
            Some(Hotspot {
                id,
                x: cx - ASSET_HOTSPOT / 2.,
                y: cy - ASSET_HOTSPOT / 2.,
                w: ASSET_HOTSPOT,
                h: ASSET_HOTSPOT,
            })
        })
        .collect();
    let has_left = hotspots.iter().any(|h| h.id == ButtonId::LeftClick);
    let has_right = hotspots.iter().any(|h| h.id == ButtonId::RightClick);
    let make_default = |id: ButtonId, px_x: f32, px_y: f32| Hotspot {
        id,
        x: px_x - ASSET_HOTSPOT / 2.,
        y: px_y - ASSET_HOTSPOT / 2.,
        w: ASSET_HOTSPOT,
        h: ASSET_HOTSPOT,
    };
    if !has_left {
        hotspots.push(make_default(
            ButtonId::LeftClick,
            0.28 * mouse_w,
            0.12 * mouse_h,
        ));
    }
    if !has_right {
        hotspots.push(make_default(
            ButtonId::RightClick,
            0.58 * mouse_w,
            0.12 * mouse_h,
        ));
    }
    hotspots
}

/// Logitech's stable slot vocabulary → OpenLogi's `ButtonId`. Intentionally
/// conservative; unknown names fall through so widening `ButtonId` later
/// doesn't break old depots.
fn map_slot_name(name: &str) -> Option<ButtonId> {
    match name {
        "SLOT_NAME_LEFT_BUTTON" => Some(ButtonId::LeftClick),
        "SLOT_NAME_RIGHT_BUTTON" => Some(ButtonId::RightClick),
        "SLOT_NAME_MIDDLE_BUTTON" => Some(ButtonId::MiddleClick),
        "SLOT_NAME_BACK_BUTTON" => Some(ButtonId::Back),
        "SLOT_NAME_FORWARD_BUTTON" => Some(ButtonId::Forward),
        "SLOT_NAME_MODESHIFT_BUTTON" => Some(ButtonId::DpiToggle),
        "SLOT_NAME_THUMBWHEEL" => Some(ButtonId::Thumbwheel),
        "SLOT_NAME_GESTURE_BUTTON" => Some(ButtonId::GestureButton),
        _ => None,
    }
}

/// Lay labels out on the left side, evenly spaced down the mouse's vertical
/// extent in the same order the hotspots appear in the asset metadata.
/// Logitech's `label.{x,y}` direction codes are ignored for now — the
/// current layout reserves the right gutter for the DPI / gesture column.
#[allow(
    clippy::cast_precision_loss,
    reason = "hotspot count is bounded by ButtonId variants — well under f32 mantissa"
)]
fn labels_from_hotspots(hotspots: &[Hotspot]) -> Vec<Label> {
    if hotspots.is_empty() {
        return Vec::new();
    }
    let mouse_h = MOUSE_MODEL_SIZE.1;
    let step = mouse_h / (hotspots.len() as f32 + 1.);
    hotspots
        .iter()
        .enumerate()
        .map(|(i, h)| Label {
            id: h.id,
            side: Side::Left,
            y: step * (i as f32 + 1.),
        })
        .collect()
}

fn default_labels() -> Vec<Label> {
    vec![
        Label {
            id: ButtonId::LeftClick,
            side: Side::Left,
            y: 60.,
        },
        Label {
            id: ButtonId::RightClick,
            side: Side::Left,
            y: 130.,
        },
        Label {
            id: ButtonId::MiddleClick,
            side: Side::Left,
            y: 200.,
        },
        Label {
            id: ButtonId::Back,
            side: Side::Left,
            y: 290.,
        },
        Label {
            id: ButtonId::Forward,
            side: Side::Left,
            y: 360.,
        },
        Label {
            id: ButtonId::DpiToggle,
            side: Side::Left,
            y: 440.,
        },
    ]
}

fn label_card(
    label: &Label,
    binding: String,
    highlighted: bool,
    mouse_left: f32,
    mouse_w: f32,
) -> impl IntoElement {
    let x = match label.side {
        Side::Left => mouse_left - SIDE_GAP - SIDE_W,
        Side::Right => mouse_left + mouse_w + SIDE_GAP,
    };

    div()
        .absolute()
        .left(px(x))
        .top(px(label.y - LABEL_H / 2.))
        .w(px(LABEL_W))
        .h(px(LABEL_H))
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(if highlighted { ACCENT_BLUE } else { BORDER }))
        .bg(rgb(SURFACE_HOVER))
        .child(
            v_flex()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(label.id.label()),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(if highlighted {
                            ACCENT_BLUE
                        } else {
                            TEXT_PRIMARY
                        }))
                        .child(binding),
                ),
        )
}

/// Shape-based silhouette used when no asset is cached for the device.
fn silhouette(w: f32, h: f32) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .w(px(w))
        .h(px(h))
        .rounded_3xl()
        .border_1()
        .border_color(rgb(TEXT_MUTED))
        .bg(rgb(SURFACE_HOVER))
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
                .bg(rgb(BORDER)),
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

fn hotspot_popover(
    idx: usize,
    hotspot: Hotspot,
    hovered: Option<ButtonId>,
    active: Option<ButtonId>,
    view: &Entity<MouseModelView>,
) -> AnyElement {
    // Position the Popover wrapper, not the trigger. gpui-component's
    // Popover renders its trigger inside a parent div that carries the
    // `on_mouse_down` handler; if the trigger is `.absolute()`, the
    // wrapper div collapses to 0×0 and clicks never hit the handler.
    // Putting `.absolute()` on the wrapper and a w_full/h_full trigger
    // keeps the wrapper sized to the hotspot.
    let view = view.clone();
    let trigger = HotspotTrigger {
        id: ("hotspot-trigger", idx).into(),
        hotspot,
        hovered: hovered == Some(hotspot.id) || active == Some(hotspot.id),
        view: view.clone(),
        selected: false,
    };
    div()
        .absolute()
        .left(px(hotspot.x))
        .top(px(hotspot.y))
        .w(px(hotspot.w))
        .h(px(hotspot.h))
        .child(
            Popover::new(("hotspot-popover", idx))
                .anchor(Anchor::TopRight)
                .mouse_button(MouseButton::Left)
                .trigger(trigger)
                .content(move |_state, _window, cx| action_picker(hotspot.id, &view, cx)),
        )
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

        // Explicit pixel dimensions, not `.size_full()`. gpui-component's
        // Popover wraps the trigger in a `div().child(trigger)` with no
        // explicit size — that div sizes to its child. If the child is
        // `.size_full()`, the resolved size is 0×0 (no parent reference
        // for the percentage) and the popover's `on_mouse_down` never
        // receives clicks. Painting explicit pixels gives the popover's
        // wrapper a real hit-test region.
        div()
            .id(self.id)
            .w(px(hotspot.w))
            .h(px(hotspot.h))
            .rounded_md()
            .border_2()
            .border_color(if highlighted {
                rgb(ACCENT_BLUE).into()
            } else {
                hsla(0., 0., 0., 0.)
            })
            .bg(if highlighted {
                hsla(0.6, 0.85, 0.6, 0.18)
            } else {
                hsla(0., 0., 0., 0.)
            })
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
            // Diagnostic only — confirms clicks reach the hotspot trigger.
            // Does not stop_propagation so the surrounding Popover can still
            // observe the same event and toggle open.
            .on_mouse_down(MouseButton::Left, move |_ev: &MouseDownEvent, _window, _cx| {
                tracing::info!(?btn, "hotspot click reached trigger");
            })
    }
}
