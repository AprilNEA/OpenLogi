//! Tray-to-GUI navigation retained by the agent until the GUI acknowledges it.

use openlogi_agent_core::observable::ObservableState;
use openlogi_ipc::{Identity, PROTOCOL_VERSION};
use std::sync::{Arc, OnceLock};

static STATE: OnceLock<Arc<ObservableState>> = OnceLock::new();

/// Attach the cell before the platform tray can publish requests.
pub(crate) fn init(state: Arc<ObservableState>) {
    if STATE.set(state).is_err() {
        tracing::error!("device selection was initialized twice");
    }
}

/// Select a device when the native tray next launches or focuses the GUI.
pub(crate) fn request(key: String) {
    if let Some(state) = STATE.get() {
        state.request_device_selection(Identity::mine(PROTOCOL_VERSION.into()), key);
    } else {
        tracing::warn!("device selection requested before agent startup");
    }
}
