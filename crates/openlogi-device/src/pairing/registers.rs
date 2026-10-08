use hidpp::channel::HidppChannel;
use hidpp::protocol::v10::{ErrorType, Hidpp10Error};
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
            register_error(&e)
        })
}

fn register_error(e: &Hidpp10Error) -> PairingError {
    match e {
        Hidpp10Error::RegisterAccess(ErrorType::TooManyDevices) => PairingError::ReceiverFull,
        _ => PairingError::Register(format!("{e}")),
    }
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
            register_error(&e)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_receiver_is_its_own_failure() {
        assert!(matches!(
            register_error(&Hidpp10Error::RegisterAccess(ErrorType::TooManyDevices)),
            PairingError::ReceiverFull
        ));
        assert!(matches!(
            register_error(&Hidpp10Error::RegisterAccess(ErrorType::Busy)),
            PairingError::Register(_)
        ));
    }
}
