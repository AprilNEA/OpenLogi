use std::sync::Arc;

use gpui::{
    Anchor, AnyElement, App, Context, ElementId, Entity, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Render, RenderOnce, Role, StatefulInteractiveElement as _, Styled,
    Subscription, Window, canvas, div, hsla, img, prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_component::{Icon, IconName, Selectable, h_flex, popover::Popover, v_flex};
use openlogi_core::binding::{Action, ButtonId, GestureDirection, default_binding};

use super::geometry::{
    asset_dimensions_for_png, asset_has_button_labels, asset_hotspots_for_png, default_labels,
    labels_from_hotspots,
};
use super::hotspots::{Hotspot, MOUSE_MODEL_SIZE, MouseControlId, default_hotspots};
use super::leader_lines::{Geometry as LeaderGeometry, Label, Side, paint as paint_leader_lines};
use super::picker::{
    GESTURE_BUTTON_ICON, action_icon_path, action_picker, gesture_overview, thumbwheel_picker,
};
use super::thumbwheel::ThumbwheelPreset;
use crate::app::{glow_canvas, keyboard_glow};
use crate::services::assets::{GlowGeometry, ResolvedAsset};
use crate::state::AppState;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};

const SIDE_W: f32 = 180.;
const SIDE_GAP: f32 = 24.;
const LABEL_W: f32 = 156.;
const LABEL_H: f32 = 56.;

const CARD_EDGE_INSET: f32 = SIDE_GAP + (SIDE_W - LABEL_W);

const HOTSPOT_DOT: f32 = 12.;

/// Vertical space around the model that it can't draw into: the detail header
/// and footer, plus the buttons-tab padding. The model scales to fit whatever
/// viewport height remains.
const MODEL_VERTICAL_RESERVE: f32 = 224.;
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

/// Interactive mouse model with button hotspots.
pub struct MouseModelView {
    current_device_key: Option<String>,
    hovered: Option<MouseControlId>,
    open_binding_popover: Option<BindingPopover>,
    /// Which gesture direction the open gesture menu has activated (so its
    /// level-2 flyout card shows), or `None` for the plus-only state. Scratch UI
    /// state owned here (like [`Self::hovered`]) rather than in window-keyed
    /// state, so the popover's `on_open_change` — which runs outside paint — can
    /// reset it without tripping gpui's render-only guard.
    gesture_active_dir: Option<GestureDirection>,
    _state_obs: Subscription,
}

impl MouseModelView {
    /// Create the mouse model view.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = cx.observe_global::<AppState>(|_view, cx| cx.notify());
        Self {
            current_device_key: None,
            hovered: None,
            open_binding_popover: None,
            gesture_active_dir: None,
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

    fn set_binding_popover_open(&mut self, popover: BindingPopover, open: bool) {
        if open {
            self.open_binding_popover = Some(popover);
        } else if self.open_binding_popover == Some(popover) {
            self.open_binding_popover = None;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BindingPopover {
    Label(MouseControlId),
    Hotspot(MouseControlId),
}

impl Render for MouseModelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (device_key, asset, active, bindings, gesture_buttons, glow, thumbwheel) = cx
            .try_global::<AppState>()
            .map(|s| {
                (
                    s.current_record().map(|r| r.config_key.clone()),
                    s.current_record().and_then(|r| r.asset.clone()),
                    s.active_button.map(MouseControlId::from_active_button),
                    s.button_bindings.clone(),
                    s.gesture_bindings.keys().copied().collect::<Vec<_>>(),
                    s.current_record().and_then(|r| keyboard_glow(s, r)),
                    s.current_record()
                        .and_then(|r| r.capabilities)
                        .is_some_and(|capabilities| capabilities.thumbwheel),
                )
            })
            .unwrap_or_default();

        if self.current_device_key != device_key {
            self.current_device_key = device_key;
            self.hovered = None;
            self.open_binding_popover = None;
            self.gesture_active_dir = None;
        }

        // Scale the model to fit the content area in *both* axes. A tall mouse
        // is bound by the viewport height (capped at the design height, floored
        // so the side labels stay readable — the window's min height keeps the
        // viewport above the floor, see `main`). A wide keyboard is bound by the
        // available width so it can't overflow the panel (#272), and — having no
        // side labels — drops the label gutter to centre at full width.
        let viewport_h = f32::from(window.viewport_size().height);
        let viewport_w = f32::from(window.viewport_size().width);
        let target_h = (viewport_h - MODEL_VERTICAL_RESERVE).clamp(MODEL_MIN_H, MOUSE_MODEL_SIZE.1);
        let has_labels = asset.as_ref().is_none_or(asset_has_button_labels);
        let gutter = if has_labels { SIDE_W + SIDE_GAP } else { 0. };
        let content_w =
            (viewport_w - MODEL_HORIZONTAL_RESERVE).clamp(MODEL_MIN_CONTENT_W, MODEL_CONTENT_MAX_W);
        let max_image_w = (content_w - gutter).max(MODEL_MIN_CONTENT_W / 2.);
        let (mouse_w, mouse_h, hotspots, labels) =
            scaled_model(asset.as_ref(), target_h, max_image_w, thumbwheel);

        let canvas_w = gutter + mouse_w;
        let canvas_h = mouse_h;
        let mouse_left = gutter;

        let highlight = self.hovered.or(active);
        let view = cx.entity();
        let hovered = self.hovered;
        let pal = theme::palette(cx);

        let hotspots_outer = hotspots.clone();
        let labels_outer = labels.clone();
        let leader_canvas = leader_canvas(hotspots, labels, highlight, mouse_left, mouse_w);
        let breathing_art = breathing_art(asset.as_ref(), mouse_left, mouse_w, mouse_h, pal, glow);
        let hotspots_layer = hotspots_layer(
            &hotspots_outer,
            mouse_left,
            mouse_w,
            mouse_h,
            hovered,
            active,
            &gesture_buttons,
            self.open_binding_popover,
            &view,
        );
        let canvas = div()
            .relative()
            .w(px(canvas_w))
            .h(px(canvas_h))
            .child(breathing_art)
            .child(leader_canvas)
            .children(labels_outer.iter().enumerate().map(|(idx, label)| {
                let binding = binding_label_for_control(label.id, &bindings, &gesture_buttons);
                label_popover(
                    idx,
                    *label,
                    binding,
                    highlight == Some(label.id),
                    mouse_left,
                    mouse_w,
                    hovered,
                    active,
                    label
                        .id
                        .button()
                        .filter(|button| gesture_buttons.contains(button)),
                    self.open_binding_popover == Some(BindingPopover::Label(label.id)),
                    &view,
                )
            }))
            .child(hotspots_layer);

        // Gesture mode is a per-button fact edited inside each button's own
        // picker (the "Gestures" entry) — no device-level selector row.
        v_flex().w(px(canvas_w)).gap_4().child(canvas)
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
) -> (f32, f32, Vec<Hotspot>, Vec<Label>) {
    if let Some(a) = asset {
        let (w, h) = asset_dimensions_for_png(a, target_h, max_w);
        let hotspots = asset_hotspots_for_png(a, w, h);
        let labels = labels_from_hotspots(&hotspots, h);
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
        let labels = default_labels(thumbwheel)
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

#[allow(
    clippy::too_many_arguments,
    reason = "layout inputs + hover/active/gesture state; bundling would just hide the dependency"
)]
fn hotspots_layer(
    hotspots: &[Hotspot],
    mouse_left: f32,
    mouse_w: f32,
    mouse_h: f32,
    hovered: Option<MouseControlId>,
    active: Option<MouseControlId>,
    gesture_buttons: &[ButtonId],
    open_popover: Option<BindingPopover>,
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
                hovered,
                active,
                hotspot
                    .id
                    .button()
                    .filter(|button| gesture_buttons.contains(button)),
                open_popover == Some(BindingPopover::Hotspot(hotspot.id)),
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
    btn: ButtonId,
    trigger: Tr,
    binding_popover: BindingPopover,
    open: bool,
    view: Entity<MouseModelView>,
) -> impl IntoElement
where
    Tr: Selectable + IntoElement + 'static,
{
    let view_state = view.clone();
    Popover::new(popover_id)
        .appearance(false)
        .mouse_button(MouseButton::Left)
        .anchor(anchor)
        .trigger(trigger)
        .open(open)
        .on_open_change(move |open, _window, cx| {
            view_state.update(cx, |v, vcx| {
                v.set_binding_popover_open(binding_popover, *open);
                if !*open {
                    v.set_gesture_selected_dir(None);
                }
                vcx.notify();
            });
        })
        .content(move |_state, _window, cx| gesture_overview(btn, &view, cx))
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
    hovered: Option<MouseControlId>,
    active: Option<MouseControlId>,
    // `Some` exactly when the control is a button in gesture mode — that button
    // opens its gesture menu instead of the plain picker.
    gesture_button: Option<ButtonId>,
    open: bool,
    view: &Entity<MouseModelView>,
) -> AnyElement {
    let x = match label.side {
        Side::Left => mouse_left - SIDE_GAP - SIDE_W,
        Side::Right => mouse_left + mouse_w + SIDE_GAP,
    };
    let view = view.clone();
    let binding_popover = BindingPopover::Label(label.id);
    let trigger = LabelTrigger {
        id: ("label-trigger", idx).into(),
        label,
        binding,
        highlighted: highlighted || hovered == Some(label.id) || active == Some(label.id),
        selected: false,
        view: view.clone(),
    };
    let popover: AnyElement = if let Some(button) = gesture_button {
        gesture_overview_popover(
            ("label-popover", idx),
            Anchor::TopLeft,
            button,
            trigger,
            binding_popover,
            open,
            view.clone(),
        )
        .into_any_element()
    } else {
        let view_state = view.clone();
        let view_content = view.clone();
        Popover::new(("label-popover", idx))
            // `action_picker` draws its own `menu_card` surface, matching the
            // gesture menu — so suppress the framework popover surface.
            .appearance(false)
            .anchor(Anchor::TopLeft)
            .mouse_button(MouseButton::Left)
            .trigger(trigger)
            .open(open)
            .on_open_change(move |open, _window, cx| {
                view_state.update(cx, |v, vcx| {
                    v.set_binding_popover_open(binding_popover, *open);
                    vcx.notify();
                });
            })
            .content(move |_state, _window, cx| match label.id {
                MouseControlId::Button(button) => action_picker(button, &view_content, cx),
                MouseControlId::ThumbwheelRotation => thumbwheel_picker(&view, cx),
            })
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
        let selected = self.selected;
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
        let binding_description = binding.clone();
        let binding_icon = self.binding.icon;
        let button_name = tr!(self.label.id.label());
        v_flex()
            .id(self.id)
            .role(Role::Button)
            .aria_label(tr!("Bind %{name}", name => button_name.clone()))
            .aria_description(binding_description)
            .aria_expanded(selected)
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
                pal.surface
            } else {
                pal.surface_hover
            })
            .cursor_pointer()
            .hover(move |s| s.bg(pal.surface))
            // Button name — the caption (xs / muted), the same size as the
            // popover title and category headers it shares the binding flow with.
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(button_name),
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
            // Popover owns the trigger gesture and updates controlled state via
            // `on_open_change`. A second click toggle here would immediately close
            // the menu on mouse-up, producing the one-frame flash regression.
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
            is_default: false,
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
                is_default: action == default_binding(button),
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
                    is_default: preset == ThumbwheelPreset::HorizontalScroll,
                    icon: Some(preset.icon()),
                }
            } else {
                BindingLabel {
                    text: tr!("Custom"),
                    is_default: false,
                    icon: Some("action-icons/chevrons-right.svg"),
                }
            }
        }
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
    hovered: Option<MouseControlId>,
    active: Option<MouseControlId>,
    // `Some` exactly when the control is a button in gesture mode — that button
    // opens its gesture menu instead of the plain picker.
    gesture_button: Option<ButtonId>,
    open: bool,
    view: &Entity<MouseModelView>,
) -> AnyElement {
    let view = view.clone();
    let binding_popover = BindingPopover::Hotspot(hotspot.id);
    let trigger = HotspotTrigger {
        id: ("hotspot-trigger", idx).into(),
        hotspot,
        hovered: hovered == Some(hotspot.id) || active == Some(hotspot.id),
        view: view.clone(),
        selected: false,
    };
    // Open the gesture menu for any button in gesture mode — matching the
    // side-label path — so a promoted Middle/Back/Forward opens it here too and
    // a demoted button opens the plain picker.
    let popover: AnyElement = if let Some(button) = gesture_button {
        gesture_overview_popover(
            ("hotspot-popover", idx),
            Anchor::TopRight,
            button,
            trigger,
            binding_popover,
            open,
            view.clone(),
        )
        .into_any_element()
    } else {
        let view_state = view.clone();
        let view_content = view.clone();
        Popover::new(("hotspot-popover", idx))
            // `action_picker` draws its own `menu_card` surface, matching the
            // gesture menu — so suppress the framework popover surface.
            .appearance(false)
            .anchor(Anchor::TopRight)
            .mouse_button(MouseButton::Left)
            .trigger(trigger)
            .open(open)
            .on_open_change(move |open, _window, cx| {
                view_state.update(cx, |v, vcx| {
                    v.set_binding_popover_open(binding_popover, *open);
                    vcx.notify();
                });
            })
            .content(move |_state, _window, cx| match hotspot.id {
                MouseControlId::Button(button) => action_picker(button, &view_content, cx),
                MouseControlId::ThumbwheelRotation => thumbwheel_picker(&view, cx),
            })
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
        let selected = self.selected;
        let view = self.view;
        let hotspot = self.hotspot;
        let btn = hotspot.id;

        div()
            .id(self.id)
            .role(Role::Button)
            .aria_label(tr!("Bind %{name}", name => tr!(btn.label())))
            .aria_expanded(selected)
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
            // Popover owns the trigger gesture and updates controlled state via
            // `on_open_change`. A second click toggle here would immediately close
            // the menu on mouse-up, producing the one-frame flash regression.
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
        let (_, _, without, _) = scaled_model(None, 560., 420., false);
        let (_, _, with, _) = scaled_model(None, 560., 420., true);
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
