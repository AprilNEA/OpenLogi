//! The ring itself: the GPUI view, and the window it is drawn in.
//!
//! Where a client can place its own windows, the window is the ring: a 360×360
//! panel centred on the cursor and clamped to its display. Wayland allows
//! neither half of that, so there each display gets a window covering it and
//! the same panel is drawn at the pointer, in the one window that heard it last.

use gpui::{
    Bounds, Context, Div, Entity, Hsla, InteractiveElement, IntoElement, MouseMoveEvent,
    ParentElement, Pixels, Point, Render, SharedString, Size, StatefulInteractiveElement as _,
    Styled, Subscription, Window, WindowAppearance, WindowBackgroundAppearance, WindowId,
    WindowKind, WindowOptions, div, point, prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::{Action, ActionRingSlot};
use openlogi_ipc::ActionRingInvocation;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::ipc::OverlayCommand;
use crate::session::{self, ClickAwaySession, ShowingRing};

/// The window the ring is drawn in. It is larger than the ring so a hovered
/// slot's label fits beside it; everything outside the slots is transparent.
pub(crate) const WINDOW_SIZE: f32 = 360.0;
pub(crate) const SLOT_SIZE: f32 = 44.0;
/// A hovered slot grows by about 15% around the same centre.
const SELECTED_SLOT_SIZE: f32 = 50.0;
pub(crate) const RADIUS: f32 = 76.0;
const GLYPH_SIZE: f32 = 20.0;
const SELECTED_GLYPH_SIZE: f32 = 22.0;
const CANCEL_SIZE: f32 = 28.0;
const CANCEL_GLYPH_SIZE: f32 = 14.0;
/// Space between a hovered slot and its label.
const LABEL_GAP: f32 = 6.0;
/// Space a label keeps from the window edge, where its shadow would be cut off.
const LABEL_EDGE: f32 = 4.0;
const LABEL_MAX_WIDTH: f32 = 160.0;
const LABEL_LINE_HEIGHT: f32 = 16.0;
const LABEL_PADDING_X: f32 = 8.0;
const LABEL_PADDING_Y: f32 = 4.0;
/// A label wraps once, then ends in an ellipsis; [`LABEL_MAX_HEIGHT`] fits both lines.
const LABEL_LINES: usize = 2;
const LABEL_MAX_HEIGHT: f32 = LABEL_LINE_HEIGHT * 2.0 + LABEL_PADDING_Y * 2.0;
/// Identifies the ring's windows to the compositor, whatever kind they are.
pub(crate) const RING_APP_ID: &str = "openlogi-action-ring";

/// The ring's neutral surfaces in one system appearance.
///
/// The slots float over whatever is on the desktop with nothing behind them,
/// so each slot carries its own contrast: a near-opaque fill, a hairline, and a
/// shadow. The ring follows the system appearance rather than the settings
/// app's theme, which this process does not load. Only the accent is shared
/// (`openlogi_ui::color`); these greys are local by nature.
#[derive(Clone, Copy)]
struct RingColors {
    slot: Hsla,
    slot_border: Hsla,
    glyph: Hsla,
    cancel: Hsla,
    cancel_hover: Hsla,
    cancel_glyph: Hsla,
    label: Hsla,
    label_text: Hsla,
}

impl RingColors {
    const LIGHT: Self = Self {
        slot: neutral(0.99, 0.94),
        slot_border: neutral(0.0, 0.10),
        glyph: neutral(0.20, 1.0),
        cancel: neutral(0.93, 0.94),
        cancel_hover: neutral(0.86, 0.96),
        cancel_glyph: neutral(0.30, 1.0),
        label: neutral(0.12, 0.92),
        label_text: neutral(0.98, 1.0),
    };

    const DARK: Self = Self {
        slot: neutral(0.20, 0.92),
        slot_border: neutral(1.0, 0.12),
        glyph: neutral(0.92, 1.0),
        cancel: neutral(0.26, 0.92),
        cancel_hover: neutral(0.33, 0.95),
        cancel_glyph: neutral(0.80, 1.0),
        label: neutral(0.95, 0.95),
        label_text: neutral(0.10, 1.0),
    };

    const fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::LIGHT,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::DARK,
        }
    }
}

const fn neutral(lightness: f32, alpha: f32) -> Hsla {
    Hsla {
        h: 0.0,
        s: 0.0,
        l: lightness,
        a: alpha,
    }
}

/// The accent deepened for a selected slot: the brand lightness sits too close
/// to the white glyph it carries, so the fill drops to `0.48` and the ring
/// around it rises to `0.78`. Both keep the brand hue and saturation, and read
/// the same over light and dark desktops.
const SELECTED_FILL_L: f32 = 0.48;
const SELECTED_BORDER_L: f32 = 0.78;
const SELECTED_GLYPH: Hsla = neutral(1.0, 1.0);

/// The ring's footprint, out to a hovered slot's edge plus a margin.
///
/// A click between the slots belongs to the ring, but macOS hands a click on a
/// nearly transparent pixel to the window below: at 1/255 alpha it reached the
/// app under the ring, at 0.01 the ring held it (measured on macOS 27). A mid
/// grey at that alpha shifts any desktop colour by under 1%.
const FOOTPRINT: f32 = 2.0 * (RADIUS + SELECTED_SLOT_SIZE / 2.0 + LABEL_GAP);
const FOOTPRINT_FILL: Hsla = neutral(0.5, 0.01);

/// Which way a label grows from the point it is pinned to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grow {
    Before,
    Centred,
    After,
}

impl Grow {
    /// Away from the ring's centre along one axis of a slot's unit offset.
    fn away_from_centre(offset: f32) -> Self {
        if offset > 0.01 {
            Self::After
        } else if offset < -0.01 {
            Self::Before
        } else {
            Self::Centred
        }
    }
}

/// The box a hovered slot's label may fill, in panel coordinates, and the
/// edges of it the label keeps to.
///
/// The label sits just outside its slot and grows away from the ring's centre,
/// so it never covers another slot, and it stays in the window. Straight out
/// from a side slot there is room only for a short word, so a side slot's label
/// sits on its upper shoulder instead.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LabelPlace {
    left: f32,
    top: f32,
    width: f32,
    horizontal: Grow,
    vertical: Grow,
}

impl LabelPlace {
    fn of(slot: ActionRingSlot) -> Self {
        let centre = WINDOW_SIZE / 2.0;
        let (x, y) = slot.unit_offset();
        let horizontal = Grow::away_from_centre(x);
        let ((anchor_x, anchor_y), vertical) = if Grow::away_from_centre(y) == Grow::Centred {
            (
                (
                    centre + x * (RADIUS + LABEL_GAP),
                    centre - SELECTED_SLOT_SIZE / 2.0 - LABEL_GAP,
                ),
                Grow::Before,
            )
        } else {
            let reach = RADIUS + SELECTED_SLOT_SIZE / 2.0 + LABEL_GAP;
            (
                (centre + x * reach, centre + y * reach),
                Grow::away_from_centre(y),
            )
        };
        let room_before = anchor_x - LABEL_EDGE;
        let room_after = WINDOW_SIZE - LABEL_EDGE - anchor_x;
        let width = match horizontal {
            Grow::Before => room_before,
            Grow::After => room_after,
            Grow::Centred => 2.0 * room_before.min(room_after),
        }
        .min(LABEL_MAX_WIDTH);
        let left = match horizontal {
            Grow::Before => anchor_x - width,
            Grow::Centred => anchor_x - width / 2.0,
            Grow::After => anchor_x,
        };
        let top = match vertical {
            Grow::Before => anchor_y - LABEL_MAX_HEIGHT,
            Grow::Centred => anchor_y - LABEL_MAX_HEIGHT / 2.0,
            Grow::After => anchor_y,
        };
        Self {
            left,
            top,
            width,
            horizontal,
            vertical,
        }
    }
}

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
        colors: RingColors,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let presentation = self.invocation.slots.get(&slot)?;
        let icon_path = presentation.icon.asset_path();
        let selected = self.hovered == Some(slot);
        let (size, glyph_size, glyph) = if selected {
            (SELECTED_SLOT_SIZE, SELECTED_GLYPH_SIZE, SELECTED_GLYPH)
        } else {
            (SLOT_SIZE, GLYPH_SIZE, colors.glyph)
        };
        // A hovered slot grows around the centre it rests on.
        let (left, top) = slot.placement(WINDOW_SIZE, RADIUS, size);
        let session_id = self.invocation.session_id;
        let activate = self.commands.clone();
        Some(
            div()
                .id(("ring-slot", slot.index()))
                .absolute()
                .left(px(left))
                .top(px(top))
                .size(px(size))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .map(|slot| {
                    if selected {
                        slot.border_2()
                            .border_color(color::accent_at_lightness(SELECTED_BORDER_L))
                            .bg(color::accent_at_lightness(SELECTED_FILL_L))
                    } else {
                        slot.border_1()
                            .border_color(colors.slot_border)
                            .bg(colors.slot)
                    }
                })
                .shadow_md()
                .cursor_pointer()
                .child(svg().path(icon_path).size(px(glyph_size)).text_color(glyph))
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
                })
                .into_any_element(),
        )
    }
}

impl RingView {
    /// The fixed 360×360 ring. Both placements anchor this same element.
    fn panel(&self, colors: RingColors, cx: &mut Context<Self>) -> Div {
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
            Some((slot, SharedString::from(label)))
        });
        let slots = ActionRingSlot::ALL
            .into_iter()
            .filter_map(|slot| self.slot_element(slot, colors, cx))
            .collect::<Vec<_>>();

        div()
            .relative()
            .size(px(WINDOW_SIZE))
            .child(
                div()
                    .absolute()
                    .left(px((WINDOW_SIZE - FOOTPRINT) / 2.0))
                    .top(px((WINDOW_SIZE - FOOTPRINT) / 2.0))
                    .size(px(FOOTPRINT))
                    .rounded_full()
                    .bg(FOOTPRINT_FILL),
            )
            .children(slots)
            .child(
                div()
                    .id("ring-cancel")
                    .absolute()
                    .left(px((WINDOW_SIZE - CANCEL_SIZE) / 2.0))
                    .top(px((WINDOW_SIZE - CANCEL_SIZE) / 2.0))
                    .size(px(CANCEL_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(colors.slot_border)
                    .bg(colors.cancel)
                    .hover(move |cancel| cancel.bg(colors.cancel_hover))
                    .cursor_pointer()
                    // An `svg` paints only with its own text colour; a parent's
                    // does not reach it.
                    .child(
                        svg()
                            .path(RING_CANCEL_ICON)
                            .size(px(CANCEL_GLYPH_SIZE))
                            .flex_none()
                            .text_color(colors.cancel_glyph),
                    )
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let _ = center_commands.send(OverlayCommand::Cancel { session_id });
                        session::dismiss_from_window(cx, window, session_id);
                    }),
            )
            .when_some(hovered_label, |ring, (slot, label)| {
                ring.child(label_element(LabelPlace::of(slot), label, colors))
            })
    }
}

/// A hovered slot's label, kept to the edges of its [`LabelPlace`] that touch
/// the slot. Long labels wrap once and then end in an ellipsis.
fn label_element(place: LabelPlace, label: SharedString, colors: RingColors) -> Div {
    div()
        .absolute()
        .left(px(place.left))
        .top(px(place.top))
        .w(px(place.width))
        .h(px(LABEL_MAX_HEIGHT))
        .flex()
        .map(|region| match place.horizontal {
            Grow::Before => region.justify_end(),
            Grow::Centred => region.justify_center(),
            Grow::After => region.justify_start(),
        })
        .map(|region| match place.vertical {
            Grow::Before => region.items_end(),
            Grow::Centred => region.items_center(),
            Grow::After => region.items_start(),
        })
        .child(
            div()
                .max_w(px(place.width))
                .px(px(LABEL_PADDING_X))
                .py(px(LABEL_PADDING_Y))
                .rounded_md()
                .bg(colors.label)
                .shadow_sm()
                .text_xs()
                .line_height(px(LABEL_LINE_HEIGHT))
                .text_color(colors.label_text)
                .line_clamp(LABEL_LINES)
                .text_ellipsis()
                .child(label),
        )
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
        let colors = RingColors::for_appearance(window.appearance());
        let panel = panel_origin(placement, center, window.viewport_size()).map(|origin| {
            div()
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .child(self.panel(colors, cx))
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
            panel_origin(Placement::Window, None, Size::new(px(360.0), px(360.0))),
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
            Some(point(px(720.0), px(320.0)))
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
            Some(point(px(0.0), px(720.0)))
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

    /// A hovered slot's label, at its longest and tallest, stays in the window
    /// and clear of every slot, including the hovered one at its grown size.
    #[test]
    fn a_hovered_slots_label_stays_in_the_window_and_off_every_slot() {
        let overlaps = |place: LabelPlace, slot: ActionRingSlot, size: f32| {
            let (left, top) = slot.placement(WINDOW_SIZE, RADIUS, size);
            let (cx, cy, r) = (left + size / 2.0, top + size / 2.0, size / 2.0);
            let nearest_x = cx.clamp(place.left, place.left + place.width);
            let nearest_y = cy.clamp(place.top, place.top + LABEL_MAX_HEIGHT);
            (cx - nearest_x).hypot(cy - nearest_y) < r
        };
        for hovered in ActionRingSlot::ALL {
            let place = LabelPlace::of(hovered);
            assert!(
                place.left >= 0.0
                    && place.top >= 0.0
                    && place.left + place.width <= WINDOW_SIZE
                    && place.top + LABEL_MAX_HEIGHT <= WINDOW_SIZE,
                "{hovered:?}'s label leaves the window: {place:?}"
            );
            assert!(
                place.width >= 90.0,
                "{hovered:?}'s label has room for only {} pt",
                place.width
            );
            for slot in ActionRingSlot::ALL {
                let size = if slot == hovered {
                    SELECTED_SLOT_SIZE
                } else {
                    SLOT_SIZE
                };
                assert!(
                    !overlaps(place, slot, size),
                    "{hovered:?}'s label covers {slot:?}: {place:?}"
                );
            }
        }
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
