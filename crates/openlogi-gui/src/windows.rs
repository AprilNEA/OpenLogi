//! Auxiliary application windows (Settings, Add Device, …) and a registry that
//! keeps each one a singleton. About and Updates are pages inside Settings, not
//! their own windows.
//!
//! macOS apps open exactly one Settings window: re-triggering the menu item, ⌘,
//! or a footer link focuses the existing window rather than stacking a second
//! copy. [`WindowRegistry`] holds the live [`WindowHandle`] per slot;
//! [`open_or_focus`] activates it when still open, otherwise opens a fresh one
//! wired for per-window light/dark tracking via
//! [`crate::theme::apply_from_settings`].

pub mod add_device;
pub mod settings;
pub mod update_consent;

use gpui::{
    App, AppContext as _, Bounds, Context, Global, IntoElement, ParentElement as _, Pixels, Render,
    SharedString, Size, Styled as _, Subscription, TitlebarOptions, WindowBounds, WindowHandle,
    WindowOptions, div,
};
use gpui_component::{ActiveTheme as _, Root, TitleBar};
use tracing::warn;

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

/// Titlebar options for an app window.
///
/// On Linux this returns transparent options so the view can draw a client-side
/// [`TitleBar`] (see [`aux_title_bar`]); the compositor declines server-side
/// decorations there and gpui's client-side fallback is otherwise unpainted,
/// leaving the window with no titlebar or controls. On macOS / Windows it keeps
/// the native titlebar carrying `title`, unchanged.
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
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(title),
    )
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

    // Already open? Focus it and bail. A closed window leaves a stale handle
    // whose `update` errors, falling through to a fresh open.
    let existing = *slot(cx.default_global::<WindowRegistry>());
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }

    let bounds = Bounds::centered(None, size, cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Aux windows are fixed-content dialogs authored for `size`; that
        // makes it the floor too, the way the main window sets one in
        // `main_window_options` — below it rows clip their trailing controls.
        window_min_size: Some(size),
        app_id: Some("openlogi".to_string()),
        titlebar: Some(titlebar_options(title.clone())),
        ..WindowOptions::default()
    };

    let opened = cx.open_window(options, |window, cx| {
        crate::theme::apply_from_settings(Some(window), cx);
        let view = cx.new(|cx| build_view(window, cx));
        let appearance_obs = window.observe_window_appearance(|window, cx| {
            crate::theme::apply_from_settings(Some(window), cx);
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
