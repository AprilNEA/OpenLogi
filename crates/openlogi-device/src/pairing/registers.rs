use hidpp::channel::HidppChannel;
use tracing::warn;

use super::{PairingError, RECEIVER_INDEX};

/// Notification-flags register (3-byte big-endian value).
pub(super) const NOTIFICATIONS: u8 = 0x00;
/// Unifying pairing lock + unpair.
pub(super) const UNIFYING_PAIRING: u8 = 0xb2;
/// Bolt discovery start/stop (short register).
pub(super) const BOLT_DISCOVERY: u8 = 0xc0;
/// Bolt pair / cancel / unpair (long register).
pub(super) const BOLT_PAIRING: u8 = 0xc1;

/// `WIRELESS` (0x000100) | `SOFTWARE_PRESENT` (0x000800) notification flags,
/// big-endian. Both must be set for the receiver to stream pairing events.
pub(super) const NOTIFICATION_FLAGS: [u8; 3] = [0x00, 0x09, 0x00];

pub(super) async fn write_register(
    channel: &HidppChannel,
    address: u8,
    payload: [u8; 3],
) -> Result<(), PairingError> {
    channel
        .write_register(RECEIVER_INDEX, address, payload)
        .await
        .map_err(|e| {
            warn!(
                register = format_args!("{address:#04x}"),
                ?e,
                "register write failed"
            );
            PairingError::Register(format!("{e}"))
        })
}

pub(super) async fn write_long_register(
    channel: &HidppChannel,
    address: u8,
    payload: [u8; 16],
) -> Result<(), PairingError> {
    channel
        .write_long_register(RECEIVER_INDEX, address, payload)
        .await
        .map_err(|e| {
            warn!(
                register = format_args!("{address:#04x}"),
                ?e,
                "long register write failed"
            );
            PairingError::Register(format!("{e}"))
        })
}
