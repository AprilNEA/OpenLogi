//! The dialog that explains why a receiver kept a pairing the user asked it
//! to drop.
//!
//! Both the main window (a device card's removal) and Settings (a Receivers
//! page row) can send that request, so the dialog lives here instead of in
//! either view.

use gpui::{App, SharedString, Window};
use gpui_component::WindowExt as _;
use openlogi_ipc::PairingFailure;

use crate::services::ipc::UnpairFailure;
use crate::windows::add_device::pairing_failure_text;

/// Say why a device the user asked to forget is still here: its receiver kept
/// the pairing, so deleting only its settings would bring the card back.
pub(crate) fn open_removal_failed(
    window: &mut Window,
    cx: &mut App,
    name: &str,
    failure: &UnpairFailure,
) {
    let title = tr!("device.delete_device_failed", name => name.to_string());
    open_unpair_failed(window, cx, title, failure);
}

/// Say why a leftover slot the user asked to remove is still paired.
pub(crate) fn open_stale_pairing_removal_failed(
    window: &mut Window,
    cx: &mut App,
    name: &str,
    failure: &UnpairFailure,
) {
    let title = tr!("device.remove_old_pairings_failed", name => name.to_string());
    open_unpair_failed(window, cx, title, failure);
}

fn open_unpair_failed(
    window: &mut Window,
    cx: &mut App,
    title: SharedString,
    failure: &UnpairFailure,
) {
    let description = match failure {
        UnpairFailure::AgentUnreachable => tr!("agent.cant_reach_the_background_service"),
        UnpairFailure::Refused(PairingFailure::ReceiverBusy) => {
            tr!("device.delete_device_receiver_busy")
        }
        UnpairFailure::Refused(PairingFailure::ReceiverNotFound) => {
            tr!("device.delete_device_receiver_missing")
        }
        UnpairFailure::Refused(other) => pairing_failure_text(other).into(),
    };
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert.title(title.clone()).description(description.clone())
    });
}
