use hidpp::protocol::v10::{ErrorType, Hidpp10Error};
use hidpp::{channel::HidppChannel, receiver::NotificationFlags};
use tracing::warn;

use super::{PairingError, RECEIVER_INDEX};

/// Unifying pairing lock + unpair.
pub(super) const UNIFYING_PAIRING: u8 = 0xb2;
/// Bolt discovery start/stop (short register).
pub(super) const BOLT_DISCOVERY: u8 = 0xc0;
/// Bolt pair / cancel / unpair (long register).
pub(super) const BOLT_PAIRING: u8 = 0xc1;

/// Notification flags a session adds to the ones it finds: the receiver
/// streams pairing events only while both are set.
pub(super) const PAIRING_NOTIFICATIONS: NotificationFlags =
    NotificationFlags::WIRELESS.union(NotificationFlags::SOFTWARE_PRESENT);

pub(super) async fn read_notification_flags(
    channel: &HidppChannel,
) -> Result<NotificationFlags, PairingError> {
    hidpp::receiver::read_notification_flags(channel)
        .await
        .map_err(|e| {
            warn!(?e, "notification flags read failed");
            PairingError::Register(format!("{e}"))
        })
}

pub(super) async fn write_notification_flags(
    channel: &HidppChannel,
    flags: NotificationFlags,
) -> Result<(), PairingError> {
    hidpp::receiver::write_notification_flags(channel, flags)
        .await
        .map_err(|e| {
            warn!(?e, "notification flags write failed");
            PairingError::Register(format!("{e}"))
        })
}

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
