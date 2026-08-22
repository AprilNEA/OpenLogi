//! Lightweight GPUI host for the cursor-centred Actions Ring.
//!
//! This process is a pure IPC client. The agent owns HID++, session validation,
//! haptic output, and action execution; the overlay only renders the
//! agent-snapshotted actions and reports hover/activate/cancel interactions.

#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

// `t!` resolves against a backend the invoking crate must generate itself, so
// both binaries expand `i18n!` over the one catalog in `openlogi-ui` — the same
// crate this one already depends on for locale negotiation.
rust_i18n::i18n!("../openlogi-ui/locales", fallback = "en");

mod agent;
mod platform;
mod ring;
mod session;

use std::{sync::Arc, time::Instant};

use anyhow::Result;
use gpui::AppContext as _;
use tracing::{debug, warn};
use tracing_subscriber::EnvFilter;

use openlogi_core::action_ring::DISPLAY_LIFETIME;
use openlogi_ipc::ActionRingInvocation;

use crate::agent::{Ipc, OverlayCommand, spawn_ipc};
use crate::ring::{RingView, ring_window_options};
use crate::session::{ClickAwaySession, claim_the_role, spawn_click_away_dismissal};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("OPENLOGI_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    openlogi_ui::locale::activate(None);
    // Held for the whole run: dropping it hands the role to the replacement.
    let _tenancy = claim_the_role()?;
    let Ipc {
        mut invocations,
        commands,
    } = spawn_ipc();

    let mut app = gpui_platform::application().with_assets(openlogi_ui::action_icons::ActionIcons);
    app = app.with_quit_mode(gpui::QuitMode::Explicit);
    app.run(move |cx| {
        platform::configure_application();
        let live_session = Arc::new(ClickAwaySession::new());
        spawn_click_away_dismissal(cx, Arc::clone(&live_session));
        let warm_window = create_warm_window(cx, commands.clone(), Arc::clone(&live_session));

        cx.spawn(async move |cx| {
            while let Some(observed) = invocations.recv().await {
                if let Some(warm_window) = warm_window.as_ref() {
                    handle_warm_observation(cx, warm_window, observed, &commands, &live_session);
                } else {
                    handle_cold_observation(cx, observed, &commands, &live_session);
                }
            }
        })
        .detach();
    });
    Ok(())
}

/// Pre-create the Actions Ring native window once. Windows, macOS and X11 can
/// reuse it; unsupported backends keep the existing create/destroy path.
fn create_warm_window(
    cx: &mut gpui::App,
    commands: tokio::sync::mpsc::UnboundedSender<OverlayCommand>,
    live_session: Arc<ClickAwaySession>,
) -> Option<gpui::WindowHandle<RingView>> {
    let options = ring_window_options(cx, true);
    let handle = match cx.open_window(options, |_, cx| {
        cx.new(|_| RingView::idle(commands, live_session))
    }) {
        Ok(handle) => handle,
        Err(error) => {
            warn!(%error, "could not pre-create Actions Ring window; using cold overlay path");
            return None;
        }
    };
    platform::configure_windows();

    let reusable = handle
        .update(cx, |_, window, _| {
            platform::supports_warm_window(window) && platform::hide_window(window)
        })
        .unwrap_or(false);
    if reusable {
        debug!("Actions Ring native window warmed and waiting hidden");
        Some(handle)
    } else {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        debug!("Actions Ring backend requires cold window creation");
        None
    }
}

fn handle_warm_observation(
    cx: &mut gpui::AsyncApp,
    warm_window: &gpui::WindowHandle<RingView>,
    observed: Option<ActionRingInvocation>,
    commands: &tokio::sync::mpsc::UnboundedSender<OverlayCommand>,
    live_session: &Arc<ClickAwaySession>,
) {
    let Some(invocation) = observed else {
        cx.update(|cx| {
            let _ = warm_window.update(cx, |view, window, cx| {
                if let Some(open_session) = view.current_session() {
                    view.dismiss(open_session, window, cx);
                } else {
                    live_session.clear();
                    if !platform::hide_window(window) {
                        warn!("could not hide warm Actions Ring window");
                    }
                }
            });
        });
        return;
    };

    openlogi_ui::locale::activate(invocation.language.as_deref());
    let session_id = invocation.session_id;
    let timeout_commands = commands.clone();
    let show_started = Instant::now();
    let shown = cx.update(|cx| {
        warm_window
            .update(cx, |view, window, cx| {
                view.install(invocation, cx);
                window.refresh();
                if platform::show_window_at_cursor(window) {
                    live_session.set(session_id);
                    true
                } else {
                    warn!("could not position/show warm Actions Ring window");
                    view.dismiss(session_id, window, cx);
                    false
                }
            })
            .unwrap_or(false)
    });

    if !shown {
        let _ = commands.send(OverlayCommand::Cancel { session_id });
        return;
    }
    debug!(
        session_id,
        elapsed = ?show_started.elapsed(),
        "Actions Ring warm window shown"
    );

    let timeout_window = *warm_window;
    cx.spawn(async move |cx| {
        cx.background_executor().timer(DISPLAY_LIFETIME).await;
        let dismissed = timeout_window
            .update(cx, |view, window, cx| view.dismiss(session_id, window, cx))
            .unwrap_or(false);
        if dismissed {
            let _ = timeout_commands.send(OverlayCommand::Cancel { session_id });
        }
    })
    .detach();
}

fn handle_cold_observation(
    cx: &mut gpui::AsyncApp,
    observed: Option<ActionRingInvocation>,
    commands: &tokio::sync::mpsc::UnboundedSender<OverlayCommand>,
    live_session: &Arc<ClickAwaySession>,
) {
    let Some(invocation) = observed else {
        cx.update(|cx| {
            live_session.clear();
            for handle in cx.windows() {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
        });
        return;
    };

    openlogi_ui::locale::activate(invocation.language.as_deref());
    let commands = commands.clone();
    let timeout_commands = commands.clone();
    let live_session = Arc::clone(live_session);
    cx.update(|cx| {
        let previous_windows = cx.windows();
        let options = ring_window_options(cx, true);
        let session_id = invocation.session_id;
        match cx.open_window(options, |_, cx| {
            cx.new(|_| RingView::new(invocation, commands, Arc::clone(&live_session)))
        }) {
            Ok(handle) => {
                let _ = handle.update(cx, |_, window, _| platform::apply_circular_shape(window));
                live_session.set(session_id);
                for previous in previous_windows {
                    let _ = previous.update(cx, |_, window, _| window.remove_window());
                }
                platform::configure_windows();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(DISPLAY_LIFETIME).await;
                    let dismissed = handle
                        .update(cx, |view, window, cx| view.dismiss(session_id, window, cx))
                        .unwrap_or(false);
                    if dismissed {
                        let _ = timeout_commands.send(OverlayCommand::Cancel { session_id });
                    }
                })
                .detach();
            }
            Err(error) => warn!(%error, "could not open Actions Ring window"),
        }
    });
}

#[cfg(test)]
mod tests {

    /// The catalog this binary translates against lives in `openlogi-ui` and is
    /// reached by the relative path in the `i18n!` at the top. A wrong path
    /// there does **not** fail the build — `rust_i18n` compiles it to an empty
    /// catalog, and every ring label silently renders as its English key in all
    /// 20 locales. Pin one action label in a non-English locale so that
    /// breakage is loud.
    #[test]
    fn the_shared_catalog_is_wired_up() {
        rust_i18n::set_locale("zh-CN");
        assert_eq!(rust_i18n::t!("Left Click"), "左键单击");
        rust_i18n::set_locale("en");
    }
}
