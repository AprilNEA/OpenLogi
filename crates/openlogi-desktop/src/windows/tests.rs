//! Window sizing and singleton lifecycles.

use gpui::{AppContext as _, Bounds, Pixels, Size, TestAppContext, point, px, size};

use super::main_window::{MIN_SIZE, PREFERRED_SIZE};
use super::{FRAME_ALLOWANCE, WindowFit, WindowRegistry, settings};
use crate::app::menu::{self, OpenAbout, OpenSettings};
use crate::services::{assets::AssetResolver, i18n::LOCALE_LOCK};
use crate::state::{AppState, Sources};
use openlogi_core::config::Config;

#[gpui::test]
fn settings_commands_reuse_the_live_window_and_reopen_after_close(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::theme::register_builtin_themes(cx);
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let state = cx.new(|_| {
            AppState::new(Sources::in_memory(
                Config::ephemeral(),
                &AssetResolver::new(),
                commands,
            ))
        });
        AppState::set_global(state, cx);
        menu::install(cx);
        settings::open(cx);
    });
    let settings = cx.update(|cx| cx.global::<WindowRegistry>().settings.unwrap());

    for _ in 0..3 {
        cx.dispatch_action(settings.into(), OpenSettings);
        cx.dispatch_action(settings.into(), OpenAbout);
        cx.update(|cx| {
            assert_eq!(cx.windows(), vec![settings.into()]);
            assert_eq!(cx.active_window(), Some(settings.into()));
        });
    }

    let other = cx.open_window(size(px(400.), px(300.)), |_, _| gpui::Empty);
    cx.update_window(other.into(), |_, window, _| window.activate_window())
        .unwrap();
    cx.update(|cx| assert_eq!(cx.active_window(), Some(other.into())));
    cx.dispatch_action(other.into(), OpenSettings);
    cx.update(|cx| {
        assert_eq!(cx.windows().len(), 2);
        assert_eq!(cx.active_window(), Some(settings.into()));
    });

    cx.update_window(settings.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.dispatch_action(other.into(), OpenSettings);
    cx.update(|cx| {
        let reopened = cx.global::<WindowRegistry>().settings.unwrap();
        assert_ne!(reopened, settings);
        assert_eq!(cx.windows().len(), 2);
        assert_eq!(cx.active_window(), Some(reopened.into()));
    });
}

/// The Settings window's authored size, which is also its floor.
const SETTINGS: Size<Pixels> = Size {
    width: px(920.),
    height: px(640.),
};

fn work_area(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
}

/// 1920×1080 at 100 % with the 40 px Windows taskbar.
fn full_hd() -> Bounds<Pixels> {
    work_area(0., 0., 1920., 1040.)
}

/// 1920×1080 at 150 % with the 40 px (60 device px) taskbar: the 14-inch
/// laptop in #2169.
fn full_hd_at_150() -> Bounds<Pixels> {
    work_area(0., 0., 1280., 680.)
}

/// Whether the client plus half the frame allowance on every side stays inside
/// `area`. Windows splits its frame evenly around the client and macOS hangs
/// its titlebar below it; both frames fit inside that margin.
fn frame_on_screen(fit: &WindowFit, area: Bounds<Pixels>) -> bool {
    let half_w = FRAME_ALLOWANCE.width / 2.;
    let half_h = FRAME_ALLOWANCE.height / 2.;
    fit.bounds.left() - half_w >= area.left()
        && fit.bounds.right() + half_w <= area.right()
        && fit.bounds.top() - half_h >= area.top()
        && fit.bounds.bottom() + half_h <= area.bottom()
}

#[test]
fn a_work_area_that_holds_the_main_window_keeps_its_authored_geometry() {
    let area = full_hd();
    let fit = WindowFit::within(area, PREFERRED_SIZE, MIN_SIZE);

    assert_eq!(fit.bounds.size, PREFERRED_SIZE);
    assert_eq!(fit.bounds.center(), area.center());
    assert_eq!(fit.min_size, MIN_SIZE);
    assert!(frame_on_screen(&fit, area));
}

#[test]
fn the_main_window_fits_a_150_percent_full_hd_laptop() {
    let area = full_hd_at_150();
    let fit = WindowFit::within(area, PREFERRED_SIZE, MIN_SIZE);

    // Before #2169 the window opened 820 px tall in a 680 px work area, with
    // its titlebar above the top of the screen and a floor it could not reach.
    assert!(
        frame_on_screen(&fit, area),
        "{fit:?} leaves the frame off-screen in {area:?}"
    );
    assert!(fit.bounds.size.height < PREFERRED_SIZE.height);
    assert!(fit.min_size.height <= fit.bounds.size.height);
    assert!(fit.min_size.height < MIN_SIZE.height);
    // The width floor fits this display and stays as authored.
    assert_eq!(fit.min_size.width, MIN_SIZE.width);
}

#[test]
fn the_window_centres_in_a_work_area_that_does_not_start_at_the_origin() {
    // A secondary display to the right of the primary, with a top taskbar.
    let area = work_area(1920., 48., 1280., 680.);
    let fit = WindowFit::within(area, PREFERRED_SIZE, MIN_SIZE);

    assert_eq!(fit.bounds.center(), area.center());
    assert!(
        frame_on_screen(&fit, area),
        "{fit:?} leaves the frame off-screen in {area:?}"
    );
}

#[test]
fn an_aux_window_keeps_its_floor_unless_the_work_area_cannot_hold_it() {
    let roomy = WindowFit::within(full_hd(), SETTINGS, SETTINGS);
    assert_eq!(roomy.bounds.size, SETTINGS);
    assert_eq!(roomy.min_size, SETTINGS);

    let area = full_hd_at_150();
    let tight = WindowFit::within(area, SETTINGS, SETTINGS);
    assert!(
        frame_on_screen(&tight, area),
        "{tight:?} leaves the frame off-screen in {area:?}"
    );
    assert_eq!(tight.bounds.size.width, SETTINGS.width);
    assert!(tight.bounds.size.height < SETTINGS.height);
    assert_eq!(tight.min_size, tight.bounds.size);
}

#[test]
fn a_work_area_smaller_than_the_frame_yields_an_empty_size_not_a_negative_one() {
    let fit = WindowFit::within(work_area(0., 0., 10., 10.), PREFERRED_SIZE, MIN_SIZE);

    assert_eq!(fit.bounds.size, Size::default());
    assert_eq!(fit.min_size, Size::default());
}
