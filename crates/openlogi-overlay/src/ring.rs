//! The ring itself: the GPUI view, and the window it is drawn in.
//!
//! Placement is the interesting part — the panel is centred on the cursor and
//! clamped to the display it came up on, so a ring raised near a screen edge
//! stays whole instead of being cut off.

use std::sync::Arc;

use gpui::{
    Bounds, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, Point, Render,
    SharedString, Size, StatefulInteractiveElement as _, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point,
    prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::{Action, ActionRingSlot};
use openlogi_ipc::ActionRingInvocation;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use openlogi_ui::color;
use tokio::sync::mpsc;
use tracing::warn;

use crate::agent::OverlayCommand;
use crate::platform;
use crate::session::ClickAwaySession;

pub(crate) const WINDOW_SIZE: f32 = 324.0;
pub(crate) const SLOT_SIZE: f32 = 54.0;
pub(crate) const RADIUS: f32 = 122.0;

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

const SELECTED_FILL_L: f32 = 0.48;
const SELECTED_BORDER_L: f32 = 0.78;

pub(crate) struct RingView {
    invocation: Option<ActionRingInvocation>,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    hovered: Option<ActionRingSlot>,
    live_session: Arc<ClickAwaySession>,
    persistent: bool,
}

impl RingView {
    pub(crate) fn new(
        invocation: ActionRingInvocation,
        commands: mpsc::UnboundedSender<OverlayCommand>,
        live_session: Arc<ClickAwaySession>,
    ) -> Self {
        Self {
            invocation: Some(invocation),
            commands,
            hovered: None,
            live_session,
            persistent: false,
        }
    }

    pub(crate) fn idle(
        commands: mpsc::UnboundedSender<OverlayCommand>,
        live_session: Arc<ClickAwaySession>,
    ) -> Self {
        Self {
            invocation: None,
            commands,
            hovered: None,
            live_session,
            persistent: true,
        }
    }

    pub(crate) fn current_session(&self) -> Option<u64> {
        self.invocation
            .as_ref()
            .map(|invocation| invocation.session_id)
    }

    pub(crate) fn install(&mut self, invocation: ActionRingInvocation, cx: &mut Context<Self>) {
        self.hovered = None;
        self.invocation = Some(invocation);
        cx.notify();
    }

    pub(crate) fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session_id = self.current_session();
        self.hovered = None;
        self.invocation = None;
        if let Some(session_id) = session_id {
            self.live_session.clear_if(session_id);
        }
        cx.notify();
        if self.persistent {
            if !platform::hide_window(window) {
                warn!("could not hide warm Actions Ring window");
            }
        } else {
            window.remove_window();
        }
    }

    pub(crate) fn dismiss(
        &mut self,
        session_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !session_targets(session_id, self.current_session()) {
            return false;
        }
        self.hide(window, cx);
        true
    }

    pub(crate) fn cancel(&self) {
        if let Some(session_id) = self.current_session() {
            let _ = self.commands.send(OverlayCommand::Cancel { session_id });
        }
    }

    fn slot_element(
        &self,
        slot: ActionRingSlot,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let invocation = self.invocation.as_ref()?;
        let presentation = invocation.slots.get(&slot)?;
        let icon_path = presentation.icon.asset_path();
        let selected = self.hovered == Some(slot);
        let (left, top) = slot.placement(WINDOW_SIZE, RADIUS, SLOT_SIZE);
        let session_id = invocation.session_id;
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
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    let _ = activate.send(OverlayCommand::Activate { session_id, slot });
                    this.dismiss(session_id, window, cx);
                }))
                .into_any_element(),
        )
    }
}

#[must_use]
const fn session_targets(session_id: u64, open_session: Option<u64>) -> bool {
    matches!(open_session, Some(open) if open == session_id)
}

impl Render for RingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(invocation) = self.invocation.clone() else {
            return div().id("ring-root-idle").size_full();
        };
        let session_id = invocation.session_id;
        let root_commands = self.commands.clone();
        let center_commands = self.commands.clone();
        let hovered_label = self.hovered.and_then(|slot| {
            let presentation = invocation.slots.get(&slot)?;
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
            .rounded_full()
            .bg(PANEL)
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        let _ = center_commands.send(OverlayCommand::Cancel { session_id });
                        this.dismiss(session_id, window, cx);
                    })),
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
            .on_click(cx.listener(move |this, _, window, cx| {
                let _ = root_commands.send(OverlayCommand::Cancel { session_id });
                this.dismiss(session_id, window, cx);
            }))
    }
}

#[cfg(any(target_os = "windows", test))]
#[expect(
    clippy::cast_possible_truncation,
    reason = "monitor-sized logical coordinates retain sufficient precision as GPUI f32 pixels"
)]
fn cursor_in_logical_display(
    cursor: (f64, f64),
    native_origin: (f64, f64),
    native_size: (f64, f64),
    logical_bounds: &Bounds<Pixels>,
) -> Option<Point<Pixels>> {
    let logical_width = f64::from(logical_bounds.size.width.as_f32());
    let logical_height = f64::from(logical_bounds.size.height.as_f32());
    if native_size.0 <= 0.0 || native_size.1 <= 0.0 || logical_width <= 0.0 || logical_height <= 0.0
    {
        return None;
    }
    let scale_x = native_size.0 / logical_width;
    let scale_y = native_size.1 / logical_height;
    if !scale_x.is_finite() || !scale_y.is_finite() || scale_x <= 0.0 || scale_y <= 0.0 {
        return None;
    }
    Some(point(
        logical_bounds.origin.x + px(((cursor.0 - native_origin.0) / scale_x) as f32),
        logical_bounds.origin.y + px(((cursor.1 - native_origin.1) / scale_y) as f32),
    ))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "native display coordinates are monitor-sized and retain sufficient precision as GPUI f32 pixels"
)]
fn platform_cursor_placement(
    x: f64,
    y: f64,
) -> Option<(gpui::DisplayId, Point<Pixels>, Bounds<Pixels>)> {
    let display = platform::display_containing(x, y)?;
    Some((
        gpui::DisplayId::from(display.id),
        point(
            px((x - display.origin.0) as f32),
            px((y - display.origin.1) as f32),
        ),
        Bounds::new(
            Point::default(),
            Size::new(px(display.size.0 as f32), px(display.size.1 as f32)),
        ),
    ))
}

#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    clippy::cast_possible_truncation,
    reason = "Win32 monitor lookup consumes i32 device pixels and GPUI DisplayId mirrors the HMONITOR value"
)]
fn native_cursor_placement(
    cx: &mut gpui::App,
    x: f64,
    y: f64,
) -> Option<(gpui::DisplayId, Point<Pixels>, Bounds<Pixels>)> {
    use windows_sys::Win32::{
        Foundation::POINT as NativePoint,
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
    };

    let cursor = NativePoint {
        x: x.round() as i32,
        y: y.round() as i32,
    };
    // SAFETY: MonitorFromPoint consumes a by-value POINT and does not dereference caller-owned pointers.
    let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return platform_cursor_placement(x, y);
    }
    let display_id = gpui::DisplayId::from(u64::try_from(monitor as usize).ok()?);
    let logical_bounds = cx
        .displays()
        .into_iter()
        .find(|display| display.id() == display_id)?
        .bounds();

    let mut monitor_info = MONITORINFO {
        cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>()).ok()?,
        ..Default::default()
    };
    // SAFETY: monitor is a non-null HMONITOR returned by MonitorFromPoint, and monitor_info is a valid writable MONITORINFO with cbSize initialized.
    if unsafe { GetMonitorInfoW(monitor, &raw mut monitor_info) } == 0 {
        return None;
    }
    let native_origin = (
        f64::from(monitor_info.rcMonitor.left),
        f64::from(monitor_info.rcMonitor.top),
    );
    let native_size = (
        f64::from(monitor_info.rcMonitor.right - monitor_info.rcMonitor.left),
        f64::from(monitor_info.rcMonitor.bottom - monitor_info.rcMonitor.top),
    );
    let center = cursor_in_logical_display((x, y), native_origin, native_size, &logical_bounds)?;
    Some((display_id, center, logical_bounds))
}

#[cfg(not(target_os = "windows"))]
fn native_cursor_placement(
    _cx: &mut gpui::App,
    x: f64,
    y: f64,
) -> Option<(gpui::DisplayId, Point<Pixels>, Bounds<Pixels>)> {
    platform_cursor_placement(x, y)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "native cursor coordinates are screen-sized and retain sufficient precision as GPUI f32 pixels"
)]
pub(crate) fn ring_window_options(cx: &mut gpui::App, show: bool) -> WindowOptions {
    let cursor = openlogi_hook::cursor_position();
    let size = Size::new(px(WINDOW_SIZE), px(WINDOW_SIZE));
    let native_placement = cursor
        .as_ref()
        .and_then(|cursor| native_cursor_placement(cx, cursor.x, cursor.y));
    let (display_id, center, display_bounds) =
        if let Some((display_id, center, display_bounds)) = native_placement {
            (Some(display_id), center, Some(display_bounds))
        } else {
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
        show,
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
    fn session_target_requires_the_open_session() {
        assert!(session_targets(12, Some(12)));
        assert!(!session_targets(11, Some(12)));
        assert!(!session_targets(12, None));
    }

    #[test]
    fn windows_mixed_dpi_secondary_cursor_maps_to_gpui_space() {
        let logical_bounds =
            Bounds::new(point(px(1280.0), px(0.0)), Size::new(px(1280.0), px(720.0)));
        assert_eq!(
            cursor_in_logical_display(
                (2880.0, 540.0),
                (1920.0, 0.0),
                (1920.0, 1080.0),
                &logical_bounds,
            ),
            Some(point(px(1920.0), px(360.0)))
        );
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
