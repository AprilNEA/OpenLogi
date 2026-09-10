//! Explicit receiver-slot contracts, not a receiver firmware emulator.

use std::collections::BTreeMap;

use hidpp::channel::HidppMessage;
use hidpp::protocol::v10::{self, MessageType};
use hidpp::receiver::{RECEIVER_DEVICE_INDEX, bolt, unifying};

use super::{ReceiverLinkState, ReceiverSlot, ReceiverSlotState, ReplayError};

#[derive(Default)]
pub(super) struct ReceiverSlots {
    states: BTreeMap<u8, ReceiverSlotState>,
}

impl ReceiverSlots {
    pub(super) fn declare(&mut self, slots: Vec<ReceiverSlot>) -> Result<(), ReplayError> {
        for slot in slots {
            if let Some(previous) = self.states.get(&slot.slot)
                && *previous != slot.state
            {
                return Err(ReplayError::invalid(
                    "replay topology",
                    format!("shared channel disagrees on receiver slot {}", slot.slot),
                ));
            }
            self.states.insert(slot.slot, slot.state);
        }
        Ok(())
    }

    pub(super) fn get(&self, slot: u8) -> Result<ReceiverSlotState, ReplayError> {
        self.states.get(&slot).copied().ok_or_else(|| {
            ReplayError::invalid(
                "replay topology",
                format!("no declared receiver slot {slot}"),
            )
        })
    }

    pub(super) fn set(&mut self, slot: u8, state: ReceiverSlotState) -> Result<(), ReplayError> {
        self.get(slot)?;
        self.states.insert(slot, state);
        Ok(())
    }

    /// Check only explicit declarations. Unspecified slots carry no liveness claim.
    pub(super) fn validate(
        &self,
        request: Option<&[u8]>,
        report: &[u8],
    ) -> Result<(), ReplayError> {
        if self.states.is_empty() {
            return Ok(());
        }
        let Some(raw) = HidppMessage::read_raw(report) else {
            return Ok(()); // Report framing is validated at the replay boundary.
        };
        if request.is_some_and(|request| is_error_response(request, report)) {
            return Ok(());
        }
        if request.is_none_or(|request| request[1..4] != report[1..4])
            && let Some(unifying::Event::DeviceConnection(connection)) =
                unifying::decode_notification(&v10::Message::from(raw))
        {
            // Bolt and Unifying share this slot/status layout and inverted
            // liveness bit. An exact cassette can also deliver an arrival in
            // response to a receiver trigger; it is still slot evidence.
            return self.require(
                connection.index,
                Some(link_state(connection.online)),
                report,
            );
        }
        // Any successful device-addressed exchange requires a live link,
        // including acknowledgments of writes. Receiver control is separate.
        if let Some(request) = request {
            self.require(request[1], Some(ReceiverLinkState::Online), report)?;
        }
        self.require(report[1], Some(ReceiverLinkState::Online), report)?;

        if report[1] != RECEIVER_DEVICE_INDEX {
            return Ok(());
        }
        if report[2] == u8::from(MessageType::GetLongRegister)
            && report[3] == u8::from(bolt::Register::ReceiverInfo)
        {
            // RAP correlates on register, not sub-register. Validate what the
            // caller will attribute to its requested slot even in a malformed
            // cassette whose response echoes a different sub-register.
            let sub = request.map_or(report[4], |request| request[4]);
            if (0x51..=0x56).contains(&sub) {
                // Receiver::get_device_pairing_information reads this same
                // status byte; no pure public pairing-register decoder exists.
                self.require(sub & 0x0f, Some(link_state(report[5] & 0x40 == 0)), report)?;
            } else if (0x61..=0x66).contains(&sub) {
                self.require(sub & 0x0f, None, report)?;
            } else if (0x40..=0x45).contains(&sub) {
                // inventory::probe::read_codename_unifying uses base 0x40 + n-1.
                self.require(sub - 0x40 + 1, None, report)?;
            }
        } else if report[2] == u8::from(MessageType::GetRegister)
            && report[3] == u8::from(bolt::Register::Connections)
        {
            let paired = self
                .states
                .values()
                .filter(|state| matches!(state, ReceiverSlotState::Paired(_)))
                .count();
            let empty = self.states.len() - paired;
            let count = usize::from(report[5]);
            if count < paired || count > 6 - empty {
                return Err(ReplayError::invalid(
                    "receiver slot replay",
                    format!(
                        "pairing count {count} contradicts declared slots {:?}: report={report:02x?}",
                        self.states
                    ),
                ));
            }
        }
        Ok(())
    }

    fn require(
        &self,
        slot: u8,
        link: Option<ReceiverLinkState>,
        report: &[u8],
    ) -> Result<(), ReplayError> {
        let Some(state) = self.states.get(&slot) else {
            return Ok(());
        };
        if matches!(state, ReceiverSlotState::Paired(actual) if link.is_none_or(|link| link == *actual))
        {
            return Ok(());
        }
        Err(ReplayError::invalid(
            "receiver slot replay",
            format!(
                "slot {slot} is {state:?}, but report requires pairing with link {link:?}: report={report:02x?}"
            ),
        ))
    }
}

fn link_state(online: bool) -> ReceiverLinkState {
    if online {
        ReceiverLinkState::Online
    } else {
        ReceiverLinkState::Offline
    }
}

fn is_error_response(request: &[u8], response: &[u8]) -> bool {
    // Match the echoed operation, not just a sub-ID: 0x8f can also be a
    // successful HID++ 2.0 runtime feature index. Exact is a matching policy,
    // not proof that a request uses HID++ 1.0.
    response[1] == request[1]
        && response[2..4] != request[2..4]
        && (response[2] == 0xff || response[2] == u8::from(MessageType::Error))
        && response[3..5] == request[2..4]
}
