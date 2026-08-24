//! The primary application window.
//!
//! Kept a singleton through the same [`WindowRegistry`] slot the auxiliary
//! windows use, so the dock-icon reopen handler, the tray's "Show Main Window"
//! and any repeat call all land on the live window instead of stacking a
//! duplicate — and a window closed while the app kept running in the menu bar
//! can be brought back.

use gpui::{App, AppContext as _, Bounds, Size, Styled as _, WindowBounds, WindowOptions, px};
use gpui_component::{ActiveTheme as _, Root};
use openlogi_core::brand::APP_ID;
use openlogi_core::device::DeviceInventory;
use tracing::warn;

use crate::app::AppView;
use crate::ui::theme;
use crate::windows::{WindowRegistry, titlebar_options};

fn window_options(cx: &mut App) -> WindowOptions {
    let bounds = Bounds::centered(None, Size::new(px(1100.), px(750.)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Advertise a Wayland xdg-toplevel app_id (and X11 WM_CLASS). Without it
        // the window ships no app_id, so GNOME's `get_wm_class()` returns empty
        // and our own `gnome_shell` frontmost backend reports OpenLogi as `None`
        // (and the dash can't group the window under its launcher icon). The id
        // is the shared `brand::APP_ID`, matching the desktop file's
        // `StartupWMClass` and the macOS bundle-id family.
        app_id: Some(APP_ID.into()),
        // Min height keeps the buttons tab's mouse model above its scale floor
        // (`MODEL_MIN_H` + the chrome/padding reserve) so its side labels never
        // overlap; below this the model can't shrink further without crowding.
        window_min_size: Some(Size::new(px(720.), px(680.))),
        // Linux: transparent chrome so `AppView::render` can draw a client-side
        // `TitleBar` (the compositor declines server-side decorations and gpui's
        // fallback is unpainted). macOS/Windows keep their native titlebar.
        titlebar: Some(titlebar_options("OpenLogi")),
        ..WindowOptions::default()
    }
}

/// Open the main window — or focus the one already open.
pub fn open(inventories: &[DeviceInventory], cx: &mut App) {
    let existing = cx.default_global::<WindowRegistry>().main;
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return;
    }

    let options = window_options(cx);
    let opened = cx.open_window(options, |window, cx| {
        theme::apply_from_settings(Some(window), cx);

        let view = cx.new(|cx| AppView::new(inventories, window, cx));

        let appearance_obs = window.observe_window_appearance(|window, cx| {
            theme::apply_from_settings(Some(window), cx);
        });
        view.update(cx, |v, _| v.set_appearance_obs(appearance_obs));

        cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
    });

    match opened {
        Ok(handle) => {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            cx.default_global::<WindowRegistry>().main = Some(handle);
            cx.activate(true);
        }
        Err(e) => warn!(error = %e, "could not open the main window"),
    }
}

/// Open the main window as the session anchor when no window is currently open.
///
/// The auxiliary windows are standalone, so opening one with no main window up
/// would leave the app quitting the moment that window closes.
pub fn ensure(cx: &mut App) {
    if cx.windows().is_empty() {
        open(&[], cx);
    }
}
