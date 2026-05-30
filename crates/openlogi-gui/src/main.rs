//! OpenLogi GPUI desktop window.
//!
//! Initial HID++ inventory is collected synchronously on startup (GPUI owns
//! the main thread, so we can't move it onto a tokio runtime). Live polling
//! lands when there's something to react to.

mod about_window;
mod accessibility_watcher;
mod app;
mod app_menu;
mod app_watcher;
mod asset;
mod components;
mod data;
mod hardware;
mod inventory_watcher;
mod launch_agent;
mod mouse_model;
mod settings_window;
mod single_instance;
mod state;
mod theme;
mod updater;
mod windows;

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// Shared binding map threaded between `AppState` and the hook callback.
type BindingMap = Arc<RwLock<BTreeMap<ButtonId, Action>>>;

use anyhow::{Context as _, Result};
use gpui::{
    AppContext, BorrowAppContext as _, Bounds, SharedString, Size, Styled, TitlebarOptions,
    WindowBounds, WindowOptions, px,
};
use gpui_component::{ActiveTheme, Root, Theme, ThemeMode};
use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::config::Config;
use openlogi_core::device::{DeviceInventory, DeviceModelInfo};
use openlogi_hook::{EventDisposition, Hook, MouseEvent};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::app::AppView;
use crate::hardware::{toggle_smartshift_in_background, write_dpi_in_background};
use crate::state::{AppState, DpiCycleState};

#[allow(
    clippy::too_many_lines,
    reason = "top-level startup orchestration (single-instance, config, asset sync, \
              watchers, window, drain loop); splitting would scatter tightly-coupled \
              setup across helpers that each take most of these locals"
)]
fn main() -> Result<()> {
    init_tracing();

    let _guard = match single_instance::acquire() {
        Ok(g) => g,
        Err(single_instance::InstanceError::AlreadyRunning { path }) => {
            info!(
                path = %path.display(),
                "another OpenLogi instance is already running — exiting"
            );
            return Ok(());
        }
        Err(e) => return Err(anyhow::Error::from(e).context("single-instance check")),
    };

    let early_config = Config::load_or_default().ok();
    if let Some(cfg) = early_config.as_ref() {
        launch_agent::reconcile(cfg.app_settings.launch_at_login);
        updater::maybe_check(&cfg.app_settings);
    }

    let inventories = enumerate_blocking().context("HID enumeration failed")?;

    let probe_cache = asset::AssetResolver::new();
    if asset::sync::should_run(probe_cache.has_bundle_root()) {
        let server = std::env::var("OPENLOGI_ASSETS")
            .unwrap_or_else(|_| asset::sync::DEFAULT_BASE.to_string());
        let models = collect_models(&inventories);
        if let Err(e) = asset::sync::sync(&server, &models) {
            warn!(error = ?e, "asset sync raised — continuing with whatever's cached");
        }
    }
    drop(probe_cache);

    let (hook_bindings, dpi_cycle, initial_config) = load_config_and_bindings(&inventories);
    let hook_arcs = (Arc::clone(&hook_bindings), Arc::clone(&dpi_cycle));

    let mut inventory_rx = inventory_watcher::spawn(std::time::Duration::from_secs(2));
    let mut app_rx = app_watcher::spawn(std::time::Duration::from_secs(1));
    let mut accessibility_rx = accessibility_watcher::spawn(std::time::Duration::from_millis(1200));

    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        app_menu::install(cx);

        if !Hook::has_accessibility() {
            Hook::prompt_accessibility();
        }

        cx.spawn(async move |cx| {
            let bounds = cx.update(|cx| Bounds::centered(None, Size::new(px(1100.), px(750.)), cx));
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(Size::new(px(720.), px(520.))),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("OpenLogi")),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                ..WindowOptions::default()
            };

            #[allow(
                clippy::expect_used,
                reason = "failure to open the main window is fatal; nothing useful to recover to"
            )]
            cx.open_window(options, move |window, cx| {
                if !cx.has_global::<AppState>() {
                    let cache = asset::AssetResolver::new();
                    cx.set_global(AppState::with_runtime_shared(
                        initial_config,
                        &inventories,
                        &cache,
                        hook_bindings,
                        dpi_cycle,
                    ));
                }
                Theme::change(ThemeMode::from(window.appearance()), Some(window), cx);

                let view = cx.new(|cx| AppView::new(&inventories, cx));

                let appearance_obs = window.observe_window_appearance(|window, cx| {
                    Theme::change(ThemeMode::from(window.appearance()), Some(window), cx);
                });
                view.update(cx, |v, _| v.set_appearance_obs(appearance_obs));

                cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
            })
            .expect("opening the main window should not fail");

            let mut hook_handle = None;
            loop {
                tokio::select! {
                    Some(new_inv) = inventory_rx.recv() => {
                        cx.update(|cx| {
                            let cache = asset::AssetResolver::new();
                            cx.update_global::<AppState, _>(|state, _| {
                                state.refresh_inventories(&new_inv, &cache);
                            });
                        });
                    }
                    Some(bundle) = app_rx.recv() => {
                        cx.update(|cx| {
                            cx.update_global::<AppState, _>(|state, _| {
                                state.set_current_app(bundle);
                            });
                        });
                    }
                    Some(granted) = accessibility_rx.recv() => {
                        if !granted {
                            hook_handle = None;
                        }
                        cx.update(|cx| {
                            if cx.has_global::<AppState>() {
                                cx.update_global::<AppState, _>(|state, _| {
                                    state.accessibility_granted = granted;
                                });
                            }
                            cx.refresh_windows();
                        });
                        if granted && hook_handle.is_none() {
                            info!("accessibility granted — installing OS mouse hook");
                            hook_handle =
                                start_hook(Arc::clone(&hook_arcs.0), Arc::clone(&hook_arcs.1));
                        }
                    }
                    else => break,
                }
            }
        })
        .detach();
    });

    Ok(())
}

/// Load config from disk and build the initial hook-shared state using the
/// same selection and binding rules as [`AppState::with_runtime_shared`].
/// Pre-populating both `Arc`s here means the hook callback sees the right
/// bindings *and* DPI presets from the very first event, well before the GPUI
/// global is installed.
fn load_config_and_bindings(
    inventories: &[DeviceInventory],
) -> (BindingMap, Arc<RwLock<DpiCycleState>>, Config) {
    let config = match Config::load_or_default() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "could not load config.toml; using default bindings");
            Config::default()
        }
    };

    let cache = asset::AssetResolver::new();
    let (bindings, dpi_cycle) = AppState::initial_hook_state(&config, inventories, &cache);
    let bindings_arc = Arc::new(RwLock::new(bindings));
    let dpi_cycle_arc = Arc::new(RwLock::new(dpi_cycle));

    (bindings_arc, dpi_cycle_arc, config)
}

/// Attempt to start the OS hook. Returns `None` if Accessibility is not
/// granted or on an unsupported platform — the app continues without crashing.
fn start_hook(bindings: BindingMap, dpi_cycle: Arc<RwLock<DpiCycleState>>) -> Option<Hook> {
    if !Hook::has_accessibility() {
        warn!(
            "Accessibility not granted — events will not be captured. \
             Open System Settings → Privacy & Security → Accessibility."
        );
        return None;
    }

    let result = Hook::start(move |event| match event {
        MouseEvent::Button { id, pressed } => {
            let owned = matches!(id, ButtonId::Back | ButtonId::Forward);
            if !owned {
                return EventDisposition::PassThrough;
            }
            if pressed {
                let action = bindings.read().ok().and_then(|g| g.get(&id).cloned());
                if let Some(action) = action {
                    info!(button = %id, action = %action.label(), "button → executing bound action");
                    dispatch_action(&action, &dpi_cycle);
                } else {
                    info!(button = %id, "button pressed with no binding — suppressed");
                }
            }
            EventDisposition::Suppress
        }
        MouseEvent::Scroll { .. } => EventDisposition::PassThrough,
    });

    match result {
        Ok(hook) => {
            info!("OS mouse hook installed");
            Some(hook)
        }
        Err(e) => {
            warn!(error = %e, "could not install OS mouse hook — events will not be captured");
            None
        }
    }
}

/// Route a bound action either to OS-level event synthesis
/// ([`Action::execute`]) or to one of OpenLogi's hardware-side handlers
/// (currently just DPI cycling).
///
/// `dpi_cycle` is held across a write lock long enough to advance the index
/// and snapshot the new DPI + target; the actual HID write spawns its own
/// thread via [`write_dpi_in_background`] to keep the hook callback
/// non-blocking.
fn dispatch_action(action: &Action, dpi_cycle: &Arc<RwLock<DpiCycleState>>) {
    let next = match action {
        Action::CycleDpiPresets => match dpi_cycle.write() {
            Ok(mut guard) => guard.cycle(),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — cycle skipped");
                None
            }
        },
        Action::SetDpiPreset(i) => match dpi_cycle.write() {
            Ok(mut guard) => guard.set(usize::from(*i)),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — set skipped");
                None
            }
        },
        Action::ToggleSmartShift => {
            let target = dpi_cycle.read().ok().and_then(|g| g.target.clone());
            info!("SmartShift toggle → flipping wheel mode");
            toggle_smartshift_in_background(target);
            return;
        }
        other => {
            other.execute();
            None
        }
    };
    if let Some((dpi, target)) = next {
        info!(dpi, "DPI action → writing to device");
        write_dpi_in_background(target, dpi);
    } else if matches!(action, Action::CycleDpiPresets | Action::SetDpiPreset(_)) {
        info!(
            action = %action.label(),
            "no DPI presets configured for active device — press ignored"
        );
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("OPENLOGI_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

fn enumerate_blocking() -> Result<Vec<DeviceInventory>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime init")?;
    rt.block_on(openlogi_hid::enumerate())
        .context("openlogi_hid::enumerate")
}

/// Flatten every paired device's HID++ model snapshot — that's what the
/// asset sync feeds into the registry lookup.
fn collect_models(inventories: &[DeviceInventory]) -> Vec<DeviceModelInfo> {
    inventories
        .iter()
        .flat_map(|inv| inv.paired.iter())
        .filter_map(|p| p.model_info)
        .collect()
}
