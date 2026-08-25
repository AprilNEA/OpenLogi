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

use std::sync::Arc;

use anyhow::Result;
use gpui::AppContext as _;
use tracing::warn;
use tracing_subscriber::EnvFilter;

use openlogi_core::action_ring::DISPLAY_LIFETIME;

use crate::agent::{Ipc, OverlayCommand, spawn_ipc};
use crate::ring::{RingView, ring_windows};
use crate::session::{
    ClickAwaySession, claim_the_role, close_ring_windows, spawn_click_away_dismissal,
};

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
        cx.spawn(async move |cx| {
            while let Some(observed) = invocations.recv().await {
                // No ring is what a dismissal looks like: close whatever is
                // showing and open nothing. The agent has already forgotten the
                // session, so there is nothing to acknowledge either.
                let Some(invocation) = observed else {
                    cx.update(|cx| {
                        for handle in cx.windows() {
                            let _ = handle.update(cx, |_, window, _| window.remove_window());
                        }
                    });
                    continue;
                };
                openlogi_ui::locale::activate(invocation.language.as_deref());
                cx.update(|cx| {
                    live_session.clear();
                    for handle in cx.windows() {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    }
                    let timeout_commands = commands.clone();
                    let session_id = invocation.session_id;
                    // Wayland needs one window per display (see `ring_windows`),
                    // so a ring is a set of windows, and one that fails to open
                    // must not sink the others: the ring is showing as long as
                    // any of them made it.
                    let mut opened = false;
                    for (options, placement) in ring_windows(cx) {
                        let invocation = invocation.clone();
                        let commands = commands.clone();
                        match cx.open_window(options, |_, cx| {
                            cx.new(|_| RingView::new(invocation, commands, placement))
                        }) {
                            Ok(_) => opened = true,
                            Err(error) => warn!(%error, "could not open Actions Ring window"),
                        }
                    }
                    if opened {
                        live_session.set(session_id);
                        platform::configure_windows();
                        cx.spawn(async move |cx| {
                            cx.background_executor().timer(DISPLAY_LIFETIME).await;
                            if cx.update(|cx| close_ring_windows(cx, session_id)) {
                                let _ =
                                    timeout_commands.send(OverlayCommand::Cancel { session_id });
                            }
                        })
                        .detach();
                    }
                });
            }
        })
        .detach();
    });
    Ok(())
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
