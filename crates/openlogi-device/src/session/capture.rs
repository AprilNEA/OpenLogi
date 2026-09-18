//! Channel-lifecycle machinery shared by the capture sessions.

pub(super) mod liveness;

use tokio::sync::{mpsc, oneshot};

use super::capture_restore::CaptureChannelSlot;
use super::gesture::CapturedInput;
use crate::{ChannelRegistry, DeviceIoGate};

/// What the process running a capture session hands it: where inputs go, when
/// to stop, and the handles that tie the session to the rest of the device
/// layer.
pub struct CaptureHost<'a> {
    /// Receives every [`CapturedInput`] the session decodes.
    pub sink: mpsc::UnboundedSender<CapturedInput>,
    /// Resolves, or is dropped, when the session should restore its controls
    /// and return.
    pub shutdown: oneshot::Receiver<()>,
    /// Where the session publishes its open channel so bounded hardware
    /// writes reuse it instead of opening a second connection.
    pub channel_slot: CaptureChannelSlot,
    /// Inventory's channel publications: the session runs on the one current
    /// for its route and stops when that publication is replaced or removed.
    pub registry: &'a ChannelRegistry,
    /// Host device-I/O gate: the session refuses to start while it is closed
    /// and sends nothing until it reopens.
    pub device_io: DeviceIoGate,
}
