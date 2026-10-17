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

mod ipc;
mod platform;
mod ring;
mod session;

use std::sync::Arc;

use anyhow::Result;
use gpui::AppContext as _;
use tracing::warn;

use openlogi_core::action_ring::DISPLAY_LIFETIME;

use crate::ipc::OverlayCommand;
use crate::platform::RingPlacement;
use crate::ring::RingView;
use crate::session::{
    ClickAwaySession, claim_the_role, close_ring_windows, spawn_click_away_dismissal,
};

fn main() -> Result<()> {
    openlogi_core::logging::init_stderr();

    openlogi_core::locale::activate(None);
    // Held for the whole run: dropping it hands the role to the replacement.
    let _tenancy = claim_the_role()?;
    let ipc::Handle {
        mut invocations,
        commands,
    } = ipc::spawn();

    let mut app = gpui_platform::application().with_assets(openlogi_ui::action_icons::ActionIcons);
    // Application construction establishes GPUI's NSApplication subclass.
    // Asking AppKit for its singleton sooner creates a plain NSApplication,
    // which lacks the `platform` ivar GPUI needs. Set Accessory policy now:
    // GPUI owns the singleton, but `app.run` has not finished launching yet.
    platform::configure_application();
    app = app.with_quit_mode(gpui::QuitMode::Explicit);
    app.run(move |cx| {
        // GPUI's didFinishLaunching callback resets the policy to Regular.
        // Reapply the helper policy after that reset, without constructing
        // NSApplication before GPUI has installed its subclass.
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
                    #[cfg_attr(
                        windows,
                        expect(
                            unused_mut,
                            reason = "only the Wayland placement drains its openings"
                        )
                    )]
                    let mut placement = match RingPlacement::capture(cx) {
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
                    // Wayland opens one window per display. One that fails to
                    // open must not sink the others.
                    let mut primary = None;
                    for (options, panel) in placement.openings() {
                        let invocation = invocation.clone();
                        let commands = commands.clone();
                        match cx.open_window(options, |_, cx| {
                            cx.new(|_| RingView::new(invocation, commands, &live_session, panel))
                        }) {
                            Ok(handle) => {
                                if primary.is_none() {
                                    primary = Some(handle);
                                }
                            }
                            Err(error) => warn!(%error, "could not open Actions Ring window"),
                        }
                    }
                    let Some(primary) = primary else {
                        return;
                    };
                    if let Err(error) = primary
                        .update(cx, |_, window, cx| {
                            // Native placement resizes the window inside this
                            // update, where GPUI's own resize/move callbacks
                            // cannot borrow the app. Resync its viewport and
                            // scale, or it keeps rendering at the bootstrap size.
                            placement.show(window).map(|()| window.bounds_changed(cx))
                        })
                        .and_then(std::convert::identity)
                    {
                        warn!(%error, "could not position Actions Ring window");
                        close_ring_windows(cx, session_id);
                        let _ = timeout_commands.send(OverlayCommand::Cancel { session_id });
                        return;
                    }
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(DISPLAY_LIFETIME).await;
                        if cx.update(|cx| close_ring_windows(cx, session_id)) {
                            let _ = timeout_commands.send(OverlayCommand::Cancel { session_id });
                        }
                    })
                    .detach();
                });
            }
        })
        .detach();
    });
    Ok(())
}

#[cfg(test)]
mod tests;
