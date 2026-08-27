//! The Settings window — a standalone OS window (⌘, / menu bar / the right
//! panel's Configuration card) exposing the app-wide preferences in
//! [`openlogi_core::config::AppSettings`].
//!
//! Uses gpui-component's Settings widget so page navigation, search, and the
//! left sidebar share the same behaviour as the rest of that component set.

// Shared imports for the whole Settings module, re-exported so each page
// submodule can pull them in with `use super::{…}`. Traits are imported by name
// (not `as _`) so the re-export carries their methods to the submodules; the
// `.on_click` / track-focus methods need them on every platform.
pub(super) use std::rc::Rc;

pub(super) use gpui::{
    App, AppContext, Axis, ClipboardItem, Context, Entity, FocusHandle, FontWeight, Hsla,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Size,
    StatefulInteractiveElement, Styled, Subscription, Window, div, img, prelude::FluentBuilder, px,
    rgb,
};
pub(super) use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Selectable, Sizable, TITLE_BAR_HEIGHT,
    Theme, ThemeColor, ThemeMode, ThemeRegistry,
    button::{Button, ButtonGroup, ButtonVariants},
    group_box::GroupBoxVariant,
    h_flex,
    input::{InputEvent, InputState},
    select::{SelectEvent, SelectItem, SelectState},
    setting::{SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    slider::{Slider, SliderEvent, SliderState},
    tag::Tag,
    theme::ThemeConfig,
    v_flex,
};
pub(super) use gpui_updater::{UpdateStatus, Updater};
pub(super) use openlogi_core::brand::{HELP_URL, RELEASES_URL, REPO_URL};
pub(super) use openlogi_core::config::{
    Appearance, AssetSourcePreference, ThumbwheelSensitivity, UiScale, VerticalScrollSensitivity,
};

pub(super) use crate::app::menu::{CloseWindow, Minimize, Zoom};
pub(super) use crate::services::assets::sync::{AssetCommand, AssetControl};
pub(super) use crate::state::{AppState, StateEvent};
pub(super) use crate::ui::theme::{self, Palette};
#[cfg(target_os = "macos")]
pub(super) use openlogi_permissions::Permission;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) use openlogi_permissions::PermissionStatus;

use crate::windows::{self, AuxWindow};

mod about;
mod appearance;
mod assets;
// Event-tap enumeration is a macOS (`CGEventTap`) concept; the Diagnostics page
// that surfaces it is macOS-only.
#[cfg(target_os = "macos")]
mod diagnostics;
mod general;
mod language;
// Windows needs no privacy grants — the WH_MOUSE_LL hook and raw HID access
// work without one — so there the page would render empty; register it only
// where it has content. `SettingsPage::index` tracks the shift.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod permissions;
mod updates;

/// Which sidebar page the window opens to. Maps to the page order in
/// [`SettingsView::render`]; menu items deep-link here (About / Updates).
#[derive(Clone, Copy, Default)]
pub enum SettingsPage {
    #[default]
    General,
    Updates,
    About,
}

impl SettingsPage {
    /// Sidebar index — must track the `.page(...)` order in `render`.
    fn index(self) -> usize {
        match self {
            Self::General => 0,
            Self::Updates => 1,
            // One lower on Windows: the Permissions page isn't registered
            // there (see the `mod permissions` cfg).
            Self::About => {
                if cfg!(any(target_os = "macos", target_os = "linux")) {
                    5
                } else {
                    4
                }
            }
        }
    }
}

/// Appearance-page theme-grid filter. View-local (not persisted) UI state.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ThemeFilter {
    #[default]
    All,
    Light,
    Dark,
}

/// Standalone Settings window root view.
pub struct SettingsView {
    focus_handle: FocusHandle,
    appearance_obs: Option<Subscription>,
    _state_obs: Subscription,
    /// Which themes the Appearance grid shows (All / Light / Dark).
    theme_filter: ThemeFilter,
    /// Free-text filter for the Appearance theme grid (search 50+ themes by name).
    theme_search: Entity<InputState>,
    /// Page selected when the window first opens. Consumed once by the Settings
    /// widget's keyed state, so it only steers a fresh open (an already-open
    /// window is just focused).
    initial_page: SettingsPage,
    language_select: Entity<SelectState<Vec<language::LanguageOption>>>,
    asset_source_select: Entity<SelectState<Vec<assets::AssetSourceOption>>>,
    thumbwheel_sensitivity_slider: Entity<SliderState>,
    vertical_scroll_sensitivity_slider: Entity<SliderState>,
    /// Shared app-wide updater, surfaced on the Updates page. A launch-time
    /// check result is already visible when the window opens.
    updater: Entity<Updater>,
    #[expect(
        dead_code,
        reason = "held to re-render the Updates page on status change"
    )]
    updater_obs: Subscription,
    /// `true` for ~2s after a diagnostics copy, so the About button can flip its
    /// label to a confirmation.
    copied: bool,
    /// Bumped on each copy so a stale reset timer can't clear a newer confirmation.
    copied_gen: u64,
    /// Asset-cache size blurb, computed once when the window opens rather than
    /// re-walking the cache on every render. A snapshot — reopen to refresh
    /// after a Clear.
    asset_cache_desc: SharedString,
    /// Snapshot of the agent service's registration status, taken when the
    /// window opens and after every settings change (the status read is an
    /// XPC round-trip, so it must not run per frame). Drives the General
    /// page's "switched off in System Settings" notice; a flip made outside
    /// the app shows up on the next settings change or reopen.
    registration_status: crate::platform::registration::ServiceStatus,
    /// Drives the debug live event monitor: polls the agent on a timer while the
    /// Settings window is open. Dropping it with the view stops polling, which
    /// lets the agent's idle janitor turn monitoring back off.
    #[cfg(all(target_os = "macos", debug_assertions))]
    _monitor_task: gpui::Task<()>,
}

impl SettingsView {
    fn new(initial_page: SettingsPage, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        // Reuse the app-wide shared updater installed at launch, so a launch-time
        // check result is already visible. Fall back to a fresh one if it somehow
        // wasn't installed.
        let updater = crate::platform::updater::shared(cx)
            .unwrap_or_else(|| crate::platform::updater::new_entity(cx));
        let updater_obs = cx.observe(&updater, |_, _, cx| cx.notify());
        let state_obs = cx.subscribe(&AppState::global(cx), |this, _, event: &StateEvent, cx| {
            if matches!(event, StateEvent::LanguageChanged) {
                // The cache-size line is localized text cached in view state
                // (it interpolates an IO-derived size); rebuild it in the new
                // locale.
                this.asset_cache_desc = assets::cache_size_description();
            }
            if matches!(
                event,
                StateEvent::AgentChanged
                    | StateEvent::DiagnosticsChanged
                    | StateEvent::InventoryChanged
                    | StateEvent::CameraPermissionChanged
                    | StateEvent::SettingsChanged
                    | StateEvent::LanguageChanged
            ) {
                // A settings change may have run the opportunistic
                // registration ensure, so re-read the status snapshot.
                if matches!(event, StateEvent::SettingsChanged) {
                    this.registration_status = crate::platform::registration::status();
                }
                cx.notify();
            }
        });

        let theme_search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("Filter themes…")));
        cx.subscribe(&theme_search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let current = AppState::try_read(cx).and_then(|s| s.app_settings().language.clone());
        let options = language::language_options();
        let selected = language::selected_language_index(current.as_deref(), &options);
        let language_select = cx.new(|cx| SelectState::new(options, Some(selected), window, cx));
        cx.subscribe_in(&language_select, window, Self::on_language_select)
            .detach();

        let current_source = AppState::try_read(cx).map_or(AssetSourcePreference::Automatic, |s| {
            s.app_settings().asset_source
        });
        let source_options = assets::asset_source_options();
        let selected_source = assets::selected_source_index(current_source, &source_options);
        let asset_source_select =
            cx.new(|cx| SelectState::new(source_options, Some(selected_source), window, cx));
        cx.subscribe_in(&asset_source_select, window, Self::on_asset_source_select)
            .detach();

        let thumbwheel_sensitivity_slider = Self::thumbwheel_sensitivity_slider(window, cx);
        let vertical_scroll_sensitivity_slider =
            Self::vertical_scroll_sensitivity_slider(window, cx);

        // Poll the agent's live event monitor while this window is open. The task
        // is held in the view, so closing Settings drops it, polling stops, and
        // the agent disables monitoring on its own.
        #[cfg(all(target_os = "macos", debug_assertions))]
        let monitor_task = cx.spawn(async move |_view, cx| {
            loop {
                // Refresh the event-tap snapshot the Diagnostics page reads, so
                // its per-frame render works off this cache instead of issuing
                // CGGetEventTapList syscalls on every repaint.
                let taps = openlogi_hook::Hook::list_event_taps();
                let sender = cx.update(|cx| AppState::global(cx).read(cx).ipc_sender());
                let (tx, rx) = tokio::sync::oneshot::channel();
                let events = if sender
                    .send(crate::services::ipc::Command::PollEventMonitor(tx))
                    .is_ok()
                {
                    rx.await.unwrap_or_default()
                } else {
                    Vec::new()
                };
                cx.update(|cx| {
                    AppState::update(cx, |state, cx| {
                        state.set_event_taps(taps);
                        if !events.is_empty() {
                            state.push_monitor_events(events);
                        }
                        cx.emit(StateEvent::DiagnosticsChanged);
                    });
                });
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(300))
                    .await;
            }
        });

        Self {
            focus_handle,
            appearance_obs: None,
            _state_obs: state_obs,
            theme_filter: ThemeFilter::All,
            theme_search,
            initial_page,
            language_select,
            asset_source_select,
            thumbwheel_sensitivity_slider,
            vertical_scroll_sensitivity_slider,
            updater,
            updater_obs,
            copied: false,
            copied_gen: 0,
            asset_cache_desc: assets::cache_size_description(),
            registration_status: crate::platform::registration::status(),
            #[cfg(all(target_os = "macos", debug_assertions))]
            _monitor_task: monitor_task,
        }
    }

    fn thumbwheel_sensitivity_slider(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<SliderState> {
        let current = AppState::try_read(cx).map_or(ThumbwheelSensitivity::DEFAULT, |state| {
            state.app_settings().thumbwheel_sensitivity
        });
        let slider = cx.new(|_| {
            SliderState::new()
                .min(f32::from(ThumbwheelSensitivity::MIN))
                .max(f32::from(ThumbwheelSensitivity::MAX))
                .default_value(f32::from(current))
        });
        cx.subscribe_in(&slider, window, Self::on_thumbwheel_sensitivity_slider)
            .detach();
        slider
    }

    fn vertical_scroll_sensitivity_slider(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<SliderState> {
        let current = AppState::try_read(cx).map_or(VerticalScrollSensitivity::DEFAULT, |state| {
            state.app_settings().vertical_scroll_sensitivity
        });
        let slider = cx.new(|_| {
            SliderState::new()
                .min(f32::from(VerticalScrollSensitivity::MIN))
                .max(f32::from(VerticalScrollSensitivity::MAX))
                .default_value(f32::from(current))
        });
        cx.subscribe_in(&slider, window, Self::on_vertical_scroll_sensitivity_slider)
            .detach();
        slider
    }

    /// Commit the thumb-wheel sensitivity slider. The label tracks the live
    /// slider value on every `Change`; persistence happens once on `Release`.
    #[expect(
        clippy::unused_self,
        reason = "gpui subscription handlers must take &mut self"
    )]
    fn on_thumbwheel_sensitivity_slider(
        &mut self,
        _: &Entity<SliderState>,
        event: &SliderEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SliderEvent::Release(value) = event {
            let sensitivity = ThumbwheelSensitivity::from_rounded(value.start());
            AppState::update(cx, |state, cx| {
                state.set_thumbwheel_sensitivity(sensitivity);
                cx.emit(StateEvent::SettingsChanged);
            });
        }
        cx.notify();
    }

    /// Commit the vertical scroll sensitivity once the slider is released.
    #[expect(
        clippy::unused_self,
        reason = "gpui subscription handlers must take &mut self"
    )]
    fn on_vertical_scroll_sensitivity_slider(
        &mut self,
        slider: &Entity<SliderState>,
        event: &SliderEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SliderEvent::Release(value) = event {
            let sensitivity = VerticalScrollSensitivity::from_rounded(value.start());
            let committed = AppState::update(cx, |state, cx| {
                state.set_vertical_scroll_sensitivity(sensitivity);
                cx.emit(StateEvent::SettingsChanged);
                state.app_settings().vertical_scroll_sensitivity
            });
            // A failed write restores AppState's persisted configuration. Re-seat
            // this independently owned slider so it cannot keep presenting the
            // rejected value after that rollback.
            slider.update(cx, |slider, cx| {
                slider.set_value(f32::from(committed), window, cx);
            });
        }
        cx.notify();
    }

    fn on_language_select(
        &mut self,
        _: &Entity<SelectState<Vec<language::LanguageOption>>>,
        event: &SelectEvent<Vec<language::LanguageOption>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(_) = event;
        let language = self
            .language_select
            .read(cx)
            .selected_value()
            .copied()
            .filter(|code| !code.is_empty())
            .map(ToOwned::to_owned);

        AppState::update(cx, |state, cx| {
            state.set_language(language, cx);
            cx.emit(StateEvent::SettingsChanged);
        });
    }

    fn on_asset_source_select(
        &mut self,
        _: &Entity<SelectState<Vec<assets::AssetSourceOption>>>,
        event: &SelectEvent<Vec<assets::AssetSourceOption>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(_) = event;
        let source = self
            .asset_source_select
            .read(cx)
            .selected_value()
            .copied()
            .unwrap_or_default();
        let refresh = AppState::try_read(cx).is_some_and(|state| {
            state.app_settings().asset_source != source && state.app_settings().auto_download_assets
        });

        AppState::update(cx, |state, cx| {
            state.set_asset_source(source);
            cx.emit(StateEvent::SettingsChanged);
        });
        if refresh {
            assets::send_asset_command(cx, AssetCommand::Refresh);
        }
    }
}

impl AuxWindow for SettingsView {
    fn set_appearance_obs(&mut self, sub: Subscription) {
        self.appearance_obs = Some(sub);
    }
}

/// Open the Settings window on its default (General) page, or focus it if it's
/// already open.
pub fn open(cx: &mut App) {
    open_at(SettingsPage::General, cx);
}

/// Open the Settings window on a specific page, or focus it if it's already
/// open. The page only steers a *fresh* open — an already-open window is just
/// focused on whatever page it last showed (the Settings widget owns selection).
/// The window's native title — one definition for open and the live-language
/// retitle ([`windows::retitle_open`]), so the two cannot drift.
pub(crate) fn window_title() -> SharedString {
    tr!("Settings")
}

pub fn open_at(page: SettingsPage, cx: &mut App) {
    windows::open_or_focus(
        |reg| &mut reg.settings,
        window_title(),
        // Wide enough that the pages' custom rows keep slack under fonts wider
        // than the macOS system font (Segoe UI tipped the old 840 into
        // clipping the hero rows' trailing buttons on Windows).
        Size::new(px(920.), px(640.)),
        move |window, cx| SettingsView::new(page, window, cx),
        cx,
    );
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::apply_ui_scale(window, cx);
        crate::ui::components::localize_placeholder(
            &self.theme_search,
            tr!("Filter themes…"),
            window,
            cx,
        );
        let pal = theme::palette(cx);
        let view = cx.entity();
        // Only surface the Camera permission when a webcam is actually present,
        // so people without a Logitech camera are never asked for camera access.
        // Gated to the platforms that register the permission page below (macOS
        // consent is the AVFoundation gate; Windows has no such page).
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let has_camera = AppState::try_read(cx).is_some_and(AppState::has_camera);

        // Filled group boxes use the theme's content-surface token, keeping
        // settings groups distinct from the page without borrowing a control
        // colour for a large card.
        let settings = Settings::new("settings")
            .with_group_variant(GroupBoxVariant::Fill)
            .sidebar_width(px(210.))
            .default_selected_index(SelectIndex {
                page_ix: self.initial_page.index(),
                group_ix: None,
            })
            .page(general::general_page(
                self.vertical_scroll_sensitivity_slider.clone(),
                self.thumbwheel_sensitivity_slider.clone(),
                self.registration_status
                    == crate::platform::registration::ServiceStatus::RequiresApproval,
                AppState::try_read(cx).is_none_or(|state| state.app_settings().launch_at_login),
            ))
            .page(updates::updates_page(self.updater.clone()));
        // Registered only where grants exist to manage — see the `mod
        // permissions` cfg for why Windows skips it.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let settings = settings.page(permissions::permissions_page(has_camera));
        let settings = settings
            .page(appearance::appearance_page(
                view.clone(),
                self.theme_filter,
                self.theme_search.clone(),
                self.language_select.clone(),
            ))
            .page(assets::assets_page(
                view.clone(),
                self.asset_source_select.clone(),
                self.asset_cache_desc.clone(),
            ))
            .page(about::about_page(view, self.copied));
        // Surfaces competing macOS event taps (a pointer-lag cause) and, in debug
        // builds, the full tap list and a live event monitor. Appended after
        // About so [`SettingsPage::index`] stays platform-independent.
        #[cfg(target_os = "macos")]
        let settings = settings.page(diagnostics::diagnostics_page());

        div()
            .size_full()
            .relative()
            .bg(pal.page)
            .text_color(pal.text_primary)
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            // Linux only: a client-side titlebar as an absolute overlay (with
            // matching top padding) rather than a flex-column row — the
            // `Settings` sidebar uses `h_resizable` percentage sizing, which a
            // flex column would break. macOS / Windows keep their native titlebar.
            .when(cfg!(target_os = "linux"), |this| {
                this.pt(TITLE_BAR_HEIGHT).child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .child(windows::aux_title_bar(tr!("Settings"), cx)),
                )
            })
            .child(settings)
    }
}
