//! Application windows — the main window plus the auxiliary ones (Settings,
//! Add Device, …) — and a registry that keeps each one a singleton. About and
//! Updates are pages inside Settings, not their own windows.
//!
//! macOS apps open exactly one Settings window: re-triggering the menu item, ⌘,
//! or a footer link focuses the existing window rather than stacking a second
//! copy. [`WindowRegistry`] holds the live [`WindowHandle`] per slot;
//! [`open_or_focus`] activates it when still open, otherwise opens a fresh one
//! wired for per-window light/dark tracking via
//! [`crate::ui::theme::apply_from_settings`].
//!
//! Every window opens through a [`WindowFit`], which shrinks its authored size
//! and floor to the display's work area so none starts with its titlebar or
//! lower edge off-screen.

pub mod add_device;
pub mod main_window;
pub mod settings;
pub mod update_consent;

#[cfg(test)]
mod tests;

use crate::ui::theme::Typography as _;
use gpui::{
    App, AppContext as _, Bounds, Context, Decorations, Global, IntoElement, ParentElement as _,
    Pixels, Point, Render, SharedString, Size, Styled as _, Subscription, TitlebarOptions, Window,
    WindowBounds, WindowHandle, WindowOptions, div, px,
};
use gpui_component::{ActiveTheme as _, Root, TitleBar};
use openlogi_core::brand::APP_ID;
use tracing::warn;

/// Logical room kept around a window's client area for the native frame, so
/// the *outer* window fits the work area and not just its content.
///
/// gpui treats [`WindowBounds`] and `window_min_size` as the client area and
/// grows the frame outside it. Windows splits its frame (about 39 × 16 px at
/// 100 % and 150 % scaling) evenly around the client, so a client as tall as
/// the work area pushes the titlebar off the top. macOS pins the frame's
/// top-left to the client origin, so its 28 pt titlebar hangs below the
/// client instead. A centred client fits on both while each side keeps half
/// of this allowance.
const FRAME_ALLOWANCE: Size<Pixels> = Size {
    width: px(24.),
    height: px(56.),
};

/// Where a new window opens and how far the user may shrink it, fitted to the
/// work area of the display it opens on.
///
/// Each window is authored for a preferred size and a content floor. When the
/// work area — the display less the taskbar, dock or menu bar — cannot hold
/// them with the native frame, both shrink to what fits, centred in that area.
/// A 1920×1080 laptop at 150 % scaling offers only 1280×680 logical px above
/// the Windows taskbar, and a window opened taller than that has its titlebar
/// and lower edge off-screen, so it can be neither moved nor resized (#2169).
/// Below the authored floor the content may crowd, which still beats a window
/// the user cannot reach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowFit {
    bounds: Bounds<Pixels>,
    min_size: Size<Pixels>,
}

impl WindowFit {
    /// Fit a window authored for `preferred`, with a floor of `min_size`, onto
    /// the primary display — where gpui opens a window that names no display.
    /// With no display reported, the window keeps its authored geometry.
    pub fn on_primary_display(preferred: Size<Pixels>, min_size: Size<Pixels>, cx: &App) -> Self {
        match cx.primary_display() {
            Some(display) => Self::within(display.visible_bounds(), preferred, min_size),
            None => Self {
                bounds: Bounds::new(Point::default(), preferred),
                min_size,
            },
        }
    }

    /// Fit a window authored for `preferred`, with a floor of `min_size`, into
    /// `work_area`.
    fn within(work_area: Bounds<Pixels>, preferred: Size<Pixels>, min_size: Size<Pixels>) -> Self {
        let room = Size {
            width: (work_area.size.width - FRAME_ALLOWANCE.width).max(Pixels::ZERO),
            height: (work_area.size.height - FRAME_ALLOWANCE.height).max(Pixels::ZERO),
        };
        let size = preferred.min(&room);
        Self {
            bounds: Bounds::centered_at(work_area.center(), size),
            min_size: min_size.min(&size),
        }
    }
}

impl From<WindowFit> for WindowOptions {
    fn from(fit: WindowFit) -> Self {
        Self {
            window_bounds: Some(WindowBounds::Windowed(fit.bounds)),
            window_min_size: Some(fit.min_size),
            ..Self::default()
        }
    }
}

/// One live handle per auxiliary window, stored as a GPUI global so the menu
/// actions and footer links can find an already-open window and focus it.
#[derive(Default)]
pub struct WindowRegistry {
    /// The primary app window. Held so the dock-icon reopen handler can bring
    /// it back after the user closes it while the app keeps running in the
    /// background (mouse hook + watchers).
    pub main: Option<WindowHandle<Root>>,
    pub settings: Option<WindowHandle<Root>>,
    pub add_device: Option<WindowHandle<Root>>,
    pub update_consent: Option<WindowHandle<Root>>,
}

impl Global for WindowRegistry {}

/// Whether this window has to draw its own titlebar.
///
/// Only when the compositor actually declined server-side decorations. GNOME's
/// Mutter does not implement `xdg-decoration`, so gpui falls back to
/// client-side and nothing paints the chrome unless we do — which is why the
/// titlebar used to be drawn on Linux unconditionally. KWin *does* implement
/// it and hands back a real server-side titlebar, so that blanket rule put a
/// second one underneath it (#890).
///
/// macOS and Windows always report `Server` here, so the single check covers
/// every platform without a `cfg`.
#[must_use]
pub fn needs_client_titlebar(window: &Window) -> bool {
    matches!(window.window_decorations(), Decorations::Client { .. })
}

/// Titlebar options for an app window.
///
/// On Linux this returns transparent options so the view *can* draw a
/// client-side [`TitleBar`] (see [`aux_title_bar`]) on the compositors that
/// leave it to the app. Whether it actually does is
/// [`needs_client_titlebar`]'s call, made per window from what the compositor
/// negotiated. On macOS / Windows this keeps the native titlebar carrying
/// `title`, unchanged.
pub fn titlebar_options(title: impl Into<SharedString>) -> TitlebarOptions {
    if cfg!(target_os = "linux") {
        TitleBar::title_bar_options()
    } else {
        TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: false,
            traffic_light_position: None,
        }
    }
}

/// Client-side window titlebar for auxiliary windows: window controls
/// (minimize / maximize / close on Linux + Windows), the drag region, and the
/// window `title` centred. Each auxiliary view renders this as the top of its
/// layout so the window has a titlebar and controls on Linux, where the
/// compositor declines server-side decorations and gpui's client-side fallback
/// is otherwise unpainted. On macOS the widget reserves the traffic-light space.
pub fn aux_title_bar(title: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let title = title.into();
    TitleBar::new().child(
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_body()
            .text_color(cx.theme().muted_foreground)
            .child(title),
    )
}

/// Restamp the native titles of the open auxiliary windows after a live
/// language switch. The Linux client-side titlebar re-renders with the window
/// refresh, but the macOS / Windows native titlebar keeps the string stamped
/// at open. The main and update-consent windows are titled with the product
/// name, which never changes.
pub fn retitle_open(cx: &mut App) {
    let registry = cx.default_global::<WindowRegistry>();
    let retitles = [
        (registry.settings, settings::window_title()),
        (registry.add_device, add_device::window_title()),
    ];
    for (handle, title) in retitles {
        if let Some(handle) = handle {
            let _ = handle.update(cx, |_, window, _| window.set_window_title(&title));
        }
    }
}

/// Implemented by every auxiliary root view so [`open_or_focus`] can hand it
/// the appearance observer to hold onto — dropping the [`Subscription`] would
/// detach the OS light/dark tracking and leave the window stuck on one theme.
pub trait AuxWindow: Render + Sized {
    fn set_appearance_obs(&mut self, sub: Subscription);
}

/// Focus the window stored in `slot` if it's still open, otherwise open a new
/// one and record its handle.
///
/// `build_view` constructs the root view inside the freshly opened window; the
/// helper wraps it in [`Root`], installs the same OS-appearance observer the
/// main window uses, and stores the handle so the next call focuses instead of
/// duplicating.
pub fn open_or_focus<V: AuxWindow + 'static>(
    slot: impl Fn(&mut WindowRegistry) -> &mut Option<WindowHandle<Root>>,
    title: impl Into<SharedString>,
    size: Size<Pixels>,
    build_view: impl FnOnce(&mut gpui::Window, &mut Context<V>) -> V + 'static,
    cx: &mut App,
) {
    let title = title.into();

    // An action dispatched from this window temporarily takes it out of GPUI's
    // storage, so a failed update does not mean it closed. Check the live handles
    // and activate after dispatch returns the borrowed window.
    let existing = *slot(cx.default_global::<WindowRegistry>());
    if let Some(handle) = existing
        && cx.windows().contains(&handle.into())
    {
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        });
        return;
    }

    // Aux windows are fixed-content dialogs authored for `size`; that makes it
    // the floor too, the way the main window sets one in `main_window` — below
    // it rows clip their trailing controls. Only a work area too small to hold
    // `size` lowers it.
    let fit = WindowFit::on_primary_display(size, size, cx);
    let options = WindowOptions {
        // Same identity as the main window (see `main_window`): a Wayland
        // app_id that doesn't match the desktop file's `StartupWMClass` leaves
        // the window ungrouped under the launcher icon and unrecognized by our
        // own `gnome_shell` frontmost backend.
        app_id: Some(APP_ID.into()),
        titlebar: Some(titlebar_options(title.clone())),
        ..WindowOptions::from(fit)
    };

    let opened = cx.open_window(options, |window, cx| {
        crate::ui::theme::apply_from_settings(Some(window), cx);
        let view = cx.new(|cx| build_view(window, cx));
        let appearance_obs = window.observe_window_appearance(|window, cx| {
            crate::ui::theme::apply_from_settings(Some(window), cx);
        });
        view.update(cx, |v, _| v.set_appearance_obs(appearance_obs));
        cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
    });

    match opened {
        Ok(handle) => {
            let _ = handle.update(cx, |_, window, _| {
                window.activate_window();
                window.set_window_title(&title);
            });
            *slot(cx.default_global::<WindowRegistry>()) = Some(handle);
        }
        Err(e) => warn!(error = %e, title = %title, "could not open auxiliary window"),
    }
}
