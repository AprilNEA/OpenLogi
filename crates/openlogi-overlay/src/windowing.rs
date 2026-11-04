//! When the overlay holds its connection to the display.
//!
//! A windowed GPUI process on Windows runs a vsync thread that calls
//! `DwmFlush` in a loop, whether or not a window is open. With the display
//! off, the call stops waiting for a frame, so the loop keeps `dwm.exe` busy
//! for as long as the process lives (#1603). The overlay has no window for
//! almost all of its life, so on Windows it starts headless, connects before
//! it opens a window, and lets go again once it has shown nothing for
//! [`IDLE_RELEASE`]. GPUI owns the mode itself; this module only decides when
//! to ask for a switch.
//!
//! Elsewhere the overlay stays connected. On macOS a windowed GPUI app is a
//! `Regular` one, with a Dock icon, and on Linux every ring would open a new
//! display-server connection.

use std::time::Duration;

use anyhow::Result;
use gpui::{App, Global, GraphicalEnvironment, Task, WindowingRequest};
use tracing::{debug, warn};

/// How long the overlay stays connected with nothing showing, so a ring
/// opened again soon after does not wait for a new connection.
const IDLE_RELEASE: Duration = Duration::from_secs(30);

/// The release wait on this platform, or `None` to stay connected.
const PLATFORM_RELEASE: Option<Duration> = if cfg!(target_os = "windows") {
    Some(IDLE_RELEASE)
} else {
    None
};

/// The mode the overlay starts in, for [`gpui::Application::with_windowing`],
/// or `None` for GPUI's default.
pub(crate) fn initial() -> Option<WindowingRequest> {
    PLATFORM_RELEASE.map(|_| WindowingRequest::Headless)
}

/// Let go of the display whenever the overlay has shown nothing for a while,
/// on the platforms that need it.
pub(crate) fn install(cx: &mut App) {
    if let Some(after) = PLATFORM_RELEASE {
        install_with(cx, after);
    }
}

/// Connect to the display, if the overlay let go of it, before opening a
/// window. The returned task resolves once windows can open.
///
/// Every connection starts the release wait again, so one that ends up
/// opening nothing is still let go.
pub(crate) fn connect(cx: &mut App) -> Task<Result<()>> {
    if !cx.has_global::<IdleRelease>() {
        return Task::ready(Ok(()));
    }
    schedule_release(cx);
    if cx.graphical_environment().is_some() {
        return Task::ready(Ok(()));
    }
    debug!("connecting to the display");
    cx.request_windowing(WindowingRequest::Windowed(GraphicalEnvironment::detect()))
}

/// The pending release, present only where the overlay lets go of the
/// display.
struct IdleRelease {
    after: Duration,
    /// Dropping the task cancels the release it is waiting to make.
    pending: Option<Task<()>>,
}

impl Global for IdleRelease {}

fn install_with(cx: &mut App, after: Duration) {
    cx.set_global(IdleRelease {
        after,
        pending: None,
    });
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            schedule_release(cx);
        }
    })
    .detach();
}

/// Start the release wait again, replacing the one already pending.
fn schedule_release(cx: &mut App) {
    let after = cx.global::<IdleRelease>().after;
    let release = cx.spawn(async move |cx| {
        cx.background_executor().timer(after).await;
        let Some(switch) = cx.update(begin_release) else {
            return;
        };
        match switch.await {
            Ok(()) => debug!("let go of the display while nothing shows"),
            Err(error) => warn!(%error, "could not let go of the display"),
        }
    });
    cx.global_mut::<IdleRelease>().pending = Some(release);
}

/// Ask GPUI to go headless, unless a window is open or it already is.
fn begin_release(cx: &mut App) -> Option<Task<Result<()>>> {
    (cx.windows().is_empty() && cx.graphical_environment().is_some())
        .then(|| cx.request_windowing(WindowingRequest::Headless))
}

#[cfg(test)]
mod tests;
