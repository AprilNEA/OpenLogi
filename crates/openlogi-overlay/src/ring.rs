//! The ring itself: the GPUI view, and the window it is drawn in.
//!
//! Placement is the interesting part — the panel is centred on the cursor and
//! clamped to the display it came up on, so a ring raised near a screen edge
//! stays whole instead of being cut off.

use gpui::{
    Bounds, Context, InteractiveElement, IntoElement, ParentElement, Pixels, Point, Render,
    SharedString, Size, StatefulInteractiveElement as _, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, hsla, point,
    prelude::FluentBuilder as _, px, svg,
};
use openlogi_core::binding::ActionRingSlot;
use openlogi_ipc::ActionRingInvocation;
use tokio::sync::mpsc;

use crate::agent::OverlayCommand;
use crate::platform;

pub(crate) const WINDOW_SIZE: f32 = 360.0;
pub(crate) const SLOT_SIZE: f32 = 54.0;
pub(crate) const RADIUS: f32 = 122.0;

pub(crate) struct RingView {
    invocation: ActionRingInvocation,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    hovered: Option<ActionRingSlot>,
}

impl RingView {
    /// Open a view on `invocation`, reporting interactions through `commands`.
    pub(crate) const fn new(
        invocation: ActionRingInvocation,
        commands: mpsc::UnboundedSender<OverlayCommand>,
    ) -> Self {
        Self {
            invocation,
            commands,
            hovered: None,
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
                    hsla(0.59, 0.72, 0.48, 1.0)
                } else {
                    hsla(0.0, 0.0, 0.16, 0.98)
                })
                .when(selected, |slot| {
                    slot.border_2().border_color(hsla(0.59, 0.90, 0.72, 1.0))
                })
                .shadow_md()
                .text_color(hsla(0.0, 0.0, 0.98, 1.0))
                .cursor_pointer()
                .child(
                    svg()
                        .path(icon_path)
                        .size(px(22.0))
                        .text_color(hsla(0.0, 0.0, 0.98, 1.0)),
                )
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
                    .bg(hsla(0.0, 0.0, 0.06, 0.82))
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
                    .bg(hsla(0.0, 0.0, 0.20, 0.98))
                    .text_color(hsla(0.0, 0.0, 0.82, 1.0))
                    .text_lg()
                    .cursor_pointer()
                    .child("×")
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let _ = center_commands.send(OverlayCommand::Cancel { session_id });
                        window.remove_window();
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
                        .text_color(hsla(0.0, 0.0, 0.94, 1.0))
                        .child(label),
                )
            })
            .on_click(move |_, window, _| {
                let _ = root_commands.send(OverlayCommand::Cancel { session_id });
                window.remove_window();
            })
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "native cursor coordinates are screen-sized and exactly usable as GPUI f32 pixels"
)]
pub(crate) fn ring_window_options(cx: &mut gpui::App) -> WindowOptions {
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
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "panic helpers are idiomatic in tests"
)]
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
