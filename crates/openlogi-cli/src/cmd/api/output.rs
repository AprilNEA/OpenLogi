//! Deliberate JSON projections: do not expose the entire internal IPC snapshot.

use openlogi_core::hid::DeviceRoute;
use openlogi_ipc::client::ConnectError;
use openlogi_ipc::{AgentSnapshot, InventoryHealth};
use serde::Serialize;
use serde_json::{Value, json};

use crate::agent::CallFailure;

#[derive(Debug, Serialize)]
pub(super) struct ApiError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    persistence: Option<&'static str>,
}

impl ApiError {
    pub(super) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            persistence: None,
        }
    }

    pub(super) fn after_write(mut self, persistence: &'static str) -> Self {
        self.persistence = Some(persistence);
        self
    }
}

impl From<ConnectError> for ApiError {
    fn from(error: ConnectError) -> Self {
        let code = match &error {
            ConnectError::Endpoint(_) => "agent_unavailable",
            ConnectError::Handshake(_) => "handshake_failed",
            ConnectError::Skew(_) => "version_mismatch",
            ConnectError::Timeout => "timeout",
        };
        Self::new(code, error.to_string())
    }
}

impl From<CallFailure> for ApiError {
    fn from(error: CallFailure) -> Self {
        match error {
            CallFailure::TimedOut => Self::new("timeout", "agent call timed out"),
            CallFailure::Disconnected => Self::new("disconnected", "agent connection lost"),
        }
    }
}

pub(super) fn envelope(result: Result<Value, ApiError>) -> Value {
    match result {
        Ok(data) => json!({ "schema_version": 1, "ok": true, "data": data }),
        Err(error) => json!({ "schema_version": 1, "ok": false, "error": error }),
    }
}

pub(super) fn inventory_health(health: InventoryHealth) -> &'static str {
    match health {
        InventoryHealth::Scanning => "scanning",
        InventoryHealth::Ready => "ready",
        InventoryHealth::Unavailable => "unavailable",
    }
}

// Each entry retains its route for selection; JSON only exposes an opaque ID.
// Do not collapse duplicate IDs: ambiguity must refuse writes, not pick a device.
fn entries(snapshot: &AgentSnapshot) -> Vec<(Option<DeviceRoute>, bool, Value)> {
    let mut entries = Vec::new();
    for inventory in &snapshot.inventory {
        for device in &inventory.paired {
            let route = DeviceRoute::for_slot(inventory, device.slot);
            let data = json!({
                "id": route.as_ref().map(ToString::to_string),
                "name": device.codename,
                "kind": device.kind,
                "online": device.online,
                "battery": device.battery,
                "capabilities": device.capabilities,
                "light_capabilities": null,
            });
            entries.push((route, device.online, data));
        }
    }
    for device in &snapshot.standalone {
        let route = device.route();
        let data = json!({
            "id": route.to_string(),
            "name": device.display_name,
            "kind": device.kind,
            "online": device.online,
            "battery": null,
            "capabilities": device.capabilities,
            "light_capabilities": device.light_capabilities,
        });
        entries.push((Some(route), device.online, data));
    }
    entries
}

pub(super) fn devices(snapshot: &AgentSnapshot) -> Vec<Value> {
    entries(snapshot)
        .into_iter()
        .map(|(_, _, data)| data)
        .collect()
}

pub(super) fn select_device(snapshot: &AgentSnapshot, id: &str) -> Result<DeviceRoute, ApiError> {
    if snapshot.status.inventory != InventoryHealth::Ready {
        return Err(ApiError::new(
            "inventory_not_ready",
            "agent inventory is not ready; no device operation was attempted",
        ));
    }
    let matches: Vec<_> = entries(snapshot)
        .into_iter()
        .filter_map(|(route, online, _)| route.map(|route| (route, online)))
        .filter(|(route, _)| route.to_string() == id)
        .collect();
    match matches.as_slice() {
        [(route, true)] => Ok(route.clone()),
        [(_, false)] => Err(ApiError::new(
            "device_offline",
            "selected device is offline",
        )),
        [] => Err(ApiError::new(
            "device_not_found",
            "no device has this exact ID; refresh api devices",
        )),
        _ => Err(ApiError::new(
            "ambiguous_device",
            "the ID addresses multiple devices; no operation was attempted",
        )),
    }
}
