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

mod backlight;
mod ipc;
mod platform;
mod ring;
mod session;

use std::sync::Arc;

use anyhow::Result;
use gpui::AppContext as _;
use tracing::warn;

use openlogi_core::action_ring::DISPLAY_LIFETIME;

use crate::backlight::BacklightView;
use crate::ipc::OverlayCommand;
use crate::platform::RingPlacement;
use crate::ring::RingView;
use crate::session::{ClickAwaySession, claim_the_role, spawn_click_away_dismissal};

#[expect(
    clippy::too_many_lines,
    reason = "the overlay bootstraps two independent IPC observation tasks"
)]
fn main() -> Result<()> {
    openlogi_core::logging::init_stderr();

    openlogi_core::locale::activate(None);
    // Held for the whole run: dropping it hands the role to the replacement.
    let _tenancy = claim_the_role()?;
    let ipc::Handle {
        mut invocations,
        mut backlights,
        commands,
    } = ipc::spawn();

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
                openlogi_core::locale::activate(invocation.language.as_deref());
                cx.update(|cx| {
                    for handle in cx.windows() {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    }
                    let placement = match RingPlacement::capture(cx) {
                        Ok(placement) => placement,
                        Err(error) => {
                            warn!(%error, "could not locate Actions Ring display");
                            let _ = commands.send(OverlayCommand::Cancel {
                                session_id: invocation.session_id,
                            });
                            return;
                        }
                    };
                    let commands = commands.clone();
                    let timeout_commands = commands.clone();
                    let session_id = invocation.session_id;
                    match cx.open_window(placement.window_options(), |_, cx| {
                        cx.new(|_| RingView::new(invocation, commands, &live_session))
                    }) {
                        Ok(handle) => {
                            if let Err(error) = handle
                                .update(cx, |_, window, _| placement.show(window))
                                .and_then(std::convert::identity)
                            {
                                warn!(%error, "could not position Actions Ring window");
                                let _ = handle.update(cx, |_, window, _| window.remove_window());
                                let _ =
                                    timeout_commands.send(OverlayCommand::Cancel { session_id });
                                return;
                            }
                            cx.spawn(async move |cx| {
                                cx.background_executor().timer(DISPLAY_LIFETIME).await;
                                if handle
                                    .update(cx, |_, window, _| window.remove_window())
                                    .is_ok()
                                {
                                    let _ = timeout_commands
                                        .send(OverlayCommand::Cancel { session_id });
                                }
                            })
                            .detach();
                        }
                        Err(error) => warn!(%error, "could not open Actions Ring window"),
                    }
                });
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            while let Some(observation) = backlights.recv().await {
                cx.update(|cx| {
                    for handle in cx.windows() {
                        if let Some(view) = handle.downcast::<BacklightView>() {
                            let _ = view.update(cx, |view, _, _| {
                                view.observation = observation.clone();
                            });
                        }
                    }
                    if !observation.visible {
                        for handle in cx.windows() {
                            if handle.downcast::<BacklightView>().is_some() {
                                let _ = handle.update(cx, |_, window, _| window.remove_window());
                            }
                        }
                        return;
                    }
                    if cx
                        .windows()
                        .iter()
                        .all(|handle| handle.downcast::<BacklightView>().is_none())
                    {
                        let _ = cx.open_window(backlight::window_options(), |_, cx| {
                            cx.new(|_| BacklightView::new(observation.clone()))
                        });
                    }
                });
            }
        })
        .detach();
    });
    Ok(())
}

#[cfg(test)]
mod tests;
