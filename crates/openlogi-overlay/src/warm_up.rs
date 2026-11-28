//! Hidden ring windows, opened when the overlay starts.
//!
//! On macOS the first window a GPUI process opens is slow: from the overlay
//! hearing of a ring to its first draw took 81 ms the first time, 39 ms the
//! second and about 20 ms after that (MacBook Air, release build). The time
//! goes into creating the window, before the ring draws anything, so it is
//! paid once per process however the ring looks. The overlay is a helper the
//! agent keeps running, so it pays that cost at start, out of sight. With the
//! warm-up the first ring took 33 ms and the later ones 19 ms.
//!
//! Only macOS warms up. Windows starts headless and would have to connect to
//! the display just for this, and on Linux a hidden window is up to the
//! compositor.

use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, Context, IntoElement, ParentElement as _, Render, Styled as _,
    Window, WindowBounds, WindowOptions, div, point, px, size, svg,
};
use openlogi_core::binding::ActionRingSize;
use openlogi_ui::action_icons::RING_CANCEL_ICON;
use tracing::debug;

use crate::ring::{Geometry, ring_window_options};

/// How many warm-up windows to open, one after the other. The second
/// window still cost twice what the later ones did, so one is not enough.
const ROUNDS: usize = 2;

/// How long a warm-up window stays open. It is never on screen, so this only
/// has to outlast its first draw.
const LINGER: Duration = Duration::from_millis(250);

/// The ring window, sized like a medium ring, but never shown.
fn options() -> WindowOptions {
    let edge = px(Geometry::of(ActionRingSize::Medium).window);
    WindowOptions {
        show: false,
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.0), px(0.0)),
            size(edge, edge),
        ))),
        ..ring_window_options()
    }
}

/// Draws what a ring draws: a glyph from the embedded icons.
struct WarmUp;

impl Render for WarmUp {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(svg().path(RING_CANCEL_ICON).size(px(14.0)))
    }
}

/// Open and close [`ROUNDS`] hidden ring windows, one at a time.
pub(crate) fn warm_up(cx: &mut App) {
    cx.spawn(async move |cx| {
        for _ in 0..ROUNDS {
            let opened = cx.update(|cx| cx.open_window(options(), |_, cx| cx.new(|_| WarmUp)));
            let handle = match opened {
                Ok(handle) => handle,
                Err(error) => {
                    debug!(%error, "could not open a warm-up ring window");
                    return;
                }
            };
            cx.background_executor().timer(LINGER).await;
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    })
    .detach();
}

#[cfg(test)]
mod tests;
