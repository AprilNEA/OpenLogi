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
    StatefulInteractiveElement as _, Styled, Subscription, Window, WindowBackgroundAppearance,
    WindowId, WindowKind, WindowOptions, div, point, prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::{Action, ActionRingSlot};
use openlogi_ipc::ActionRingInvocation;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::ipc::OverlayCommand;
use crate::session::{self, ClickAwaySession, ShowingRing};

/// Ring geometry. The radius is the mouse travel to any slot, so it is kept
/// as tight as the slots allow: eight 48 pt slots on an 84 pt radius leave a
/// 16 pt gap between neighbours, which is enough for the desktop to read
/// through and for a slot's hover edge to be unambiguous. The window is the
/// ring plus room for the hover label under it and the slot shadows.
pub(crate) const WINDOW_SIZE: f32 = 300.0;
pub(crate) const SLOT_SIZE: f32 = 48.0;
pub(crate) const RADIUS: f32 = 84.0;
/// Identifies the ring's windows to the compositor, whatever kind they are.
pub(crate) const RING_APP_ID: &str = "openlogi-action-ring";
const CANCEL_SIZE: f32 = 36.0;
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
                session::dismiss_from_window(cx, window, session_id);
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
        // The slots' fly-out runs on the wall clock. This test is about which
        // window draws the panel, so it takes the end state Reduce Motion
        // renders: the slots are home on the first frame after the panel's.
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

        above_cx.simulate_mouse_move(top_slot_at(center), None, Modifiers::default());
        assert_eq!(
            above.read_with(cx, |view, _| view.hovered).unwrap(),
            Some(ActionRingSlot::Top),
            "the window on top draws a working panel"
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
