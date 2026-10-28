//! The ring itself: the GPUI view, and the window it is drawn in.
//!
//! Where a client can place its own windows, the window is the ring: a square
//! panel centred on the cursor and clamped to its display. Wayland allows
//! neither half of that, so there each display gets a window covering it and
//! the same panel is drawn at the pointer, in the one window that heard it last.
//! What the panel looks like, and how big it is, is the invocation's theme:
//! [`classic`] for `Classic`, [`skin`] for `InMotion`, [`obsidian`] for
//! `Obsidian`, [`porcelain`] for `Porcelain`. This module owns placement,
//! hover state, closing, and the window.

mod classic;
mod motion;
mod obsidian;
mod porcelain;
mod skin;

use gpui::{
    AppContext as _, Bounds, Context, Entity, InteractiveElement, IntoElement, MouseMoveEvent,
    ParentElement, Pixels, Point, Render, SharedString, Size, StatefulInteractiveElement as _,
    Styled, Subscription, Window, WindowBackgroundAppearance, WindowId, WindowKind, WindowOptions,
    div, point, prelude::FluentBuilder as _, px,
};
use openlogi_core::binding::{Action, ActionRingSlot, ActionRingTheme};
use openlogi_ipc::{ActionRingInvocation, ActionRingPresentation};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

use crate::ipc::OverlayCommand;
use crate::session::{self, ClickAwaySession, ShowingRing};

/// The ring's edge for `theme` on a display offering `available`: the theme's
/// designed size, or less when the theme can shrink and the display is
/// shorter. Every placement path and the panel itself size the ring through
/// this one decision.
///
/// Classic is always its 360 px disc, as it was before themes. The other
/// themes are their designed size, or the display's shorter side when that
/// is smaller: a 1280×720 display at 200% is 360 logical pixels tall, and a
/// 480 px ring anchored to its top would put the bottom slot off screen, so
/// those skins scale their layout to whatever edge this returns.
pub(crate) fn panel_edge(theme: ActionRingTheme, available: Size<Pixels>) -> Pixels {
    match theme {
        ActionRingTheme::Classic => px(classic::PANEL_SIZE),
        ActionRingTheme::InMotion | ActionRingTheme::Obsidian | ActionRingTheme::Porcelain => {
            px(design_edge(theme)).min(available.width.min(available.height))
        }
    }
}

/// The ring's designed edge for `theme`, where no display is known yet.
pub(crate) const fn design_edge(theme: ActionRingTheme) -> f32 {
    match theme {
        ActionRingTheme::Classic => classic::PANEL_SIZE,
        ActionRingTheme::InMotion => skin::DESIGN_EDGE,
        ActionRingTheme::Obsidian => obsidian::DESIGN_EDGE,
        ActionRingTheme::Porcelain => porcelain::DESIGN_EDGE,
    }
}
/// Identifies the ring's windows to the compositor, whatever kind they are.
pub(crate) const RING_APP_ID: &str = "openlogi-action-ring";

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

/// How a ring looks, with the state that look needs: nothing for the classic
/// disc, the motion clocks for the animated themes. A classic ring cannot be
/// mid-exit, so the type says so instead of a flag that is always off.
enum RingLook {
    Classic,
    InMotion(skin::Motion),
    Obsidian(obsidian::Motion),
    Porcelain(porcelain::Motion),
}

impl From<ActionRingTheme> for RingLook {
    fn from(theme: ActionRingTheme) -> Self {
        match theme {
            ActionRingTheme::Classic => Self::Classic,
            ActionRingTheme::InMotion => Self::InMotion(skin::Motion::default()),
            ActionRingTheme::Obsidian => Self::Obsidian(obsidian::Motion::default()),
            ActionRingTheme::Porcelain => Self::Porcelain(porcelain::Motion::default()),
        }
    }
}

impl RingLook {
    const fn theme(&self) -> ActionRingTheme {
        match self {
            Self::Classic => ActionRingTheme::Classic,
            Self::InMotion(_) => ActionRingTheme::InMotion,
            Self::Obsidian(_) => ActionRingTheme::Obsidian,
            Self::Porcelain(_) => ActionRingTheme::Porcelain,
        }
    }

    /// Whether the exit animation is already playing.
    const fn is_closing(&self) -> bool {
        match self {
            Self::Classic => false,
            Self::InMotion(motion) => motion.is_closing(),
            Self::Obsidian(motion) => motion.is_closing(),
            Self::Porcelain(motion) => motion.is_closing(),
        }
    }

    /// Start the exit of a look that has one: `None` for a look that goes at
    /// once, `Some(false)` when the exit is already playing.
    fn begin_close(&mut self, now: Instant) -> Option<bool> {
        match self {
            Self::Classic => None,
            Self::InMotion(motion) => Some(motion.begin_close(now)),
            Self::Obsidian(motion) => Some(motion.begin_close(now)),
            Self::Porcelain(motion) => Some(motion.begin_close(now)),
        }
    }
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
    /// Publishes click-away identity for exactly this view's lifetime.
    _showing: ShowingRing,
    look: RingLook,
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
        let look = RingLook::from(invocation.theme);
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
            _showing: showing,
            look,
        }
    }

    /// Whether the exit animation is already playing.
    pub(crate) const fn is_closing(&self) -> bool {
        self.look.is_closing()
    }

    /// Send the ring's final command once and close every window of the
    /// session. An animated ring with motion on plays its exit first and
    /// closes itself when that ends; the others go at once, so no invisible
    /// window stays to swallow a click.
    fn close(&mut self, command: OverlayCommand, window: &mut Window, cx: &mut Context<Self>) {
        let session_id = self.invocation.session_id;
        let exit = if cx.reduce_motion() {
            None
        } else {
            self.look.begin_close(Instant::now())
        };
        match exit {
            Some(false) => {}
            Some(true) => {
                let _ = self.commands.send(command);
                session::close_ring_windows(cx, session_id);
                cx.notify();
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(motion::EXIT).await;
                    let _ = this.update(cx, |_, cx| {
                        let id = cx.entity_id();
                        cx.with_window(id, |window, cx| {
                            session::dismiss_from_window(cx, window, session_id);
                        });
                    });
                })
                .detach();
            }
            None => {
                let _ = self.commands.send(command);
                session::dismiss_from_window(cx, window, session_id);
            }
        }
    }

    /// Record the slot under the pointer, telling the agent when it changes.
    /// A ring that is closing keeps the slot it had.
    fn hover(&mut self, slot: ActionRingSlot, hovered: bool, cx: &mut Context<Self>) {
        if self.is_closing() {
            return;
        }
        if hovered && self.hovered != Some(slot) {
            self.hovered = Some(slot);
            let _ = self.commands.send(OverlayCommand::Hover {
                session_id: self.invocation.session_id,
                slot,
            });
            cx.notify();
        } else if !hovered && self.hovered == Some(slot) {
            self.hovered = None;
            cx.notify();
        }
    }

    /// The hovered slot's caption, if any slot is hovered.
    fn hovered_caption(&self) -> Option<SharedString> {
        self.hovered
            .and_then(|slot| self.invocation.slots.get(&slot))
            .map(slot_caption)
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
}

impl Render for RingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_id = self.invocation.session_id;
        let placement = self.placement;
        let center = self
            .pointer
            .read(cx)
            .center_in(window.window_handle().window_id());
        let viewport = window.viewport_size();
        let theme = self.look.theme();
        let edge = panel_edge(theme, viewport);
        let panel = panel_origin(theme, placement, center, viewport).map(|origin| {
            div()
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .child(match self.look {
                    RingLook::Classic => self.classic_panel(cx).into_any_element(),
                    RingLook::InMotion(_) => self.skinned_panel(edge, cx).into_any_element(),
                    RingLook::Obsidian(_) => self
                        .obsidian_panel(edge, origin, window, cx)
                        .into_any_element(),
                    RingLook::Porcelain(_) => self.porcelain_panel(edge, cx).into_any_element(),
                })
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
            .on_click(cx.listener(move |this, _, window, cx| {
                this.close(OverlayCommand::Cancel { session_id }, window, cx);
            }))
    }
}

/// The caption a slot shows for `presentation`.
///
/// User-authored labels render verbatim: passing them through the
/// localization table would translate any label that happens to collide
/// with a known key ("Copy" would become "Copier" under fr).
fn slot_caption(presentation: &ActionRingPresentation) -> SharedString {
    let label = if presentation.literal {
        presentation.label.clone()
    } else if let Some(key) = Action::translation_key_for_label(&presentation.label) {
        rust_i18n::t!(key).into_owned()
    } else {
        presentation.label.clone()
    };
    SharedString::from(label)
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
    theme: ActionRingTheme,
    placement: Placement,
    center: Option<Point<Pixels>>,
    viewport: Size<Pixels>,
) -> Option<Point<Pixels>> {
    let center = match placement {
        Placement::Window => return Some(Point::default()),
        Placement::AtPointer => center?,
    };
    let edge = panel_edge(theme, viewport);
    let size = Size::new(edge, edge);
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

    use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
    use openlogi_core::binding::ActionRingIcon;
    use openlogi_ipc::ActionRingPresentation;

    use super::*;

    fn window(id: u64) -> WindowId {
        WindowId::from(id)
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
        for theme in ActionRingTheme::ALL {
            let edge = px(design_edge(theme));
            assert_eq!(
                panel_origin(theme, Placement::Window, None, Size::new(edge, edge)),
                Some(Point::default())
            );
        }
    }

    #[test]
    fn each_theme_sizes_the_ring_its_own_way() {
        let wide = Size::new(px(1920.0), px(1080.0));
        assert_eq!(panel_edge(ActionRingTheme::Classic, wide), px(360.0));
        assert_eq!(panel_edge(ActionRingTheme::InMotion, wide), px(480.0));
        assert_eq!(panel_edge(ActionRingTheme::Obsidian, wide), px(440.0));
        assert_eq!(panel_edge(ActionRingTheme::Porcelain, wide), px(420.0));
        // 1280×720 at 200%: In Motion shrinks to the 360 px display, Classic
        // stays its 360 px disc, as before themes.
        let short = Size::new(px(640.0), px(360.0));
        assert_eq!(panel_edge(ActionRingTheme::InMotion, short), px(360.0));
        assert_eq!(panel_edge(ActionRingTheme::Obsidian, short), px(360.0));
        assert_eq!(panel_edge(ActionRingTheme::Porcelain, short), px(360.0));
        assert_eq!(panel_edge(ActionRingTheme::Classic, short), px(360.0));
        let tiny = Size::new(px(200.0), px(100.0));
        assert_eq!(panel_edge(ActionRingTheme::InMotion, tiny), px(100.0));
        assert_eq!(panel_edge(ActionRingTheme::Obsidian, tiny), px(100.0));
        assert_eq!(panel_edge(ActionRingTheme::Porcelain, tiny), px(100.0));
        assert_eq!(panel_edge(ActionRingTheme::Classic, tiny), px(360.0));
    }

    #[test]
    fn a_display_sized_ring_draws_nothing_until_the_pointer_is_known() {
        assert_eq!(
            panel_origin(
                ActionRingTheme::InMotion,
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
                ActionRingTheme::InMotion,
                Placement::AtPointer,
                Some(point(px(900.0), px(500.0))),
                Size::new(px(1920.0), px(1080.0))
            ),
            Some(point(px(660.0), px(260.0)))
        );
    }

    #[test]
    fn a_display_sized_ring_stays_whole_against_an_edge() {
        assert_eq!(
            panel_origin(
                ActionRingTheme::InMotion,
                Placement::AtPointer,
                Some(point(px(4.0), px(1076.0))),
                Size::new(px(1920.0), px(1080.0))
            ),
            Some(point(px(0.0), px(600.0)))
        );
    }

    #[test]
    fn a_display_shorter_than_the_ring_shrinks_the_panel_to_its_short_side() {
        // 1280×720 at 200%: 640×360 logical. The panel becomes 360 px and
        // sits flush with the top and bottom edges instead of overhanging.
        let viewport = Size::new(px(640.0), px(360.0));
        assert_eq!(panel_edge(ActionRingTheme::InMotion, viewport), px(360.0));
        assert_eq!(
            panel_origin(
                ActionRingTheme::InMotion,
                Placement::AtPointer,
                Some(point(px(320.0), px(180.0))),
                viewport
            ),
            Some(point(px(140.0), px(0.0)))
        );
        assert_eq!(
            panel_edge(ActionRingTheme::InMotion, Size::new(px(1920.0), px(1080.0))),
            px(skin::DESIGN_EDGE),
            "a display taller than the ring leaves it at full size"
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

    #[test]
    fn every_look_decides_its_own_closing_once() {
        let now = Instant::now();
        for theme in [
            ActionRingTheme::InMotion,
            ActionRingTheme::Obsidian,
            ActionRingTheme::Porcelain,
        ] {
            let mut look = RingLook::from(theme);
            assert!(!look.is_closing(), "{theme:?} opens with no exit playing");
            assert_eq!(
                look.begin_close(now),
                Some(true),
                "{theme:?} plays its exit"
            );
            assert!(look.is_closing());
            assert_eq!(
                look.begin_close(now),
                Some(false),
                "{theme:?} does not restart an exit that is playing"
            );
        }
        let mut classic = RingLook::from(ActionRingTheme::Classic);
        assert_eq!(
            classic.begin_close(now),
            None,
            "the classic disc goes at once"
        );
        assert!(!classic.is_closing());
    }

    fn invocation(theme: ActionRingTheme) -> ActionRingInvocation {
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
            theme,
        }
    }

    /// Where the `Top` slot's centre lands when the panel centres on `center`
    /// well away from every edge.
    fn top_slot_at(theme: ActionRingTheme, center: Point<Pixels>) -> Point<Pixels> {
        let (x, y) = match theme {
            ActionRingTheme::Classic => classic::slot_center(ActionRingSlot::Top),
            ActionRingTheme::InMotion => skin::slot_center(ActionRingSlot::Top),
            ActionRingTheme::Obsidian => obsidian::slot_center(ActionRingSlot::Top),
            ActionRingTheme::Porcelain => porcelain::slot_center(ActionRingSlot::Top),
        };
        let half = px(design_edge(theme) / 2.0);
        point(center.x - half + px(x), center.y - half + px(y))
    }

    #[gpui::test]
    fn a_ring_window_that_loses_the_pointer_stops_drawing_and_hovering(cx: &mut TestAppContext) {
        loses_the_pointer(ActionRingTheme::InMotion, cx);
    }

    #[gpui::test]
    fn a_classic_ring_hovers_like_the_in_motion_one(cx: &mut TestAppContext) {
        loses_the_pointer(ActionRingTheme::Classic, cx);
    }

    #[gpui::test]
    fn an_obsidian_ring_hovers_like_the_in_motion_one(cx: &mut TestAppContext) {
        loses_the_pointer(ActionRingTheme::Obsidian, cx);
    }

    #[gpui::test]
    fn a_porcelain_ring_hovers_like_the_in_motion_one(cx: &mut TestAppContext) {
        loses_the_pointer(ActionRingTheme::Porcelain, cx);
    }

    fn loses_the_pointer(theme: ActionRingTheme, cx: &mut TestAppContext) {
        let live = Arc::new(ClickAwaySession::new());
        let (commands, mut sent) = mpsc::unbounded_channel();
        let pointer = cx.new(|_| RingPointer::default());
        let open = |cx: &mut TestAppContext| -> WindowHandle<RingView> {
            let (commands, pointer, live) = (commands.clone(), pointer.clone(), Arc::clone(&live));
            cx.add_window(move |window, cx| {
                RingView::new(
                    invocation(theme),
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
        below_cx.simulate_mouse_move(top_slot_at(theme, center), None, Modifiers::default());
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

        above_cx.simulate_mouse_move(top_slot_at(theme, center), None, Modifiers::default());
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
