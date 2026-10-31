//! The ring itself: the GPUI view, and the window it is drawn in.
//!
//! Where a client can place its own windows, the window is the ring: a
//! square panel centred on the cursor and clamped to its display. Wayland
//! allows neither half of that, so there each display gets a window covering
//! it and the same panel is drawn at the pointer, in the one window that heard
//! it last.
//!
//! The ring is eight free-floating slots around a cancel button, with the
//! desktop showing through between them; there is no backing disc. On open the
//! slots fly out from the cursor to their places on the ring, so the motion
//! itself says where the ring came from.

use gpui::{
    Animation, AnimationExt as _, Bounds, Context, Div, Entity, Hsla, InteractiveElement,
    IntoElement, MouseMoveEvent, ParentElement, Pixels, Point, Render, SharedString, Size,
    StatefulInteractiveElement as _, Styled, Subscription, Task, Window,
    WindowBackgroundAppearance, WindowId, WindowKind, WindowOptions, div, point,
    prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::action_ring::geometry::{CANCEL_DIAMETER, RADIUS, SLOT_DIAMETER};
use openlogi_core::binding::{Action, ActionRingSlot};
use openlogi_ipc::ActionRingInvocation;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::ipc::OverlayCommand;
use crate::session::{self, ClickAwaySession, ShowingRing};

/// The window is the ring (`openlogi_core::action_ring::geometry`) plus room
/// for the hover label under it and the slot shadows.
pub(crate) const WINDOW_SIZE: f32 = 300.0;
pub(crate) const SLOT_SIZE: f32 = SLOT_DIAMETER;
/// Identifies the ring's windows to the compositor, whatever kind they are.
pub(crate) const RING_APP_ID: &str = "openlogi-action-ring";
const CANCEL_SIZE: f32 = CANCEL_DIAMETER;
const SLOT_GLYPH: f32 = 20.0;
const CANCEL_GLYPH_SIZE: f32 = 16.0;
/// Widest the hover label's pill may grow. Nothing sits beside it under the
/// ring, so it may run wider than the ring's slots; longer text is ellipsized.
const LABEL_WIDTH: f32 = 240.0;

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

/// Where the ring's panel sits inside the window it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// The window was opened at the cursor, so the panel fills it.
    Window,
    /// The window covers a display, as each of the ring's windows does. The
    /// panel is drawn where the ring's [`RingPointer`] says, if it names this window.
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "only Wayland covers a display")
    )]
    AtPointer,
}

/// Which of a ring's windows draws its panel, and where.
///
/// Wayland gives a ring one window per display, and the compositor decides
/// where each of them goes: GNOME stacks them all on one display (#1901). Each
/// window that heard the pointer would then draw a panel of its own, so the
/// ring's windows share this one record and only the last to hear the pointer
/// draws. That is the window on top, the only one a click can reach.
#[derive(Debug, Default)]
pub(crate) struct RingPointer {
    heard: Option<(WindowId, Point<Pixels>)>,
}

impl RingPointer {
    /// Record that `window` heard the pointer at `position`, reporting whether
    /// the panel moved.
    ///
    /// A window's first report places the panel: it is the move synthesized
    /// from pointer-enter, which says where the ring was opened. Later reports
    /// while that window keeps the panel do not move it.
    pub(crate) fn hear(&mut self, window: WindowId, position: Point<Pixels>) -> bool {
        if self.heard.is_some_and(|(drawing, _)| drawing == window) {
            return false;
        }
        self.heard = Some((window, position));
        true
    }

    /// Where `window` centres the panel: nowhere unless it heard the pointer last.
    pub(crate) fn center_in(&self, window: WindowId) -> Option<Point<Pixels>> {
        self.heard
            .filter(|&(drawing, _)| drawing == window)
            .map(|(_, center)| center)
    }
}

pub(crate) struct RingView {
    invocation: ActionRingInvocation,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    hovered: Option<ActionRingSlot>,
    placement: Placement,
    /// Shared by every window of this ring.
    pointer: Entity<RingPointer>,
    /// Redraws this window when the panel moves, whichever window took it.
    _pointer_changes: Subscription,
    /// Whether the fly-out has been started. GPUI draws a new window once,
    /// synchronously, inside `open_window` — before the platform has put it
    /// on screen — and an animation started on that draw has its clock running
    /// while the window is still invisible; by the first frame anyone sees,
    /// most of the motion is gone. So the first render only parks the slots
    /// at the cursor and asks for a frame, and the animation is started on the
    /// frame after, which comes from the frame loop of a presented window.
    armed: bool,
    /// Whether the fly-out has landed, so the slots answer the cursor. Until
    /// then they are on their way out from under it, and the topmost one would
    /// read as hovered — a spurious haptic on every open — so slot hover and
    /// click are held back. Set by `landing`.
    landed: bool,
    /// Lands the fly-out one [`FLY_OUT`] after the render that starts it, or
    /// at once under Reduce Motion, which draws the slots in place. Dropped,
    /// and so cancelled, if this window stops drawing the panel first.
    landing: Option<Task<()>>,
    /// The slot under the pointer, landed or not. GPUI reports a hover once,
    /// when it starts, so one that starts in flight is kept here and becomes
    /// `hovered` on landing.
    pointer_over: Option<ActionRingSlot>,
    /// Publishes click-away identity for exactly this view's lifetime.
    _showing: ShowingRing,
}

impl RingView {
    /// Open a view on `invocation` in `window`, reporting interactions through
    /// `commands`. `pointer` is shared by all of the ring's windows.
    pub(crate) fn new(
        invocation: ActionRingInvocation,
        commands: mpsc::UnboundedSender<OverlayCommand>,
        live: &Arc<ClickAwaySession>,
        placement: Placement,
        pointer: Entity<RingPointer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let showing = live.showing(invocation.session_id);
        let pointer_changes = cx.observe_in(&pointer, window, |this, pointer, window, cx| {
            // A window that stopped drawing the panel has no slot under the pointer.
            if pointer
                .read(cx)
                .center_in(window.window_handle().window_id())
                .is_none()
            {
                this.hovered = None;
                // The slots' animation state goes with the panel, so the next
                // time this window draws it they fly out afresh, and must
                // land again before they answer the cursor.
                this.landed = false;
                this.landing = None;
                this.pointer_over = None;
            }
            cx.notify();
        });
        Self {
            invocation,
            commands,
            hovered: None,
            placement,
            pointer,
            _pointer_changes: pointer_changes,
            armed: false,
            landed: false,
            landing: None,
            pointer_over: None,
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

    /// Track the slot under the pointer, and hover it if the slots have landed.
    fn slot_hovered(&mut self, slot: ActionRingSlot, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            self.pointer_over = Some(slot);
        } else if self.pointer_over == Some(slot) {
            self.pointer_over = None;
        }
        self.sync_hover(cx);
    }

    /// The fly-out has landed: hover whatever slot the pointer is resting on.
    fn land(&mut self, cx: &mut Context<Self>) {
        self.landed = true;
        self.landing = None;
        self.sync_hover(cx);
    }

    /// Once the slots have landed, hover the slot under the pointer,
    /// reporting it to the agent if it is newly hovered.
    fn sync_hover(&mut self, cx: &mut Context<Self>) {
        if !self.landed || self.hovered == self.pointer_over {
            return;
        }
        self.hovered = self.pointer_over;
        if let Some(slot) = self.hovered {
            let _ = self.commands.send(OverlayCommand::Hover {
                session_id: self.invocation.session_id,
                slot,
            });
        }
        cx.notify();
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
                this.slot_hovered(slot, *hovered, cx);
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if !this.landed {
                    return;
                }
                let _ = this
                    .commands
                    .send(OverlayCommand::Activate { session_id, slot });
                session::dismiss_from_window(cx, window, session_id);
            }));
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

impl RingView {
    /// The fixed ring. Both placements anchor this same element. `armed` is
    /// whether the slots fly out on this render or wait under the cursor.
    fn panel(&self, armed: bool, cx: &mut Context<Self>) -> Div {
        let session_id = self.invocation.session_id;
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
            .relative()
            .size(px(WINDOW_SIZE))
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
                        session::dismiss_from_window(cx, window, session_id);
                    }),
            )
            .when_some(hovered_label, |ring, label| {
                // With the desktop showing through the ring, the label brings
                // its own dark pill so it stays legible over anything. It sits
                // under the ring: the centre is too tight for text now, and
                // below the bottom slot it never covers a target. Only one
                // line fits between the bottom slot and the window's edge, so
                // a long label is ellipsized rather than wrapped and clipped.
                ring.child(
                    div()
                        .absolute()
                        .left(px((WINDOW_SIZE - LABEL_WIDTH) / 2.0))
                        .top(px(WINDOW_SIZE / 2.0 + RADIUS + SLOT_SIZE / 2.0 + 6.0))
                        .w(px(LABEL_WIDTH))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .max_w_full()
                                .px_3()
                                .py_1()
                                .rounded_full()
                                .bg(LABEL_PILL)
                                .shadow_md()
                                .text_center()
                                .text_sm()
                                .text_color(LABEL)
                                .truncate()
                                .child(label),
                        ),
                )
            })
    }
}

impl Render for RingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_id = self.invocation.session_id;
        let root_commands = self.commands.clone();
        let placement = self.placement;
        let center = self
            .pointer
            .read(cx)
            .center_in(window.window_handle().window_id());
        let panel = panel_origin(placement, center, window.viewport_size()).map(|origin| {
            // The first render that draws the panel only parks the slots and
            // asks for a frame; see `armed`.
            let armed = std::mem::replace(&mut self.armed, true);
            if !armed {
                window.request_animation_frame();
            } else if !self.landed && self.landing.is_none() {
                // This render starts the fly-out. Reduce Motion draws the
                // slots in place, so the ring must not look ready while
                // ignoring the cursor.
                let flight = if cx.reduce_motion() {
                    Duration::ZERO
                } else {
                    FLY_OUT
                };
                let flight = cx.background_executor().timer(flight);
                self.landing = Some(cx.spawn_in(window, async move |this, cx| {
                    flight.await;
                    let _ = this.update(cx, RingView::land);
                }));
            }
            div()
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .child(self.panel(armed, cx))
        });

        div()
            .id("ring-root")
            .relative()
            .size_full()
            .when(matches!(placement, Placement::AtPointer), |root| {
                root.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    let heard_in = window.window_handle().window_id();
                    this.pointer.update(cx, |pointer, cx| {
                        if pointer.hear(heard_in, event.position) {
                            cx.notify();
                        }
                    });
                }))
            })
            .children(panel)
            .on_click(move |_, window, cx| {
                let _ = root_commands.send(OverlayCommand::Cancel { session_id });
                session::dismiss_from_window(cx, window, session_id);
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
        app_id: Some(RING_APP_ID.to_string()),
        ..WindowOptions::default()
    }
}

/// Top-left of the ring panel within its window, or nothing while this window
/// does not draw it.
///
/// `center` is where this window heard the pointer, if it draws the panel.
/// `viewport` is the pointer's space. Display bounds are logical pixels and
/// disagree with it under fractional scaling, so the clamp cannot use them.
fn panel_origin(
    placement: Placement,
    center: Option<Point<Pixels>>,
    viewport: Size<Pixels>,
) -> Option<Point<Pixels>> {
    let center = match placement {
        Placement::Window => return Some(Point::default()),
        Placement::AtPointer => center?,
    };
    let size = Size::new(px(WINDOW_SIZE), px(WINDOW_SIZE));
    Some(clamp_window_origin(
        point(center.x - size.width / 2.0, center.y - size.height / 2.0),
        size,
        Bounds::new(Point::default(), viewport),
    ))
}

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
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{AppContext as _, Modifiers, TestAppContext, VisualTestContext, WindowHandle};
    use openlogi_core::binding::ActionRingIcon;
    use openlogi_ipc::ActionRingPresentation;

    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;

    fn window(id: u64) -> WindowId {
        WindowId::from(id)
    }

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
    fn a_window_sized_ring_fills_its_window() {
        assert_eq!(
            panel_origin(
                Placement::Window,
                None,
                Size::new(px(WINDOW_SIZE), px(WINDOW_SIZE))
            ),
            Some(Point::default())
        );
    }

    #[test]
    fn a_display_sized_ring_draws_nothing_until_the_pointer_is_known() {
        assert_eq!(
            panel_origin(
                Placement::AtPointer,
                None,
                Size::new(px(1920.0), px(1080.0))
            ),
            None
        );
    }

    #[test]
    fn a_display_sized_ring_centers_on_the_pointer() {
        assert_eq!(
            panel_origin(
                Placement::AtPointer,
                Some(point(px(900.0), px(500.0))),
                Size::new(px(1920.0), px(1080.0))
            ),
            Some(point(
                px(900.0 - WINDOW_SIZE / 2.0),
                px(500.0 - WINDOW_SIZE / 2.0)
            ))
        );
    }

    #[test]
    fn a_display_sized_ring_stays_whole_against_an_edge() {
        assert_eq!(
            panel_origin(
                Placement::AtPointer,
                Some(point(px(4.0), px(1076.0))),
                Size::new(px(1920.0), px(1080.0))
            ),
            Some(point(px(0.0), px(1080.0 - WINDOW_SIZE)))
        );
    }

    #[test]
    fn no_window_draws_the_panel_before_one_hears_the_pointer() {
        assert_eq!(RingPointer::default().center_in(window(1)), None);
    }

    #[test]
    fn only_the_window_that_heard_the_pointer_draws_the_panel() {
        let mut pointer = RingPointer::default();
        assert!(pointer.hear(window(1), point(px(900.0), px(500.0))));
        assert_eq!(
            pointer.center_in(window(1)),
            Some(point(px(900.0), px(500.0)))
        );
        assert_eq!(pointer.center_in(window(2)), None);
    }

    #[test]
    fn the_panel_stays_where_its_window_first_heard_the_pointer() {
        let mut pointer = RingPointer::default();
        pointer.hear(window(1), point(px(900.0), px(500.0)));
        assert!(
            !pointer.hear(window(1), point(px(960.0), px(540.0))),
            "a later move in the drawing window does not move the panel"
        );
        assert_eq!(
            pointer.center_in(window(1)),
            Some(point(px(900.0), px(500.0)))
        );
    }

    #[test]
    fn the_window_that_hears_the_pointer_last_takes_the_panel() {
        // GNOME stacks the ring's windows on one display, and the one mapped
        // on top hears the pointer after the one below it (#1901).
        let (below, above) = (window(1), window(2));
        let mut pointer = RingPointer::default();
        pointer.hear(below, point(px(300.0), px(200.0)));
        assert!(pointer.hear(above, point(px(900.0), px(500.0))));
        assert_eq!(
            pointer.center_in(below),
            None,
            "the covered window stops drawing"
        );
        assert_eq!(pointer.center_in(above), Some(point(px(900.0), px(500.0))));
        assert!(
            pointer.hear(below, point(px(10.0), px(20.0))),
            "a window takes the panel back when it hears the pointer again"
        );
        assert_eq!(pointer.center_in(above), None);
        assert_eq!(pointer.center_in(below), Some(point(px(10.0), px(20.0))));
    }

    fn invocation() -> ActionRingInvocation {
        ActionRingInvocation {
            session_id: 7,
            slots: [(
                ActionRingSlot::Top,
                ActionRingPresentation {
                    label: "Copy".to_string(),
                    literal: true,
                    icon: ActionRingIcon::Copy,
                },
            )]
            .into(),
            language: None,
        }
    }

    /// Where the `Top` slot's centre lands when the panel centres on `center`
    /// well away from every edge.
    fn top_slot_at(center: Point<Pixels>) -> Point<Pixels> {
        let (left, top) = ActionRingSlot::Top.placement(WINDOW_SIZE, RADIUS, SLOT_SIZE);
        point(
            center.x - px(WINDOW_SIZE / 2.0) + px(left + SLOT_SIZE / 2.0),
            center.y - px(WINDOW_SIZE / 2.0) + px(top + SLOT_SIZE / 2.0),
        )
    }

    #[gpui::test]
    fn a_ring_window_that_loses_the_pointer_stops_drawing_and_hovering(cx: &mut TestAppContext) {
        // The slots' fly-out runs on the wall clock, and test windows have no
        // frame loop. This test is about which window draws the panel, so it
        // takes the end state Reduce Motion renders and draws the frame a
        // window asks for once it has the panel; its slots are then home.
        cx.update(|cx| cx.set_reduce_motion(true));
        let live = Arc::new(ClickAwaySession::new());
        let (commands, mut sent) = mpsc::unbounded_channel();
        let pointer = cx.new(|_| RingPointer::default());
        let open = |cx: &mut TestAppContext| -> WindowHandle<RingView> {
            let (commands, pointer, live) = (commands.clone(), pointer.clone(), Arc::clone(&live));
            cx.add_window(move |window, cx| {
                RingView::new(
                    invocation(),
                    commands,
                    &live,
                    Placement::AtPointer,
                    pointer,
                    window,
                    cx,
                )
            })
        };
        let below = open(cx);
        let above = open(cx);
        let center = point(px(600.0), px(500.0));
        let below_view = below.update(cx, |_, _, cx| cx.entity()).unwrap();
        let below_redraws = Rc::new(Cell::new(0_u32));
        cx.update(|cx| {
            let redraws = Rc::clone(&below_redraws);
            cx.observe(&below_view, move |_, _| redraws.set(redraws.get() + 1))
                .detach();
        });

        let mut below_cx = VisualTestContext::from_window(below.into(), cx);
        below_cx.simulate_mouse_move(center, None, Modifiers::default());
        start_fly_out(&mut below_cx, below);
        below_cx.simulate_mouse_move(top_slot_at(center), None, Modifiers::default());
        assert_eq!(
            below.read_with(cx, |view, _| view.hovered).unwrap(),
            Some(ActionRingSlot::Top),
            "the window that heard the pointer draws the panel"
        );
        assert!(matches!(
            sent.try_recv(),
            Ok(OverlayCommand::Hover {
                session_id: 7,
                slot: ActionRingSlot::Top
            })
        ));

        let mut above_cx = VisualTestContext::from_window(above.into(), cx);
        let redraws_before = below_redraws.get();
        above_cx.simulate_mouse_move(center, None, Modifiers::default());
        let below_id = below.window_id();
        let above_id = above.window_id();
        assert_eq!(
            pointer.read_with(cx, |pointer, _| pointer.center_in(below_id)),
            None
        );
        assert_eq!(
            pointer.read_with(cx, |pointer, _| pointer.center_in(above_id)),
            Some(center)
        );
        assert_eq!(
            below.read_with(cx, |view, _| view.hovered).unwrap(),
            None,
            "the covered window no longer has a slot under the pointer"
        );
        assert!(
            below_redraws.get() > redraws_before,
            "the covered window redraws without its panel"
        );

        start_fly_out(&mut above_cx, above);
        above_cx.simulate_mouse_move(top_slot_at(center), None, Modifiers::default());
        assert_eq!(
            above.read_with(cx, |view, _| view.hovered).unwrap(),
            Some(ActionRingSlot::Top),
            "the window on top draws a working panel"
        );
    }

    /// Open one of a ring's windows, drawing the panel at `placement`.
    fn open_ring(
        cx: &mut TestAppContext,
        commands: &mpsc::UnboundedSender<OverlayCommand>,
        live: &Arc<ClickAwaySession>,
        placement: Placement,
        pointer: &Entity<RingPointer>,
    ) -> WindowHandle<RingView> {
        let (commands, live, pointer) = (commands.clone(), Arc::clone(live), pointer.clone());
        cx.add_window(move |window, cx| {
            RingView::new(
                invocation(),
                commands,
                &live,
                placement,
                pointer,
                window,
                cx,
            )
        })
    }

    /// Draw `ring` until its fly-out has started: the first frame that draws
    /// the panel parks the slots, and the next one sends them out.
    fn start_fly_out(cx: &mut TestAppContext, ring: WindowHandle<RingView>) {
        for _ in 0..2 {
            ring.update(cx, |_, window, _| window.refresh()).unwrap();
            cx.run_until_parked();
        }
        assert!(
            ring.read_with(cx, |view, _| view.landing.is_some() || view.landed)
                .unwrap(),
            "the fly-out has started"
        );
    }

    fn hovered(cx: &mut TestAppContext, ring: WindowHandle<RingView>) -> Option<ActionRingSlot> {
        ring.read_with(cx, |view, _| view.hovered).unwrap()
    }

    #[gpui::test]
    fn a_slot_entered_in_flight_is_hovered_once_the_slots_land(cx: &mut TestAppContext) {
        let live = Arc::new(ClickAwaySession::new());
        let (commands, mut sent) = mpsc::unbounded_channel();
        let pointer = cx.new(|_| RingPointer::default());
        let ring = open_ring(cx, &commands, &live, Placement::Window, &pointer);
        start_fly_out(cx, ring);

        // GPUI reports the hover once, while the slot is still on its way.
        ring.update(cx, |view, _, cx| {
            view.slot_hovered(ActionRingSlot::Top, true, cx);
        })
        .unwrap();
        assert_eq!(hovered(cx, ring), None, "not before the slots land");
        assert!(matches!(sent.try_recv(), Err(TryRecvError::Empty)));

        cx.executor().advance_clock(FLY_OUT);
        cx.run_until_parked();
        assert_eq!(
            hovered(cx, ring),
            Some(ActionRingSlot::Top),
            "the pointer resting on the slot hovers it on landing"
        );
        assert!(matches!(
            sent.try_recv(),
            Ok(OverlayCommand::Hover {
                session_id: 7,
                slot: ActionRingSlot::Top
            })
        ));
        assert!(
            matches!(sent.try_recv(), Err(TryRecvError::Empty)),
            "reported once"
        );
    }

    #[gpui::test]
    fn under_reduce_motion_the_slots_answer_as_soon_as_they_appear(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let live = Arc::new(ClickAwaySession::new());
        let (commands, _sent) = mpsc::unbounded_channel();
        let pointer = cx.new(|_| RingPointer::default());
        let ring = open_ring(cx, &commands, &live, Placement::Window, &pointer);
        start_fly_out(cx, ring);
        assert!(
            ring.read_with(cx, |view, _| view.landed).unwrap(),
            "drawn in place, so no wait"
        );
    }

    #[gpui::test]
    fn a_window_that_takes_the_panel_back_lands_again_before_it_answers(cx: &mut TestAppContext) {
        let live = Arc::new(ClickAwaySession::new());
        let (commands, mut sent) = mpsc::unbounded_channel();
        let pointer = cx.new(|_| RingPointer::default());
        let first = open_ring(cx, &commands, &live, Placement::AtPointer, &pointer);
        let second = open_ring(cx, &commands, &live, Placement::AtPointer, &pointer);
        let center = point(px(600.0), px(500.0));
        let hear = |cx: &mut TestAppContext, ring: WindowHandle<RingView>| {
            VisualTestContext::from_window(ring.into(), cx).simulate_mouse_move(
                center,
                None,
                Modifiers::default(),
            );
        };

        hear(cx, first);
        start_fly_out(cx, first);
        cx.executor().advance_clock(FLY_OUT);
        cx.run_until_parked();
        assert!(first.read_with(cx, |view, _| view.landed).unwrap());

        // Another window takes the panel mid-session, then the first takes it
        // back: its slots fly out afresh, so they must land again.
        hear(cx, second);
        assert!(
            first
                .read_with(cx, |view, _| !view.landed && view.landing.is_none())
                .unwrap(),
            "a window that stops drawing the panel forgets its landing"
        );
        hear(cx, first);
        start_fly_out(cx, first);
        while sent.try_recv().is_ok() {}
        first
            .update(cx, |view, _, cx| {
                view.slot_hovered(ActionRingSlot::Top, true, cx);
            })
            .unwrap();
        assert_eq!(
            hovered(cx, first),
            None,
            "not while the slots fly out again"
        );
        assert!(matches!(sent.try_recv(), Err(TryRecvError::Empty)));

        cx.executor().advance_clock(FLY_OUT);
        cx.run_until_parked();
        assert_eq!(hovered(cx, first), Some(ActionRingSlot::Top));
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
