//! OpenLogi's desktop app: process bootstrap.
//!
//! Only the order the process has to start in lives here — logging, the
//! single-instance guard, config, the UI locale, then the IPC client to the
//! agent that owns every device. Everything past the GPUI `run` call belongs
//! to [`runtime`], which owns the event loop and the state outliving any one
//! event, and to [`windows`], which owns the windows themselves.

// Without this Windows runs the exe as a console app and pops a terminal
// window behind the UI. Debug builds keep the console so logs stay visible.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

/// Translate `key` (an English msgid) to the current locale and wrap it as a
/// [`gpui::SharedString`], ready for `.child(...)` / `.label(...)` / menu items.
/// Forwards `rust_i18n` interpolation, e.g. `tr!("Bind %{name}", name => x)`.
///
/// Defined before the `mod` declarations so every submodule can use it without
/// an import (textual macro scope). Pairs with the `rust_i18n::i18n!` below.
macro_rules! tr {
    ($($args:tt)*) => {
        // `t!` yields `Cow<'static, str>`. A borrowed hit — the common case: a
        // found translation or the English-key fallback — wraps into a
        // `SharedString` with no copy; only owned (interpolated) results allocate.
        match ::rust_i18n::t!($($args)*) {
            ::std::borrow::Cow::Borrowed(s) => ::gpui::SharedString::from(s),
            ::std::borrow::Cow::Owned(s) => ::gpui::SharedString::from(s),
        }
    };
}

mod app;
mod app_assets;
mod features;
mod platform;
mod runtime;
mod services;
mod state;
mod ui;
mod windows;

// Loads the Crowdin-managed `crates/openlogi-ui/locales/*.yml` files at compile
// time and generates the `t!`/`tr!` lookup backend for this crate. `fallback =
// "en"` matches the codes gpui-component ships, so the framework's own widgets
// localize alongside ours.
rust_i18n::i18n!("../openlogi-ui/locales", fallback = "en");

use anyhow::Result;
use openlogi_core::brand::DeeplinkCommand;
use openlogi_core::config::{Config, ConfigFile};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::services::assets::sync::{AssetCommand, AssetControl};
use crate::services::{i18n, ipc};
use crate::state::ConfigPersistence;
use crate::ui::theme;

fn main() -> Result<()> {
    init_tracing();

    let _guard = match openlogi_core::single_instance::acquire("openlogi.lock") {
        Ok(g) => g,
        Err(openlogi_core::single_instance::InstanceError::AlreadyRunning { path }) => {
            info!(
                path = %path.display(),
                "another OpenLogi instance is already running — exiting"
            );
            return Ok(());
        }
        Err(e) => return Err(anyhow::Error::from(e).context("single-instance check")),
    };

    let (initial_config, config_persistence) = match ConfigFile::load_or_default() {
        Ok((config, file)) => (config, ConfigPersistence::UserFile(file)),
        Err(error) => {
            warn!(error = %error, "could not load config.toml; disabling config writes");
            (
                Config::ephemeral(),
                ConfigPersistence::ReadOnly(error.to_string()),
            )
        }
    };

    // Resolve the UI locale before any menu or window is built so the first
    // frame already renders in the right language.
    i18n::apply(&initial_config.app_settings);

    // The always-on agent owns the hook, the HID++ capture, and all device I/O.
    // The GUI is a client: it observes inventory + status and forwards device
    // commands over IPC. Started here so the first state is already on its way.
    let ipc::IpcClient {
        updates,
        commands: ipc_commands,
    } = ipc::spawn();

    // Manual asset actions (Settings → Assets): Refresh / Clear cache. The
    // sender is published as a global so the Settings window can drive the
    // sync that lives on the event loop; the loop keeps a second sender to
    // re-issue a command it had to defer while a sync was in flight.
    let (asset_ctrl_tx, asset_commands) = tokio::sync::mpsc::unbounded_channel::<AssetCommand>();
    let asset_self_tx = asset_ctrl_tx.clone();

    // `with_assets` registers the embedded app logo
    // ([`app_assets`]) plus the lucide SVGs that back
    // `gpui_component::IconName`; without it `img()` / `Icon` would fail to load.
    let app = gpui_platform::application().with_assets(app_assets::AppAssets);

    // URL scheme: `open openlogi://open-settings` from the agent's tray or
    // external apps. Works for both cold start (macOS launches the app then
    // delivers the URL) and warm reactivation (delivered to the running app).
    let (deeplink_tx, deeplinks) = tokio::sync::mpsc::unbounded_channel::<DeeplinkCommand>();
    app.on_open_urls(move |urls| {
        for url in &urls {
            if let Some(cmd) = DeeplinkCommand::parse_url(url) {
                let _ = deeplink_tx.send(cmd);
            } else {
                warn!(url, "unknown openlogi:// command — ignoring");
            }
        }
    });

    // Reopen the window when the app is relaunched with none open (dock click).
    app.on_reopen(|cx| windows::main_window::open(&[], cx));

    app.run(move |cx| {
        gpui_component::init(cx);
        theme::register_builtin_themes(cx);
        app::menu::install(cx);

        // Seed the Add Device window's initial state. Its buttons only send
        // intents; the session itself is the agent's, and every observation
        // republishes it into this global.
        cx.set_global(windows::add_device::PairingUi::Idle);

        // The Settings → Assets buttons drive the asset sync (which lives on
        // the event loop) through this global.
        cx.set_global(AssetControl(asset_ctrl_tx));

        // Publish the shared updater and, if the user opted in, run one
        // check on launch. Done before `initial_config` is handed to the
        // event loop below.
        platform::updater::install(cx, &initial_config.app_settings);

        // Wear the icon the user picked. An update replaces the bundle and
        // takes the icon with it, so this is a repair as much as a restore.
        platform::app_icon::restore(initial_config.app_settings.app_icon);

        // On-demand GUI: quit when the last window closes. The agent stays
        // resident and keeps remapping (and hosts the menu-bar item from which
        // the GUI is reopened), so nothing needs the GUI process to linger.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        runtime::spawn(
            runtime::Startup {
                config: initial_config,
                persistence: config_persistence,
                ipc_commands,
                updates,
                asset_commands,
                asset_self_tx,
                deeplinks,
            },
            cx,
        );
    });

    Ok(())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("OPENLOGI_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}
