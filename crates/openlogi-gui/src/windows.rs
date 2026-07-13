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
    App, AppContext as _, Bounds, Context, Global, Pixels, Render, SharedString, Size, Styled as _,
    Subscription, TitlebarOptions, WindowBounds, WindowHandle, WindowOptions,
};
use gpui_component::{ActiveTheme as _, Root};
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
        app_id: Some("openlogi".to_string()),
        titlebar: Some(TitlebarOptions {
            title: Some(title.clone()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
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
