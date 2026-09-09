//! The ring itself: the GPUI view, and the window it is drawn in.
//!
//! Placement is the interesting part — the panel is centred on the cursor and
//! clamped to the display it came up on, so a ring raised near a screen edge
//! stays whole instead of being cut off.

use gpui::{
    AppContext as _, Bounds, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels,
    Point, Render, SharedString, Size, StatefulInteractiveElement as _, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point,
    prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::{Action, ActionRingSlot};
use openlogi_ipc::ActionRingInvocation;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;
use std::sync::Arc;
use tokio::sync::mpsc;
#[cfg(target_os = "linux")]
use tokio::sync::oneshot;

use crate::agent::OverlayCommand;
use crate::platform;
use crate::session::{ClickAwaySession, ShowingRing};

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

pub(crate) struct RingView {
    invocation: ActionRingInvocation,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    hovered: Option<ActionRingSlot>,
    /// Publishes click-away identity for exactly this view's lifetime.
    _showing: ShowingRing,
    /// The invisible full-screen Wayland layer-shell window this ring is an
    /// `AnchoredPopup` child of, if [`open_ring`] opened one — `None`
    /// everywhere else (macOS, Windows, Linux/X11, or Wayland without
    /// `zwlr_layer_shell_v1`). Every place that closes this window must also
    /// close this one, or it lingers as an invisible click-blocking layer
    /// over the whole screen (#1206).
    host: Option<gpui::AnyWindowHandle>,
}

impl RingView {
    /// Open a view on `invocation`, reporting interactions through `commands`.
    pub(crate) fn new(
        invocation: ActionRingInvocation,
        commands: mpsc::UnboundedSender<OverlayCommand>,
        live: &Arc<ClickAwaySession>,
        host: Option<gpui::AnyWindowHandle>,
    ) -> Self {
        let showing = live.showing(invocation.session_id);
        Self {
            invocation,
            commands,
            hovered: None,
            _showing: showing,
            host,
        }
    }

    /// The ring session this view is showing.
    pub(crate) const fn session_id(&self) -> u64 {
        self.invocation.session_id
    }

    /// The host window ([`Self::host`]) a click-away dismissal must also close.
    pub(crate) const fn host(&self) -> Option<gpui::AnyWindowHandle> {
        self.host
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
        let host = self.host;
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
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let _ = activate.send(OverlayCommand::Activate { session_id, slot });
                    close_ring(window, cx, host);
                })
                .into_any_element(),
        )
    }
}

impl Render for RingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_id = self.invocation.session_id;
        let root_commands = self.commands.clone();
        let center_commands = self.commands.clone();
        let root_host = self.host;
        let center_host = self.host;
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
            .filter_map(|slot| self.slot_element(slot, cx))
            .collect::<Vec<_>>();

        div()
            .id("ring-root")
            .relative()
            .size_full()
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
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let _ = center_commands.send(OverlayCommand::Cancel { session_id });
                        close_ring(window, cx, center_host);
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
            .on_click(move |_, window, cx| {
                let _ = root_commands.send(OverlayCommand::Cancel { session_id });
                close_ring(window, cx, root_host);
            })
    }
}

/// Close a ring window and, if it has one, its Wayland layer-shell host
/// together — every dismissal path but the display-lifetime timeout (which
/// closes the host itself, see `main.rs`) goes through here.
fn close_ring(window: &mut Window, cx: &mut gpui::App, host: Option<gpui::AnyWindowHandle>) {
    window.remove_window();
    if let Some(host) = host {
        let _ = host.update(cx, |_, window, _| window.remove_window());
    }
}

/// Where the ring belongs: which display it's on, its clamped top-left origin
/// on that display (zeroed to the display's own top-left, matching GPUI's
/// display-relative window bounds), and that display's own zero-origin bounds.
#[expect(
    clippy::cast_possible_truncation,
    reason = "native cursor coordinates are screen-sized and exactly usable as GPUI f32 pixels"
)]
fn ring_placement(
    cx: &mut gpui::App,
) -> (
    Option<gpui::DisplayId>,
    Point<Pixels>,
    Option<Bounds<Pixels>>,
) {
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
    (display_id, origin, display_bounds)
}

/// Open the Actions Ring for `invocation`, reporting interactions through
/// `commands`. Returns the ring's window handle and, on Linux/Wayland, the
/// invisible layer-shell host it opened alongside it (every dismissal path
/// must close that host together with the ring — see [`RingView::host`]).
///
/// A plain `WindowKind::PopUp` (used everywhere else, and as the Linux
/// fallback when no Wayland layer-shell is available) is just another
/// toplevel to a Wayland compositor — nothing stops it from being drawn under
/// a panel/dock's own always-on-top surface. Anchoring the ring as a popup off
/// a `Layer::Overlay` host instead inherits that host's guaranteed
/// above-everything stacking (#1206).
pub(crate) async fn open_ring(
    cx: &mut gpui::AsyncApp,
    invocation: ActionRingInvocation,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    live_session: Arc<ClickAwaySession>,
) -> anyhow::Result<(gpui::WindowHandle<RingView>, Option<gpui::AnyWindowHandle>)> {
    let size = Size::new(px(WINDOW_SIZE), px(WINDOW_SIZE));
    let host = linux_wayland_ring_host(cx, invocation.session_id).await;
    let options = host.map_or_else(
        || {
            let (display_id, origin, _) = cx.update(ring_placement);
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size))),
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
        },
        |(host, cursor)| {
            // The anchor point is the host's real, live-tracked cursor
            // position, already local to the host's own surface — no
            // display-relative math needed (#1206).
            WindowOptions {
                // AnchoredPopup ignores window_bounds' origin (see its own
                // doc comment) — only the size matters here, the host's
                // anchor_rect carries the position.
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(Point::default(), size))),
                titlebar: None,
                focus: false,
                show: true,
                kind: WindowKind::AnchoredPopup(gpui::popup::PopupOptions {
                    parent: host,
                    // Center anchor + center gravity centers the popup
                    // exactly on the anchor point — no manual half-size
                    // offset to get subtly wrong (#1206).
                    anchor_rect: Bounds::new(cursor, Size::default()),
                    anchor: gpui::popup::PopupAnchor::Center,
                    gravity: gpui::popup::PopupGravity::Center,
                    constraint_adjustment: gpui::popup::PopupConstraintAdjustment::SLIDE_X
                        | gpui::popup::PopupConstraintAdjustment::SLIDE_Y,
                    offset: Point::default(),
                    grab: false,
                }),
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                display_id: None,
                window_background: WindowBackgroundAppearance::Transparent,
                app_id: Some("openlogi-action-ring".to_string()),
                ..WindowOptions::default()
            }
        },
    );
    let host = host.map(|(host, _)| host);
    let opened = cx.update(|cx| {
        cx.open_window(options, move |_, cx| {
            cx.new(|_| RingView::new(invocation, commands, &live_session, host))
        })
    });
    let handle = match opened {
        Ok(handle) => handle,
        Err(error) => {
            // No RingView exists for this host yet, so nothing else will ever
            // close it — leaving it open would block clicks across the whole
            // screen until the next ring invocation or process exit.
            if let Some(host) = host {
                cx.update(|cx| {
                    let _ = host.update(cx, |_, window, _| window.remove_window());
                });
            }
            return Err(error);
        }
    };
    Ok((handle, host))
}

/// Open the invisible full-screen `Layer::Overlay` window the ring anchors to
/// on Linux/Wayland, so the ring inherits guaranteed above-panel stacking, and
/// read the real cursor position from it. `None` on every other platform, on
/// Linux/X11, or when the compositor doesn't support `zwlr_layer_shell_v1` —
/// the ring then falls back to a plain `WindowKind::PopUp` at
/// [`ring_placement`]'s (best-effort) guess, exactly as before this host
/// existed.
///
/// Wayland deliberately has no query for "where is the pointer right now" —
/// [`openlogi_hook::cursor_position`] falls back to XWayland's X11
/// `query_pointer` under a Wayland session, which is not a real Wayland
/// pointer and does not track one; it reports whatever coordinate it last
/// held (often stale, and always display-ambiguous on a multi-monitor
/// desktop). This host sidesteps that entirely: it opens, then waits for the
/// compositor's own first pointer-move delivery — this surface covers the
/// whole desktop with no other `zwlr_layer_shell_v1` surface competing for
/// it, so that motion event's position is the real cursor position, already
/// local to this surface (#1206). [`gpui::Window::mouse_position`] read
/// immediately after `open_window` returns is **not** equivalent: the
/// platform round-trip that carries the compositor's first `enter`/`motion`
/// hasn't necessarily happened yet, so that read can (and, observed live,
/// intermittently does) return a stale value from whatever this process last
/// knew — capped at [`CURSOR_WAIT`] and falling back to
/// [`ring_placement`]'s guess so a slow or absent compositor still
/// eventually opens *a* ring rather than hanging.
///
/// No visible content otherwise: its only job is being a layer-shell surface
/// the ring can be a positioned `AnchoredPopup` child of, and a click-away
/// target (see [`RingHostView`]'s own doc comment). Its output is left to the
/// compositor's own choice (no `display_id`) rather than guessed from the
/// same broken cursor position — most compositors, including KWin, place an
/// unrequested layer-shell surface on the output the pointer is currently
/// over, which is exactly the output this ring needs to open on.
#[cfg(target_os = "linux")]
const CURSOR_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

#[cfg(target_os = "linux")]
async fn linux_wayland_ring_host(
    cx: &mut gpui::AsyncApp,
    session_id: u64,
) -> Option<(gpui::AnyWindowHandle, Point<Pixels>)> {
    if gpui::guess_compositor() != "Wayland" {
        return None;
    }
    let (cursor_tx, cursor_rx) = oneshot::channel();
    let opened = cx.update(|cx| {
        // A placeholder size only: anchoring to all four edges makes the
        // compositor's own configure response the real, authoritative size
        // regardless of what's requested here.
        let size = cx
            .primary_display()
            .map_or_else(|| Size::new(px(1920.0), px(1080.0)), |d| d.bounds().size);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(Point::default(), size))),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::LayerShell(gpui::layer_shell::LayerShellOptions {
                namespace: "openlogi-action-ring-host".to_string(),
                layer: gpui::layer_shell::Layer::Overlay,
                anchor: gpui::layer_shell::Anchor::TOP
                    | gpui::layer_shell::Anchor::BOTTOM
                    | gpui::layer_shell::Anchor::LEFT
                    | gpui::layer_shell::Anchor::RIGHT,
                exclusive_zone: None,
                exclusive_edge: None,
                margin: None,
                keyboard_interactivity: gpui::layer_shell::KeyboardInteractivity::None,
            }),
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id: None,
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some("openlogi-action-ring-host".to_string()),
            ..WindowOptions::default()
        };
        cx.open_window(options, |_, cx| {
            cx.new(|_| RingHostView {
                session_id,
                cursor_tx: std::rc::Rc::new(std::cell::RefCell::new(Some(cursor_tx))),
            })
        })
    });
    let handle = match opened {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(
                %error,
                "could not open the Actions Ring's layer-shell host — \
                 falling back to a plain popup, which a panel may draw over"
            );
            return None;
        }
    };
    let cursor = tokio::select! {
        cursor = cursor_rx => cursor.ok(),
        () = cx.background_executor().timer(CURSOR_WAIT) => {
            tracing::warn!(
                "no pointer motion on the Actions Ring's layer-shell host within {CURSOR_WAIT:?} \
                 — opening at a best-effort guess instead"
            );
            None
        }
    };
    let cursor = if let Some(cursor) = cursor {
        cursor
    } else {
        // `ring_placement`'s origin is in global desktop coordinates (what
        // the `WindowKind::PopUp` fallback wants), but this anchor must be
        // local to the host's own surface — the same conversion the host
        // itself exists to avoid needing on the happy path.
        let (_, origin, display_bounds) = cx.update(ring_placement);
        display_bounds.map_or(origin, |bounds| origin - bounds.origin)
    };
    Some((handle.into(), cursor))
}

// `async` only to match the Linux implementation's signature, which the
// caller `.await`s unconditionally — this stub has nothing to await.
#[cfg(not(target_os = "linux"))]
#[expect(clippy::allow_attributes, reason = "see below")]
#[allow(
    clippy::unused_async,
    reason = "kept async to match the Linux implementation's signature"
)]
async fn linux_wayland_ring_host(
    _cx: &mut gpui::AsyncApp,
    _session_id: u64,
) -> Option<(gpui::AnyWindowHandle, Point<Pixels>)> {
    None
}

/// The invisible layer-shell window [`linux_wayland_ring_host`] opens. A
/// click anywhere on it (i.e. anywhere outside the ring it hosts) dismisses
/// that ring, and its first mouse-move reports the real cursor position back
/// to whoever is waiting on [`Self::cursor_tx`] — see
/// [`linux_wayland_ring_host`]'s own doc comment for both.
#[cfg(target_os = "linux")]
struct RingHostView {
    session_id: u64,
    /// Taken and fired on this view's first mouse-move; `None` after. Shared
    /// (not owned outright) because [`Render::render`] hands a fresh
    /// `on_mouse_move` closure to a new element tree on every call, and each
    /// must see whether an earlier one already fired it.
    cursor_tx: std::rc::Rc<std::cell::RefCell<Option<oneshot::Sender<Point<Pixels>>>>>,
}

#[cfg(target_os = "linux")]
impl Render for RingHostView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let session_id = self.session_id;
        let cursor_tx = std::rc::Rc::clone(&self.cursor_tx);
        div()
            .id("ring-host")
            .size_full()
            .on_mouse_move(move |event, _window, _cx| {
                if let Some(tx) = cursor_tx.borrow_mut().take() {
                    let _ = tx.send(event.position);
                }
            })
            .on_click(move |_, _window, cx| {
                crate::session::dismiss_click_away(cx, session_id);
            })
    }
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
    fn overlay_origin_stays_cursor_centered_away_from_edges() {
        let display = Bounds::new(Point::default(), Size::new(px(1600.0), px(1000.0)));
        let desired = point(px(600.0), px(300.0));
        assert_eq!(
            clamp_window_origin(desired, Size::new(px(400.0), px(400.0)), display),
            desired
        );
    }
}
