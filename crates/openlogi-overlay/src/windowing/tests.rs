use gpui::{IntoElement, Render, TestAppContext, Window, WindowHandle};

use super::*;

const AFTER: Duration = Duration::from_secs(30);

struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

fn open(cx: &mut TestAppContext) -> WindowHandle<Blank> {
    cx.add_window(|_, _| Blank)
}

fn close(cx: &mut TestAppContext, window: WindowHandle<Blank>) {
    window
        .update(cx, |_, window, _| window.remove_window())
        .expect("the window is still open");
    cx.run_until_parked();
}

/// Whether a release has been scheduled since the last check cleared it.
fn release_scheduled(cx: &mut TestAppContext) -> bool {
    cx.update(|cx| cx.global_mut::<IdleRelease>().pending.take().is_some())
}

#[gpui::test]
fn only_the_last_window_closing_starts_the_release_wait(cx: &mut TestAppContext) {
    cx.update(|cx| install_with(cx, AFTER));
    let ring = open(cx);
    let notice = open(cx);
    assert!(!release_scheduled(cx), "opening a window schedules nothing");

    close(cx, ring);
    assert!(
        !release_scheduled(cx),
        "a notice still showing keeps the display"
    );

    close(cx, notice);
    assert!(
        release_scheduled(cx),
        "the last window closing must start the wait, or the display is never let go"
    );
}

#[gpui::test]
fn a_connection_that_opens_nothing_is_still_let_go(cx: &mut TestAppContext) {
    cx.update(|cx| install_with(cx, AFTER));
    // The test platform cannot switch modes, so the connection itself fails;
    // only the release it schedules matters here.
    drop(cx.update(connect));
    assert!(
        release_scheduled(cx),
        "no window will close to start the wait, so connecting must start it"
    );
}

/// The test platform refuses every switch, so a connection that asked for
/// one would fail. A switch to windowed would put a macOS overlay in the Dock.
#[cfg(not(target_os = "windows"))]
#[gpui::test]
async fn away_from_windows_connecting_never_switches_modes(cx: &mut TestAppContext) {
    cx.update(install);
    assert!(!cx.update(|cx| cx.has_global::<IdleRelease>()));
    cx.update(connect)
        .await
        .expect("away from Windows the overlay never asks GPUI to switch modes");
}
