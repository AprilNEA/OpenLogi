//! `openlogi://` command dispatch.
//!
//! The agent's tray and external apps drive the GUI through the URL scheme
//! rather than IPC, so every command here has to work both cold (macOS
//! launches the app, then delivers the URL) and warm (delivered to the running
//! app).

use gpui::App;
use openlogi_core::brand::{DeeplinkCommand, DeviceLink};
use openlogi_core::device::DeviceKind;
use openlogi_ipc::InventoryHealth;
use tracing::warn;

use super::DetailTab;
use crate::app;
use crate::state::{AppState, DeviceRecord};
use crate::windows;

#[cfg(test)]
mod tests;

/// Runs `openlogi://` commands, and holds the one link that names a page the
/// device list cannot show yet.
#[derive(Default)]
pub(crate) struct Deeplinks {
    /// A device or profile link that arrived before the device it names: a
    /// cold launch delivers its URL before the agent's first complete
    /// inventory. Any later command replaces it, so a late inventory never
    /// pulls the window away from what the user opened since.
    pending: Option<Page>,
}

/// A main-window page that a link names through the device list.
#[derive(Debug)]
enum Page {
    /// A device's detail page.
    Device(DeviceLink),
    /// The selected device's profile for an application, by
    /// [`ForegroundApp::id`](openlogi_core::app::ForegroundApp::id).
    AppProfile(String),
    /// A camera's page on `tab`, by the camera's settings key.
    Camera { key: String, tab: DetailTab },
}

impl Deeplinks {
    /// Run one `openlogi://` command.
    pub(crate) fn dispatch(&mut self, command: DeeplinkCommand, cx: &mut App) {
        use DeeplinkCommand as Cmd;
        self.pending = None;
        match command {
            // Flag the IPC client before quitting, or its unreachable→spawn
            // reflex can resurrect the agent mid-teardown (observed live).
            Cmd::Quit => {
                crate::services::ipc::mark_suite_quitting();
                cx.quit();
            }
            // Always route Show through `main_window::open`: it re-focuses (and
            // deminiaturizes) an existing window or opens a fresh one, so the tray's
            // "Show Main Window" works whether or not a window is already up.
            Cmd::Show => windows::main_window::open(cx),
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
            Cmd::AddDevice => {
                windows::main_window::ensure(cx);
                windows::add_device::open(cx);
            }
            Cmd::OpenFlow => {
                windows::main_window::ensure(cx);
                windows::flow::open(cx);
            }
            Cmd::OpenDevice(device) => self.open(Page::Device(device), cx),
            Cmd::OpenAppProfile(app) => self.open(Page::AppProfile(app), cx),
            Cmd::OpenCamera(key) => self.open(
                Page::Camera {
                    key,
                    tab: DetailTab::Device,
                },
                cx,
            ),
            Cmd::PreviewCamera(key) => self.open(
                Page::Camera {
                    key,
                    tab: DetailTab::Camera,
                },
                cx,
            ),
        }
    }

    /// Try the waiting link again against a device list a snapshot has just
    /// refreshed. Once the agent's inventory is complete this is its last try.
    pub(crate) fn retry(&mut self, cx: &mut App) {
        if let Some(page) = self.pending.take()
            && !page.show(cx)
        {
            self.wait_or_drop(page, cx);
        }
    }

    fn open(&mut self, page: Page, cx: &mut App) {
        if !page.show(cx) {
            // The click still brings the app forward while its page waits for
            // the device list, or turns out not to be in it.
            windows::main_window::open(cx);
            self.wait_or_drop(page, cx);
        }
    }

    fn wait_or_drop(&mut self, page: Page, cx: &App) {
        if inventory_complete(cx) {
            warn!(?page, "deeplink names nothing in the device list");
        } else {
            self.pending = Some(page);
        }
    }
}

impl Page {
    /// Show this page in the main window, or return `false` without touching
    /// any window while the device list has nothing it names.
    fn show(&self, cx: &mut App) -> bool {
        let Some(state) = AppState::try_read(cx) else {
            return false;
        };
        match self {
            Self::Device(device) => {
                let Some(record_key) = state
                    .devices()
                    .iter()
                    .find(|record| names(record, device))
                    .map(DeviceRecord::record_key)
                else {
                    return false;
                };
                windows::main_window::navigate(cx, |view, cx| view.open_device(record_key, cx));
            }
            Self::AppProfile(app) => {
                let Some((record_key, tab)) = profile_page(state) else {
                    return false;
                };
                windows::main_window::navigate(cx, |view, cx| {
                    view.open_device(record_key, cx);
                    view.active_tab = tab;
                });
                AppState::apply(cx, |state| state.open_app_profile(app));
            }
            Self::Camera { key, tab } => {
                // Two serial-less units of one model share a key; the link
                // opens the first, as the gallery lists them.
                let Some(record_key) = state
                    .devices()
                    .iter()
                    .find(|record| record.kind == DeviceKind::Camera && record.config_key == *key)
                    .map(DeviceRecord::record_key)
                else {
                    return false;
                };
                let tab = *tab;
                windows::main_window::navigate(cx, |view, cx| {
                    view.open_device(record_key, cx);
                    view.active_tab = tab;
                });
            }
        }
        true
    }
}

/// Whether `record` is the device `link` names: under the key its settings
/// live under, or under the identity key they move to once the GUI adopts the
/// route they were saved under.
fn names(record: &DeviceRecord, link: &DeviceLink) -> bool {
    record.config_key == link.as_str() || record.canonical_key.as_deref() == Some(link.as_str())
}

/// The selected device's page and its first tab that edits per-application
/// profiles, or `None` when it cannot carry profiles.
fn profile_page(state: &AppState) -> Option<(String, DetailTab)> {
    let record = state
        .current_record()
        .filter(|_| state.current_device_is_persistent())?;
    let tab = DetailTab::tabs_for(record)
        .into_iter()
        .find(|tab| tab.edits_app_profiles(state))?;
    Some((record.record_key(), tab))
}

/// Whether the device list holds the agent's complete inventory, so a link
/// that names nothing in it never will.
fn inventory_complete(cx: &App) -> bool {
    AppState::try_read(cx)
        .and_then(AppState::agent_status)
        .is_some_and(|status| status.inventory == InventoryHealth::Ready)
}
