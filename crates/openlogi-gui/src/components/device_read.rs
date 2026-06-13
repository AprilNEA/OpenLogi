//! The lazy device-read skeleton shared by the DPI and SmartShift panels.
//!
//! Both panels resolve their state by sending a one-shot read request to the
//! agent over IPC and awaiting the typed reply off the render thread, then
//! storing the result — or clearing the loading marker if the reply never
//! comes. Only the command, the store action, and the clear action differ; the
//! panels inject them as their own `AppState` method references.

use gpui::{BorrowAppContext as _, Context};
use openlogi_hid::DeviceRoute;
use tokio::sync::oneshot;

use crate::ipc_client::Command;
use crate::state::AppState;

/// Issue a one-shot device read over IPC and apply its typed result to
/// [`AppState`], off the render thread.
///
/// `make_command` builds the read [`Command`] from the route and reply channel
/// (e.g. [`Command::ReadDpi`]); `store` applies a delivered result (e.g.
/// [`AppState::store_dpi_info`]); `clear` resets the loading marker when the
/// reply is dropped (e.g. [`AppState::clear_dpi_loading`]). The caller marks the
/// loading state first for an initial load, or skips it for a post-write confirm
/// re-read so the optimistic value stays on screen until the real one lands.
pub fn issue_device_read<P, T>(
    cx: &mut Context<P>,
    key: String,
    route: DeviceRoute,
    make_command: impl FnOnce(DeviceRoute, oneshot::Sender<T>) -> Command,
    store: impl FnOnce(&mut AppState, String, &DeviceRoute, T) + 'static,
    clear: impl Fn(&mut AppState, &str) + 'static,
) where
    P: 'static,
    T: 'static,
{
    let sender = cx.global::<AppState>().ipc_sender();
    let (tx, rx) = oneshot::channel();
    if sender.send(make_command(route.clone(), tx)).is_err() {
        // Repaint here too: the async branches below refresh after clearing the
        // marker, so a channel that's already broken when the read is issued
        // must not leave the panel stuck on "Reading…" until an unrelated event
        // redraws it. (Both original panels missed this on the send-error path.)
        cx.update_global::<AppState, _>(|state, cx| {
            clear(state, &key);
            cx.refresh_windows();
        });
        return;
    }
    cx.spawn(async move |_panel, cx| {
        match rx.await {
            Ok(result) => {
                cx.update_global::<AppState, _>(|state, cx| {
                    store(state, key, &route, result);
                    cx.refresh_windows();
                });
            }
            // The client thread dropped the reply (it's gone). Reset the loading
            // marker so the device isn't stuck on "Reading…".
            Err(_) => {
                cx.update_global::<AppState, _>(|state, cx| {
                    clear(state, &key);
                    cx.refresh_windows();
                });
            }
        }
    })
    .detach();
}
