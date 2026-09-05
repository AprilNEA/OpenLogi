//! The ring itself: the GPUI view, and the window it is drawn in.
//!
//! Placement is the interesting part — the panel is centred on the cursor and
//! clamped to the display it came up on, so a ring raised near a screen edge
//! stays whole instead of being cut off.
//!
//! The ring is eight free-floating slots around a cancel button, with the
//! desktop showing through between them; there is no backing disc. On open the
//! slots fly out from the cursor to their places on the ring, so the motion
//! itself says where the ring came from.

#[cfg(any(not(target_os = "windows"), test))]
use gpui::{Bounds, Pixels, Point, Size, point};
use gpui::{
    Animation, AnimationExt as _, Context, Hsla, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Window,
    WindowBackgroundAppearance, WindowKind, WindowOptions, div, prelude::FluentBuilder as _, px,
    svg,
};
use openlogi_core::action_ring::geometry::{CANCEL_DIAMETER, RADIUS, SLOT_DIAMETER};
use openlogi_core::binding::{Action, ActionRingSlot};
use openlogi_ipc::ActionRingInvocation;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::agent::OverlayCommand;
use crate::session::{ClickAwaySession, ShowingRing};

/// The window is the ring (`openlogi_core::action_ring::geometry`) plus room
/// for the hover label under it and the slot shadows.
pub(crate) const WINDOW_SIZE: f32 = 300.0;
pub(crate) const SLOT_SIZE: f32 = SLOT_DIAMETER;
const CANCEL_SIZE: f32 = CANCEL_DIAMETER;
const SLOT_GLYPH: f32 = 20.0;
const CANCEL_GLYPH_SIZE: f32 = 16.0;

/// How long the slots take to fly out from the cursor to their places on the
/// ring. Short enough to read as the ring *appearing* rather than a transition
/// the user waits on; the cubic ease-out front-loads the motion, so the slots
/// are most of the way home by the midpoint and the tail only settles them.
const FLY_OUT: Duration = Duration::from_millis(180);

/// Cubic ease-out: fast off the cursor, settling gently into place.
fn ease_out_cubic(delta: f32) -> f32 {
    1.0 - (1.0 - delta).powi(3)
}

/// The ring's own neutral scale. It floats over whatever is on the desktop, so
/// unlike the settings app it cannot take its surfaces from the OS appearance —
/// it commits to dark slots and rides its own contrast. Only the accent is
/// shared (`openlogi_ui::color`); these greys are local by nature.
const LABEL_PILL: Hsla = neutral(0.06, 0.82);
const SLOT_RESTING: Hsla = neutral(0.16, 0.98);
const CANCEL_RESTING: Hsla = neutral(0.20, 0.98);
const GLYPH: Hsla = neutral(0.98, 1.0);
const LABEL: Hsla = neutral(0.94, 1.0);
const CANCEL_GLYPH: Hsla = neutral(0.82, 1.0);

const fn neutral(lightness: f32, alpha: f32) -> Hsla {
    Hsla {
        h: 0.0,
        s: 0.0,
        l: lightness,
        a: alpha,
    }
}

/// The accent deepened for the dark panel: the brand lightness sits too close to
/// the white glyph a selected slot carries, so the fill drops to `0.48` and the
/// ring around it rises to `0.78`. Both keep the brand hue and saturation.
const SELECTED_FILL_L: f32 = 0.48;
const SELECTED_BORDER_L: f32 = 0.78;

pub(crate) struct RingView {
    invocation: ActionRingInvocation,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    hovered: Option<ActionRingSlot>,
    /// Whether the fly-out has been started. GPUI draws a new window once,
    /// synchronously, inside `open_window` — before the platform has put it
    /// on screen — and an animation started on that draw has its clock running
    /// while the window is still invisible; by the first frame anyone sees,
    /// most of the motion is gone. So the first render only parks the slots
    /// at the cursor and asks for a frame, and the animation is started on the
    /// frame after, which comes from the frame loop of a presented window.
    armed: bool,
    /// Publishes click-away identity for exactly this view's lifetime.
    _showing: ShowingRing,
}

impl RingView {
    /// Open a view on `invocation`, reporting interactions through `commands`.
    pub(crate) fn new(
        invocation: ActionRingInvocation,
        commands: mpsc::UnboundedSender<OverlayCommand>,
        live: &Arc<ClickAwaySession>,
    ) -> Self {
        let showing = live.showing(invocation.session_id);
        Self {
            invocation,
            commands,
            hovered: None,
            armed: false,
            _showing: showing,
        }
    }

    /// The ring session this view is showing.
    pub(crate) const fn session_id(&self) -> u64 {
        self.invocation.session_id
    }

    /// Report this ring cancelled. The window is closed by the caller, which
    /// is the only one holding the handle.
    pub(crate) fn cancel(&self) {
        let _ = self.commands.send(OverlayCommand::Cancel {
            session_id: self.invocation.session_id,
        });
    }

    fn slot_element(
        &self,
        slot: ActionRingSlot,
        armed: bool,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let presentation = self.invocation.slots.get(&slot)?;
        let icon_path = presentation.icon.asset_path();
        let selected = self.hovered == Some(slot);
        let home = slot.placement(WINDOW_SIZE, RADIUS, SLOT_SIZE);
        let session_id = self.invocation.session_id;
        let activate = self.commands.clone();
        let element = div()
            .id(("ring-slot", slot.index()))
            .absolute()
            .size(px(SLOT_SIZE))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(if selected {
                color::accent_at_lightness(SELECTED_FILL_L)
            } else {
                SLOT_RESTING
            })
            .when(selected, |slot| {
                slot.border_2()
                    .border_color(color::accent_at_lightness(SELECTED_BORDER_L))
            })
            .shadow_md()
            .text_color(GLYPH)
            .cursor_pointer()
            .child(svg().path(icon_path).size(px(SLOT_GLYPH)).text_color(GLYPH))
            .on_hover(cx.listener(move |this, hovered, _, cx| {
                if *hovered && this.hovered != Some(slot) {
                    this.hovered = Some(slot);
                    let _ = this
                        .commands
                        .send(OverlayCommand::Hover { session_id, slot });
                    cx.notify();
                } else if !*hovered && this.hovered == Some(slot) {
                    this.hovered = None;
                    cx.notify();
                }
            }))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                let _ = activate.send(OverlayCommand::Activate { session_id, slot });
                window.remove_window();
            });
        // Position is owned by the fly-out: the slot starts under the cursor
        // and flies to `home`, fading in on the way. Before the view is armed
        // it just waits at the start. Reduce Motion renders the end state
        // directly (GPUI's contract for one-shot animations), so the ring
        // simply appears in place.
        let fly_out = move |slot: gpui::Stateful<gpui::Div>, progress: f32| {
            let (left, top) = fly_out_position(home, progress);
            slot.left(px(left)).top(px(top)).opacity(progress)
        };
        Some(if armed {
            element
                .with_animation(
                    ("ring-slot-fly-out", slot.index()),
                    Animation::new(FLY_OUT).with_easing(ease_out_cubic),
                    fly_out,
                )
                .into_any_element()
        } else {
            fly_out(element, 0.0).into_any_element()
        })
    }
}

/// Where a slot sits `progress` (0..=1) of the way from the ring's centre to
/// its `home` placement. Positions are the slot's top-left corner, as GPUI lays
/// it out.
fn fly_out_position(home: (f32, f32), progress: f32) -> (f32, f32) {
    let centre = WINDOW_SIZE / 2.0 - SLOT_SIZE / 2.0;
    (
        centre + (home.0 - centre) * progress,
        centre + (home.1 - centre) * progress,
    )
}

impl Render for RingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let armed = std::mem::replace(&mut self.armed, true);
        if !armed {
            window.request_animation_frame();
        }
        let session_id = self.invocation.session_id;
        let root_commands = self.commands.clone();
        let center_commands = self.commands.clone();
        let hovered_label = self.hovered.and_then(|slot| {
            let presentation = self.invocation.slots.get(&slot)?;
            // User-authored labels render verbatim: passing them through the
            // localization table would translate any label that happens to
            // collide with a known key ("Copy" → "Copier" under fr).
            let label = if presentation.literal {
                presentation.label.clone()
            } else if let Some(key) = Action::translation_key_for_label(&presentation.label) {
                rust_i18n::t!(key).into_owned()
            } else {
                presentation.label.clone()
            };
            Some(SharedString::from(label))
        });
        let slots = ActionRingSlot::ALL
            .into_iter()
            .filter_map(|slot| self.slot_element(slot, armed, cx))
            .collect::<Vec<_>>();

        // The cancel button is not animated: it marks the cursor the ring was
        // raised at, and it is there on the first frame so the press reads as
        // acknowledged before the slots have finished arriving.
        div()
            .id("ring-root")
            .relative()
            .size_full()
            .children(slots)
            .child(
                div()
                    .id("ring-cancel")
                    .absolute()
                    .left(px(WINDOW_SIZE / 2.0 - CANCEL_SIZE / 2.0))
                    .top(px(WINDOW_SIZE / 2.0 - CANCEL_SIZE / 2.0))
                    .size(px(CANCEL_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(CANCEL_RESTING)
                    .shadow_md()
                    .text_color(CANCEL_GLYPH)
                    .cursor_pointer()
                    .child(
                        svg()
                            .path(RING_CANCEL_ICON)
                            .size(px(CANCEL_GLYPH_SIZE))
                            .flex_none(),
                    )
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let _ = center_commands.send(OverlayCommand::Cancel { session_id });
                        window.remove_window();
                    }),
            )
            .when_some(hovered_label, |ring, label| {
                // With the desktop showing through the ring, the label brings
                // its own dark pill so it stays legible over anything. It sits
                // under the ring: the centre is too tight for text now, and
                // below the bottom slot it never covers a target.
                ring.child(
                    div()
                        .absolute()
                        .left(px(WINDOW_SIZE / 2.0 - 80.0))
                        .top(px(WINDOW_SIZE / 2.0 + RADIUS + SLOT_SIZE / 2.0 + 6.0))
                        .w(px(160.0))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded_full()
                                .bg(LABEL_PILL)
                                .shadow_md()
                                .text_center()
                                .text_sm()
                                .text_color(LABEL)
                                .child(label),
                        ),
                )
            })
            .on_click(move |_, window, _| {
                let _ = root_commands.send(OverlayCommand::Cancel { session_id });
                window.remove_window();
            })
    }
}

/// Appearance shared by the platform-specific placement paths.
pub(crate) fn ring_window_options() -> WindowOptions {
    WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("openlogi-action-ring".to_string()),
        ..WindowOptions::default()
    }
}

#[cfg(any(not(target_os = "windows"), test))]
pub(crate) fn clamp_window_origin(
    desired: Point<Pixels>,
    window_size: Size<Pixels>,
    display: Bounds<Pixels>,
) -> Point<Pixels> {
    let max = point(
        display.right() - window_size.width,
        display.bottom() - window_size.height,
    );
    desired.clamp(&display.origin, &max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_fly_out_from_the_centre_to_their_placement() {
        let home = ActionRingSlot::Right.placement(WINDOW_SIZE, RADIUS, SLOT_SIZE);
        let centre = WINDOW_SIZE / 2.0 - SLOT_SIZE / 2.0;
        assert_eq!(fly_out_position(home, 0.0), (centre, centre));
        assert_eq!(fly_out_position(home, 1.0), home);
        let (mid_x, mid_y) = fly_out_position(home, 0.5);
        assert!((mid_x - (centre + RADIUS / 2.0)).abs() < 1e-3);
        assert!((mid_y - centre).abs() < 1e-3);
    }

    #[test]
    fn the_fly_out_curve_is_front_loaded_and_lands_exactly() {
        assert!((ease_out_cubic(0.0)).abs() < 1e-6);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);
        assert!(ease_out_cubic(0.5) > 0.85);
    }

    #[test]
    fn overlay_origin_is_clamped_to_the_display() {
        let display = Bounds::new(point(px(100.0), px(50.0)), Size::new(px(800.0), px(600.0)));
        let size = Size::new(px(400.0), px(400.0));
        assert_eq!(
            clamp_window_origin(point(px(-50.0), px(-50.0)), size, display),
            point(px(100.0), px(50.0))
        );
        assert_eq!(
            clamp_window_origin(point(px(700.0), px(500.0)), size, display),
            point(px(500.0), px(250.0))
        );
    }

    #[test]
    fn overlay_origin_stays_cursor_centered_away_from_edges() {
        let display = Bounds::new(Point::default(), Size::new(px(1600.0), px(1000.0)));
        let desired = point(px(600.0), px(300.0));
        assert_eq!(
            clamp_window_origin(desired, Size::new(px(400.0), px(400.0)), display),
            desired
        );
    }
}
