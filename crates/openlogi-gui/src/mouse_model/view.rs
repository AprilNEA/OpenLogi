use gpui::{
    Anchor, AnyElement, App, Context, DismissEvent, ElementId, Entity, Focusable as _, FontWeight,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, RenderOnce,
    StatefulInteractiveElement as _, Styled, Subscription, Window, canvas, div, hsla, img, px, rgb,
};
use gpui_component::{Selectable, h_flex, menu::PopupMenu, popover::Popover, v_flex};

use crate::asset::ResolvedAsset;
use crate::data::mouse_buttons::{
    Action, ButtonId, Hotspot, MOUSE_MODEL_SIZE, default_binding, default_hotspots,
};
use crate::mouse_model::geometry::{
    asset_dimensions_for_png, asset_hotspots_for_png, default_labels, labels_from_hotspots,
};
use crate::mouse_model::leader_lines::{
    Geometry as LeaderGeometry, Label, Side, paint as paint_leader_lines,
};
use crate::mouse_model::picker::{action_picker, build_gesture_menu};
use crate::state::AppState;
use crate::theme::{self, ACCENT_BLUE, Palette};

const SIDE_W: f32 = 210.;
const SIDE_GAP: f32 = 26.;
const LABEL_W: f32 = 190.;
const LABEL_H: f32 = 60.;

const CARD_EDGE_INSET: f32 = SIDE_GAP + (SIDE_W - LABEL_W);

const HOTSPOT_DOT: f32 = 12.;

/// Interactive mouse model with button hotspots.
pub struct MouseModelView {
    hovered: Option<ButtonId>,
    _state_obs: Subscription,
}

impl MouseModelView {
    /// Create the mouse model view.
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
                let (w, h) = asset_dimensions_for_png(a, mouse_h);
                let hotspots = asset_hotspots_for_png(a, w, h);
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
        let pal = theme::palette(cx);

        let hotspots_outer = hotspots.clone();
        let labels_outer = labels.clone();
        let leader_canvas = leader_canvas(hotspots, labels, highlight, mouse_left, mouse_w);
        let breathing_art = breathing_art(asset.as_ref(), mouse_left, mouse_w, mouse_h, pal);
        let hotspots_layer = hotspots_layer(
            &hotspots_outer,
            mouse_left,
            mouse_w,
            mouse_h,
            hovered,
            active,
            &view,
        );

        div()
            .relative()
            .w(px(canvas_w))
            .h(px(canvas_h))
            .child(breathing_art)
            .child(leader_canvas)
            .children(labels_outer.iter().enumerate().map(|(idx, label)| {
                let binding = if label.id == ButtonId::GestureButton {
                    BindingLabel {
                        text: tr!("5 directions"),
                        is_default: false,
                    }
                } else {
                    match bindings.get(&label.id) {
                        Some(action) => BindingLabel {
                            text: localized_action_label(action),
                            is_default: *action == default_binding(label.id),
                        },
                        None => BindingLabel {
                            text: tr!("Unbound"),
                            is_default: false,
                        },
                    }
                };
                label_popover(
                    idx,
                    *label,
                    binding,
                    highlight == Some(label.id),
                    mouse_left,
                    mouse_w,
                    hovered,
                    active,
                    &view,
                )
            }))
            .child(hotspots_layer)
    }
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
    mouse_left: f32,
    mouse_w: f32,
    mouse_h: f32,
    pal: Palette,
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
        .child(device_art)
}

fn hotspots_layer(
    hotspots: &[Hotspot],
    mouse_left: f32,
    mouse_w: f32,
    mouse_h: f32,
    hovered: Option<ButtonId>,
    active: Option<ButtonId>,
    view: &Entity<MouseModelView>,
) -> impl IntoElement {
    div()
        .absolute()
        .left(px(mouse_left))
        .top(px(0.))
        .w(px(mouse_w))
        .h(px(mouse_h))
        .children(
            hotspots
                .iter()
                .enumerate()
                .map(|(idx, hotspot)| hotspot_popover(idx, *hotspot, hovered, active, view)),
        )
}

/// One-shot cache for the gesture button's [`PopupMenu`] entity so the
/// popover's per-frame `content` closure reuses it instead of rebuilding —
/// rebuilding every frame would reset submenu hover state, so a hovered-open
/// submenu would snap shut. Cleared on dismiss so the next open rebuilds with
/// fresh `checked` state. Mirrors gpui-component's `DropdownMenu`.
#[derive(Default)]
struct GestureMenuState {
    menu: Option<Entity<PopupMenu>>,
}

/// Wrap `trigger` in a left-click [`Popover`] hosting the gesture button's
/// native cascading [`PopupMenu`]. The popover surface is suppressed
/// (`appearance(false)`) because the menu draws its own; outside-click dismissal
/// is driven by the menu and relayed here via [`DismissEvent`].
fn gesture_menu_popover<Tr>(
    popover_id: impl Into<ElementId>,
    state_key: impl Into<ElementId>,
    anchor: Anchor,
    trigger: Tr,
) -> impl IntoElement
where
    Tr: Selectable + IntoElement + 'static,
{
    let state_key = state_key.into();
    Popover::new(popover_id)
        .appearance(false)
        .overlay_closable(false)
        .mouse_button(MouseButton::Left)
        .anchor(anchor)
        .trigger(trigger)
        .content(move |_state, window, cx| {
            let menu_state =
                window.use_keyed_state(state_key.clone(), cx, |_, _| GestureMenuState::default());
            if let Some(menu) = menu_state.read(cx).menu.clone() {
                return menu;
            }

            let menu = PopupMenu::build(window, cx, build_gesture_menu);
            menu_state.update(cx, |state, _| state.menu = Some(menu.clone()));
            menu.focus_handle(cx).focus(window, cx);

            // Closing the menu (pick or outside-click) emits DismissEvent; relay
            // it to the host popover and drop the cache so the next open rebuilds.
            let popover_state = cx.entity();
            let cache = menu_state.clone();
            window
                .subscribe(&menu, cx, move |_, _: &DismissEvent, window, cx| {
                    popover_state.update(cx, |state, cx| state.dismiss(window, cx));
                    cache.update(cx, |state, _| state.menu = None);
                })
                .detach();

            menu
        })
}

/// Position the popover wrapper at the label's slot in the side gutter and
/// host a Popover whose trigger is the label card itself. Same picker
/// content as the hotspot dot — clicking either entry point lands on the
/// same binding flow.
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
    view: &Entity<MouseModelView>,
) -> AnyElement {
    let x = match label.side {
        Side::Left => mouse_left - SIDE_GAP - SIDE_W,
        Side::Right => mouse_left + mouse_w + SIDE_GAP,
    };
    let view = view.clone();
    let trigger = LabelTrigger {
        id: ("label-trigger", idx).into(),
        label,
        binding,
        highlighted: highlighted || hovered == Some(label.id) || active == Some(label.id),
        selected: false,
        view: view.clone(),
    };
    let popover: AnyElement = if label.id == ButtonId::GestureButton {
        gesture_menu_popover(
            ("label-popover", idx),
            ("label-gesture-menu", idx),
            Anchor::TopLeft,
            trigger,
        )
        .into_any_element()
    } else {
        Popover::new(("label-popover", idx))
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
        .child(popover)
        .into_any_element()
}

struct BindingLabel {
    text: gpui::SharedString,
    is_default: bool,
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
        let binding = if self.binding.is_default {
            tr!("Default")
        } else {
            self.binding.text
        };
        div()
            .id(self.id)
            .w(px(LABEL_W))
            .h(px(LABEL_H))
            .px_3()
            .py_2()
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
            .cursor_pointer()
            .hover(move |s| s.bg(pal.surface))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1p5()
                            .text_xs()
                            .line_height(gpui::relative(1.15))
                            .text_color(pal.text_muted)
                            .child(div().text_xs().child("•"))
                            .child(tr!(self.label.id.label())),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(if self.binding.is_default {
                                        FontWeight::NORMAL
                                    } else {
                                        FontWeight::SEMIBOLD
                                    })
                                    .text_color(binding_color)
                                    .child(binding),
                            )
                            .child(div().text_sm().text_color(pal.text_muted).child("›")),
                    ),
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
        // A few navigation actions carry macOS vocabulary in the core action
        // enum; show the matching Windows concept on Windows.
        #[cfg(target_os = "windows")]
        Action::MissionControl | Action::AppExpose => tr!("Task View"),
        #[cfg(target_os = "windows")]
        Action::LaunchpadShow => tr!("Start Menu"),
        Action::SetDpiPreset(index) => {
            tr!("DPI Preset %{index}", index => (index + 1).to_string())
        }
        Action::CustomShortcut(combo) => combo.rendered_label().into(),
        _ => tr!(action.label()),
    }
}

/// Shape-based silhouette used when no asset is cached for the device.
fn silhouette(w: f32, h: f32, pal: Palette) -> impl IntoElement {
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

fn hotspot_popover(
    idx: usize,
    hotspot: Hotspot,
    hovered: Option<ButtonId>,
    active: Option<ButtonId>,
    view: &Entity<MouseModelView>,
) -> AnyElement {
    let view = view.clone();
    let trigger = HotspotTrigger {
        id: ("hotspot-trigger", idx).into(),
        hotspot,
        hovered: hovered == Some(hotspot.id) || active == Some(hotspot.id),
        view: view.clone(),
        selected: false,
    };
    let popover: AnyElement = if hotspot.id == ButtonId::GestureButton {
        gesture_menu_popover(
            ("hotspot-popover", idx),
            ("hotspot-gesture-menu", idx),
            Anchor::TopRight,
            trigger,
        )
        .into_any_element()
    } else {
        Popover::new(("hotspot-popover", idx))
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
        .child(popover)
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
