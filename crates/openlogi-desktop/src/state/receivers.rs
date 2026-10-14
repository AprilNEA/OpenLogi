//! Every pairing slot of each connected receiver, for the Settings Receivers
//! page.
//!
//! Derived from the agent's inventories plus the merged device list, so a
//! slot names the same device its gallery card does, and a leftover slot is
//! exactly one the device list folded into a card as a stale pairing.

use openlogi_core::device::DeviceInventory;
use openlogi_core::hid::{DeviceRoute, ReceiverBrand, ReceiverDescriptor, find_receiver};

use super::devices::DeviceRecord;

/// One connected receiver and what each of its slots holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceiverSlots {
    /// Product family, which decides the label; `None` for a receiver the
    /// registry does not know.
    pub(crate) brand: Option<ReceiverBrand>,
    /// The name the agent reported, shown when the family has no label.
    pub(crate) name: String,
    /// How many devices the receiver can pair, when that is known.
    pub(crate) capacity: Option<u8>,
    /// Every slot in order: all of them when the capacity is known,
    /// otherwise only the occupied ones.
    pub(crate) slots: Vec<ReceiverSlot>,
}

/// One pairing slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceiverSlot {
    pub(crate) slot: u8,
    pub(crate) occupant: SlotOccupant,
}

/// What a slot holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlotOccupant {
    /// Nothing is paired here.
    Empty,
    /// The slot a device's card is reached through.
    Device { name: String, online: bool },
    /// An offline leftover pairing of the device named, folded into that
    /// device's card and removable on its own.
    OldPairing { name: String, route: DeviceRoute },
}

impl ReceiverSlots {
    /// How many slots hold a pairing.
    pub(crate) fn used(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.occupant != SlotOccupant::Empty)
            .count()
    }
}

/// The slots of every receiver in `inventories`, naming each occupied slot
/// by the device `records` show for it.
///
/// A direct device's inventory names the device itself as its receiver and
/// has no receiver id; it has no slots and is left out.
pub(super) fn build_receivers(
    inventories: &[DeviceInventory],
    records: &[DeviceRecord],
) -> Vec<ReceiverSlots> {
    inventories
        .iter()
        .filter_map(|inventory| {
            let receiver = &inventory.receiver;
            let descriptor = find_receiver(receiver.vendor_id, receiver.product_id);
            if descriptor.is_none() && receiver.unique_id.is_none() {
                return None;
            }
            let capacity = descriptor.and_then(ReceiverDescriptor::pairing_slots);
            let mut slots: Vec<ReceiverSlot> = inventory
                .paired
                .iter()
                .map(|paired| ReceiverSlot {
                    slot: paired.slot,
                    occupant: occupant(
                        DeviceRoute::for_slot(inventory, paired.slot),
                        paired.codename.as_deref(),
                        paired.online,
                        records,
                    ),
                })
                .collect();
            for slot in 1..=capacity.unwrap_or(0) {
                if !slots.iter().any(|known| known.slot == slot) {
                    slots.push(ReceiverSlot {
                        slot,
                        occupant: SlotOccupant::Empty,
                    });
                }
            }
            slots.sort_by_key(|slot| slot.slot);
            Some(ReceiverSlots {
                brand: descriptor.map(|descriptor| descriptor.brand),
                name: receiver.name.clone(),
                capacity,
                slots,
            })
        })
        .collect()
}

/// Name a slot's occupant after the card that represents it: the card on
/// this route, else the card this route was folded into as a leftover, else
/// the codename the receiver reported.
fn occupant(
    route: Option<DeviceRoute>,
    codename: Option<&str>,
    online: bool,
    records: &[DeviceRecord],
) -> SlotOccupant {
    let reported = || SlotOccupant::Device {
        name: codename.unwrap_or_default().to_string(),
        online,
    };
    let Some(route) = route else {
        return reported();
    };
    if let Some(record) = records
        .iter()
        .find(|record| record.route.as_ref() == Some(&route))
    {
        return SlotOccupant::Device {
            name: record.display_name.clone(),
            online,
        };
    }
    records
        .iter()
        .find(|record| record.stale_pairings.contains(&route))
        .map_or_else(reported, |record| SlotOccupant::OldPairing {
            name: record.display_name.clone(),
            route,
        })
}
