//! The ring itself: the GPUI view, and the window it is drawn in.
//!
//! Placement is the interesting part, and it takes two shapes because the
//! platforms disagree about who may position a window.
//!
//! Where a client can place its own windows — macOS, Windows, X11 — the window
//! *is* the ring: a 360×360 panel centred on the cursor and clamped to its
//! display, so a ring raised near a screen edge stays whole.
//!
//! Wayland allows neither half of that. No protocol reports the global pointer
//! position to an ordinary client, and an `xdg_toplevel` cannot choose its own
//! origin — `WindowOptions::window_bounds` carries an origin the backend
//! ignores. So there the ring covers the whole display instead, transparent
//! and empty until the compositor's pointer-enter tells it where the cursor
//! is, and the panel is placed at that point *within* the window. One such
//! window per display: `wl_pointer.enter` goes to exactly the surface under
//! the cursor, so the compositor picks the right display for us.

use gpui::{
    Bounds, Context, Div, Hsla, InteractiveElement, IntoElement, MouseMoveEvent, ParentElement,
    Pixels, PlatformDisplay, Point, Render, SharedString, Size, StatefulInteractiveElement as _,
    Styled, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div,
    point, prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::ActionRingSlot;
use openlogi_ipc::ActionRingInvocation;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;
use tokio::sync::mpsc;
use tracing::warn;

use crate::agent::OverlayCommand;
use crate::platform;
use crate::session;

pub(crate) const WINDOW_SIZE: f32 = 360.0;
pub(crate) const SLOT_SIZE: f32 = 54.0;
pub(crate) const RADIUS: f32 = 122.0;

/// The ring's own neutral scale. It floats over whatever is on the desktop, so
/// unlike the settings app it cannot take its surfaces from the OS appearance —
/// it commits to a dark panel and rides its own contrast. Only the accent is
/// shared (`openlogi_ui::color`); these greys are local by nature.
const PANEL: Hsla = neutral(0.06, 0.82);
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
    /// The window is the ring: it was opened at the cursor already, so the
    /// panel fills it.
    Window,
    /// The window covers a whole display and the panel is drawn at the pointer
    /// within it. `center` stays `None` until the compositor reports a pointer
    /// position for this window — on the displays the cursor is not on, it
    /// stays `None` for the ring's whole life and nothing is drawn.
    AtPointer { center: Option<Point<Pixels>> },
}

pub(crate) struct RingView {
    invocation: ActionRingInvocation,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    hovered: Option<ActionRingSlot>,
    placement: Placement,
}

impl RingView {
    /// Open a view on `invocation`, reporting interactions through `commands`.
    pub(crate) const fn new(
        invocation: ActionRingInvocation,
        commands: mpsc::UnboundedSender<OverlayCommand>,
        placement: Placement,
    ) -> Self {
        Self {
            invocation,
            commands,
            hovered: None,
            placement,
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
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let presentation = self.invocation.slots.get(&slot)?;
        let icon_path = presentation.icon.asset_path();
        let selected = self.hovered == Some(slot);
        let (left, top) = slot.placement(WINDOW_SIZE, RADIUS, SLOT_SIZE);
        let session_id = self.invocation.session_id;
        let activate = self.commands.clone();
        Some(
            div()
                .id(("ring-slot", slot.index()))
                .absolute()
                .left(px(left))
                .top(px(top))
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
                .child(svg().path(icon_path).size(px(22.0)).text_color(GLYPH))
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
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = activate.send(OverlayCommand::Activate { session_id, slot });
                    session::close_ring_windows(cx, session_id);
                })
                .into_any_element(),
        )
    }

    /// The ring panel: a fixed 360×360 square holding the backdrop, the eight
    /// slots, the cancel affordance and the hovered label. It is the same
    /// element under both placements — only where it is anchored differs.
    fn panel(&self, cx: &mut Context<Self>) -> Div {
        let session_id = self.invocation.session_id;
        let center_commands = self.commands.clone();
        let hovered_label = self.hovered.and_then(|slot| {
            let presentation = self.invocation.slots.get(&slot)?;
            // User-authored labels render verbatim: passing them through the
            // localization table would translate any label that happens to
            // collide with a known key ("Copy" → "Copier" under fr).
            let label = if presentation.literal {
                presentation.label.clone()
            } else {
                rust_i18n::t!(presentation.label.as_str()).into_owned()
            };
            Some(SharedString::from(label))
        });
        let slots = ActionRingSlot::ALL
            .into_iter()
            .filter_map(|slot| self.slot_element(slot, cx))
            .collect::<Vec<_>>();

        div()
            .relative()
            .size(px(WINDOW_SIZE))
            .child(
                div()
                    .absolute()
                    .left(px(18.0))
                    .top(px(18.0))
                    .size(px(WINDOW_SIZE - 36.0))
                    .rounded_full()
                    .bg(PANEL)
                    .shadow_lg(),
            )
            .children(slots)
            .child(
                div()
                    .id("ring-cancel")
                    .absolute()
                    .left(px(WINDOW_SIZE / 2.0 - 24.0))
                    .top(px(WINDOW_SIZE / 2.0 - 24.0))
                    .size(px(48.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(CANCEL_RESTING)
                    .text_color(CANCEL_GLYPH)
                    .cursor_pointer()
                    .child(svg().path(RING_CANCEL_ICON).size(px(20.0)).flex_none())
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        let _ = center_commands.send(OverlayCommand::Cancel { session_id });
                        session::close_ring_windows(cx, session_id);
                    }),
            )
            .when_some(hovered_label, |ring, label| {
                ring.child(
                    div()
                        .absolute()
                        .left(px(WINDOW_SIZE / 2.0 - 80.0))
                        .top(px(WINDOW_SIZE / 2.0 + 34.0))
                        .w(px(160.0))
                        .text_center()
                        .text_sm()
                        .text_color(LABEL)
                        .child(label),
                )
            })
    }
}

impl Render for RingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_id = self.invocation.session_id;
        let root_commands = self.commands.clone();
        let placement = self.placement;

        let panel = panel_origin(placement, window.viewport_size()).map(|origin| {
            div()
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .child(self.panel(cx))
        });

        div()
            .id("ring-root")
            .relative()
            .size_full()
            .when(matches!(placement, Placement::AtPointer { .. }), |root| {
                // The MouseMove GPUI synthesizes from `wl_pointer.enter` is the
                // only report of the cursor a Wayland client gets, and the
                // display the cursor is not on never sends one. Latch the first
                // one: the ring stays where it was raised instead of following
                // the pointer around the display.
                root.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    if let Placement::AtPointer { center } = &mut this.placement
                        && center.is_none()
                    {
                        *center = Some(event.position);
                        cx.notify();
                    }
                }))
            })
            .children(panel)
            .on_click(move |_, _, cx| {
                let _ = root_commands.send(OverlayCommand::Cancel { session_id });
                session::close_ring_windows(cx, session_id);
            })
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "native cursor coordinates are screen-sized and exactly usable as GPUI f32 pixels"
)]
fn ring_window_options(cx: &mut gpui::App) -> WindowOptions {
    let cursor = openlogi_hook::cursor_position();
    let size = Size::new(px(WINDOW_SIZE), px(WINDOW_SIZE));
    // GPUI window bounds are display-relative (`display.bounds()` zeroes every
    // origin) while the hook reports the cursor in global coordinates, so the
    // cursor's display must be resolved natively and the cursor translated into
    // that display's space. Feeding the global point straight into the clamp
    // pins a ring triggered on a secondary display to the primary one's edge.
    let native_display = cursor
        .as_ref()
        .and_then(|cursor| platform::display_containing(cursor.x, cursor.y));
    let (display_id, center, display_bounds) =
        if let (Some(cursor), Some(display)) = (&cursor, native_display) {
            (
                Some(gpui::DisplayId::from(display.id)),
                point(
                    px((cursor.x - display.origin.0) as f32),
                    px((cursor.y - display.origin.1) as f32),
                ),
                Some(Bounds::new(
                    Point::default(),
                    Size::new(px(display.size.0 as f32), px(display.size.1 as f32)),
                )),
            )
        } else {
            // No cursor or no native lookup (non-macOS): GPUI's own display
            // list, centering on the display when the cursor is unknown.
            let cursor_point = cursor
                .as_ref()
                .map(|cursor| point(px(cursor.x as f32), px(cursor.y as f32)));
            let display = cursor_point
                .and_then(|cursor| {
                    cx.displays()
                        .into_iter()
                        .find(|display| display.bounds().contains(&cursor))
                })
                .or_else(|| cx.primary_display());
            let center = cursor_point
                .or_else(|| display.as_ref().map(|display| display.bounds().center()))
                .unwrap_or_default();
            let bounds = display.as_ref().map(|display| display.bounds());
            (display.map(|display| display.id()), center, bounds)
        };
    let desired_origin = point(center.x - size.width / 2.0, center.y - size.height / 2.0);
    let origin = display_bounds.map_or(desired_origin, |display_bounds| {
        clamp_window_origin(desired_origin, size, display_bounds)
    });
    let bounds = Bounds::new(origin, size);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("openlogi-action-ring".to_string()),
        ..WindowOptions::default()
    }
}

/// A transparent window covering `display`, for the placement Wayland forces.
///
/// Covering the display rather than sitting at the cursor because an
/// `xdg_toplevel` cannot choose its own origin. It is also what makes the
/// pointer reachable at all: the compositor reports a position only in
/// surface-local coordinates, and only to the surface under the cursor.
///
/// Maximized, specifically — **not** fullscreen. `xdg_toplevel.set_fullscreen`
/// gets the same coverage, but a compositor presents a fullscreen surface
/// against black rather than against the desktop, so a transparent one renders
/// as a black screen (observed on Mutter). Maximizing asks for the same area
/// without entering that presentation mode.
///
/// The bounds passed here are only the restore size a maximized window would
/// return to, which this window never does. They are also in a different unit
/// from the one the window ends up reporting — GPUI gives display bounds in
/// logical pixels and window bounds in device pixels, which differ under
/// fractional scaling — so nothing may derive a pointer-space coordinate from
/// them. [`panel_origin`] measures against the viewport for that reason.
fn covering_window_options(display: &std::rc::Rc<dyn PlatformDisplay>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Maximized(display.bounds())),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: Some(display.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        // Client-side, and this window draws none. Left at the default the
        // compositor dresses a display-sized window in a title bar.
        window_decorations: Some(gpui::WindowDecorations::Client),
        app_id: Some("openlogi-action-ring".to_string()),
        ..WindowOptions::default()
    }
}

/// Every window one ring invocation should open, with the placement its view
/// must use.
///
/// One window everywhere a client may position its own — and on Wayland one
/// per display, because nothing here can know which display holds the cursor.
/// The compositor settles it: `wl_pointer.enter` reaches exactly the surface
/// under the pointer, so the window that hears from the pointer is the one
/// that draws, and the rest stay empty and transparent until the ring closes.
pub(crate) fn ring_windows(cx: &mut gpui::App) -> Vec<(WindowOptions, Placement)> {
    if !on_wayland() {
        return vec![(ring_window_options(cx), Placement::Window)];
    }
    let displays = cx.displays();
    if displays.is_empty() {
        // Nothing to cover. The positioned path cannot place the ring on
        // Wayland either, but it still puts a ring on screen, which beats
        // opening no window at all.
        warn!("no displays reported; falling back to a positioned Actions Ring window");
        return vec![(ring_window_options(cx), Placement::Window)];
    }
    displays
        .iter()
        .map(|display| {
            (
                covering_window_options(display),
                Placement::AtPointer { center: None },
            )
        })
        .collect()
}

/// Whether GPUI is talking to a Wayland compositor.
///
/// `guess_compositor` is GPUI's own backend selection, so asking it is the one
/// way to stay in step with the backend actually in use rather than guessing
/// from the environment a second time.
fn on_wayland() -> bool {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        gpui::guess_compositor() == "Wayland"
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        false
    }
}

/// Top-left of the ring panel within its window, or `None` while there is
/// nothing to draw.
///
/// Under [`Placement::Window`] the window is the panel, so the two corners
/// coincide. Under [`Placement::AtPointer`] the panel is centred on the pointer
/// and clamped into the window, which covers the display — the same rule the
/// positioned path applies to the window itself, one level in. A pointer this
/// window has not seen yields `None`: on a multi-display Wayland session that
/// is every display except the cursor's, and they draw nothing at all.
///
/// `viewport` and not the display's bounds: GPUI reports display bounds in
/// logical pixels and the pointer in the window's device pixels, so under
/// fractional scaling the two disagree (a 1.5× display reports 1128×752 while
/// its window reports 1692×1128). The viewport is the space the pointer
/// arrives in, so it is the only one this may clamp against.
fn panel_origin(placement: Placement, viewport: Size<Pixels>) -> Option<Point<Pixels>> {
    let center = match placement {
        Placement::Window => return Some(Point::default()),
        Placement::AtPointer { center } => center?,
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
    use super::*;

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
            panel_origin(Placement::Window, Size::new(px(360.0), px(360.0))),
            Some(Point::default())
        );
    }

    #[test]
    fn a_display_sized_ring_draws_nothing_until_the_pointer_is_known() {
        // Every display except the cursor's stays in this state for the ring's
        // whole life, so "no pointer yet" and "no ring here" are the same case.
        assert_eq!(
            panel_origin(
                Placement::AtPointer { center: None },
                Size::new(px(1920.0), px(1080.0))
            ),
            None
        );
    }

    #[test]
    fn a_display_sized_ring_centers_on_the_pointer() {
        assert_eq!(
            panel_origin(
                Placement::AtPointer {
                    center: Some(point(px(900.0), px(500.0))),
                },
                Size::new(px(1920.0), px(1080.0))
            ),
            Some(point(px(720.0), px(320.0)))
        );
    }

    #[test]
    fn a_display_sized_ring_stays_whole_against_an_edge() {
        // A ring raised in the corner would hang off the window, and the window
        // is the display: clamping is the only thing keeping it on screen.
        assert_eq!(
            panel_origin(
                Placement::AtPointer {
                    center: Some(point(px(4.0), px(1076.0))),
                },
                Size::new(px(1920.0), px(1080.0))
            ),
            Some(point(px(0.0), px(720.0)))
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
