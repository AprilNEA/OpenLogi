//! `openlogi://` command dispatch.
//!
//! The agent's tray and external apps drive the GUI through the URL scheme
//! rather than IPC, so every command here has to work both cold (macOS
//! launches the app, then delivers the URL) and warm (delivered to the running
//! app).

use openlogi_core::brand::DeeplinkCommand;

use crate::app;
use crate::windows;

/// Run one `openlogi://` command.
pub fn dispatch(command: DeeplinkCommand, cx: &mut gpui::App) {
    use DeeplinkCommand as Cmd;
    match command {
        // The tray sends Quit right before the agent exits: flag the IPC
        // client first, or its unreachable→spawn reflex can resurrect the
        // agent while this process is still tearing down (observed live —
        // Quit, then the agent back four seconds later).
        Cmd::Quit => {
            crate::services::ipc::mark_suite_quitting();
            cx.quit();
        }
        // Always route Show through `main_window::open`: it re-focuses (and
        // deminiaturizes) an existing window or opens a fresh one, so the tray's
        // "Show Main Window" works whether or not a window is already up.
        Cmd::Show => windows::main_window::open(&[], cx),
        // The aux windows are standalone; open the main window first as the
        // session anchor (no-op when one is already open) so closing the aux
        // window doesn't leave the app windowless — and quitting — by surprise.
        Cmd::OpenSettings => {
            windows::main_window::ensure(cx);
            windows::settings::open(cx);
        }
        Cmd::OpenAbout => {
            windows::main_window::ensure(cx);
            windows::settings::open_at(windows::settings::SettingsPage::About, cx);
        }
        Cmd::CheckForUpdates => {
            windows::main_window::ensure(cx);
            app::menu::check_for_updates(cx);
        }
    }
}
