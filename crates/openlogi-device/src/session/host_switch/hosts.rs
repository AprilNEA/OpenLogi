//! The Easy-Switch slots of a keyboard and of a linked device, read through
//! `HostsInfo` (`0x1815`), and the slot that a linked device takes when the
//! keyboard moves.
//!
//! The table is advisory. A device without `HostsInfo`, or one whose table
//! cannot be read, has no table, and the session then keeps the slot numbers
//! as they are.

use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::{
        CreatableFeature,
        hosts_info::{HostIndex, HostSlotStatus, HostsInfoFeature},
    },
    protocol::v20::{self, Hidpp20Error},
};
use tokio::time::timeout;
use tracing::debug;

use super::{HIDPP_OPERATION_TIMEOUT, timed_hidpp};
use crate::write::read_host_friendly_name;

/// What `HostsInfo` reports for one Easy-Switch slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HostSlot {
    /// The slot reports the `Empty` status and stores no name.
    ///
    /// Some devices report `Empty` for every slot, also for paired slots that
    /// store a real name (an MX Vertical, #1709). A stored name is therefore
    /// evidence of a pairing, as in [`super::host_slot_is_empty`].
    pub(super) empty: bool,
    /// The host name that the slot stores, if any.
    pub(super) name: Option<String>,
}

/// The slots of one device, indexed by zero-based host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct HostTable(pub(super) Vec<HostSlot>);

impl HostTable {
    /// Whether the device reports `host` as an empty slot.
    pub(super) fn is_empty_slot(&self, host: u8) -> bool {
        self.slot(host).is_some_and(|slot| slot.empty)
    }

    /// The name that the device stores for `host`.
    pub(super) fn name(&self, host: u8) -> Option<&str> {
        self.slot(host).and_then(|slot| slot.name.as_deref())
    }

    /// The slots, other than empty ones, that store `name`.
    fn hosts_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = u8> + 'a {
        (0..=u8::MAX)
            .zip(&self.0)
            .filter(move |(_, slot)| !slot.empty && slot.name.as_deref() == Some(name))
            .map(|(host, _)| host)
    }

    fn slot(&self, host: u8) -> Option<&HostSlot> {
        self.0.get(usize::from(host))
    }
}

/// `setHostFriendlyName`, the `HostsInfo` function that renames a slot.
const SET_HOST_FRIENDLY_NAME: u8 = 4;

/// The `HostsInfo` feature of `device` and its index. Returns `None` when the
/// device has no `HostsInfo`, or when the lookup fails or takes longer than
/// one operation budget.
async fn locate_hosts_info(device: &mut Device) -> Option<(u8, Arc<HostsInfoFeature>)> {
    match timed_hidpp(
        "locating hosts-info feature",
        device.root().get_feature(HostsInfoFeature::ID),
    )
    .await
    {
        Ok(Some(info)) => Some((
            info.index,
            device.add_feature::<HostsInfoFeature>(info.index),
        )),
        Ok(None) => None,
        Err(error) => {
            debug!(%error, "hosts-info lookup failed; slot names are unknown");
            None
        }
    }
}

/// Read the slot table of `device`. Returns `None` when the device has no
/// `HostsInfo`, or when a read fails or takes longer than one operation
/// budget.
pub(super) async fn read_host_table(device: &mut Device) -> Option<HostTable> {
    let (_, feature) = locate_hosts_info(device).await?;
    read_table(&feature).await
}

async fn read_table(feature: &HostsInfoFeature) -> Option<HostTable> {
    match timeout(HIDPP_OPERATION_TIMEOUT, read_slots(feature)).await {
        Ok(Ok(table)) => Some(table),
        Ok(Err(error)) => {
            debug!(?error, "hosts-info read failed; slot names are unknown");
            None
        }
        Err(_) => {
            debug!("hosts-info read timed out; slot names are unknown");
            None
        }
    }
}

/// The slot table of the keyboard while a session watches it.
///
/// A rename through this agent changes a name that the table holds. The
/// session therefore reads the table again after each rename, while the
/// keyboard is still on this host.
#[derive(Default)]
pub(super) struct KeyboardHosts {
    feature: Option<(u8, Arc<HostsInfoFeature>)>,
    table: Option<HostTable>,
}

impl KeyboardHosts {
    /// Locate `HostsInfo` on the keyboard. The table is not read yet.
    pub(super) async fn locate(device: &mut Device) -> Self {
        Self {
            feature: locate_hosts_info(device).await,
            table: None,
        }
    }

    /// The `HostsInfo` index, when the keyboard has the feature.
    pub(super) fn feature_index(&self) -> Option<u8> {
        self.feature.as_ref().map(|(index, _)| *index)
    }

    /// The table from the last read.
    pub(super) fn table(&self) -> Option<&HostTable> {
        self.table.as_ref()
    }

    /// Read the table again. A failed read leaves no table, so that a
    /// departure keeps the slot numbers and does not use an old name.
    pub(super) async fn read(&mut self) {
        self.table = match &self.feature {
            Some((_, feature)) => read_table(feature).await,
            None => None,
        };
    }

    /// Drop the table without a read, when the keyboard must not be read.
    pub(super) fn forget(&mut self) {
        self.table = None;
    }

    /// Hosts with `table` already read, and no `HostsInfo` to read it again.
    #[cfg(test)]
    pub(super) fn with_table(table: HostTable) -> Self {
        Self {
            feature: None,
            table: Some(table),
        }
    }
}

/// Whether `message` is a `setHostFriendlyName` report of the `HostsInfo`
/// feature at `feature_index` of the device at `device_index`: the reply to a
/// rename of one of its slots. An error reply uses feature index `0xff` and
/// does not match.
pub(super) fn renames_host(message: &v20::Message, device_index: u8, feature_index: u8) -> bool {
    let header = message.header();
    header.device_index == device_index
        && header.feature_index == feature_index
        && header.function_id.to_lo() == SET_HOST_FRIENDLY_NAME
}

async fn read_slots(feature: &HostsInfoFeature) -> Result<HostTable, Hidpp20Error> {
    let info = feature.get_feature_info().await?;
    let mut slots = Vec::with_capacity(usize::from(info.host_count));
    for host in 0..info.host_count {
        let slot = feature.get_host_info(HostIndex::Slot(host)).await?;
        let name = if slot.name_len > 0 {
            read_host_friendly_name(feature, host, slot.name_len).await?
        } else {
            None
        };
        slots.push(HostSlot {
            empty: slot.status == HostSlotStatus::Empty && slot.name_len == 0,
            name,
        });
    }
    Ok(HostTable(slots))
}

/// The slot that a linked device takes when the keyboard moves to `host`.
///
/// Logi Options+ 2.8 moves a linked device to its paired slot whose
/// `HostsInfo` name equals the name that the keyboard stores for `host`, and
/// does not move it when no slot matches (reverse-engineered,
/// `feature_1815_hosts_infos::on_device_host_changed`). OpenLogi uses the
/// name only when the name is unambiguous, and keeps the slot number
/// otherwise, as it did before it read names:
///
/// - The keyboard stores no name for `host`, the name is on more than one
///   keyboard slot, or a table is missing: `host`.
/// - The linked device's slot `host` stores the name: `host`.
/// - Exactly one other slot of the linked device stores the name: that slot.
/// - No slot or more than one slot stores the name: `host`.
pub(super) fn follower_host(
    keyboard: Option<&HostTable>,
    follower: Option<&HostTable>,
    host: u8,
) -> u8 {
    let (Some(keyboard), Some(follower)) = (keyboard, follower) else {
        return host;
    };
    let Some(name) = keyboard.name(host) else {
        return host;
    };
    if keyboard.hosts_named(name).count() != 1 {
        return host;
    }
    let mut named = follower.hosts_named(name);
    match (named.next(), named.next()) {
        (Some(slot), None) => slot,
        _ => host,
    }
}

#[cfg(test)]
mod tests {
    use super::{HostSlot, HostTable, follower_host};

    fn table(slots: &[Option<&str>]) -> HostTable {
        HostTable(
            slots
                .iter()
                .map(|name| HostSlot {
                    empty: name.is_none(),
                    name: name.map(str::to_owned),
                })
                .collect(),
        )
    }

    #[test]
    fn a_unique_name_moves_the_follower_to_the_slot_that_stores_it() {
        let keyboard = table(&[Some("Desk"), Some("Laptop"), None]);
        let follower = table(&[Some("Laptop"), Some("Desk"), None]);
        assert_eq!(follower_host(Some(&keyboard), Some(&follower), 1), 0);
        assert_eq!(follower_host(Some(&keyboard), Some(&follower), 0), 1);
    }

    #[test]
    fn the_slot_number_is_kept_when_the_name_does_not_settle_the_slot() {
        let keyboard = table(&[Some("Desk"), Some("Laptop"), None]);
        for (follower, why) in [
            (
                table(&[Some("Desk"), Some("Laptop"), None]),
                "the same slot stores it",
            ),
            (
                table(&[Some("Desk"), Some("Other"), None]),
                "no slot stores it",
            ),
            (
                table(&[Some("Laptop"), Some("Other"), Some("Laptop")]),
                "two slots store it",
            ),
        ] {
            assert_eq!(
                follower_host(Some(&keyboard), Some(&follower), 1),
                1,
                "{why}"
            );
        }
    }

    #[test]
    fn a_name_that_two_keyboard_slots_store_keeps_the_slot_number() {
        let keyboard = table(&[Some("Mac"), Some("Mac"), None]);
        let follower = table(&[None, None, Some("Mac")]);
        assert_eq!(follower_host(Some(&keyboard), Some(&follower), 1), 1);
    }

    #[test]
    fn a_missing_name_or_table_keeps_the_slot_number() {
        let keyboard = table(&[Some("Desk"), None, None]);
        let follower = table(&[None, Some("Desk"), None]);
        assert_eq!(follower_host(Some(&keyboard), Some(&follower), 1), 1);
        assert_eq!(follower_host(None, Some(&follower), 0), 0);
        assert_eq!(follower_host(Some(&keyboard), None, 0), 0);
    }

    #[test]
    fn an_empty_slot_never_matches_a_name() {
        let keyboard = table(&[Some("Desk"), None, None]);
        let mut follower = table(&[None, Some("Desk"), None]);
        follower.0[1].empty = true;
        assert_eq!(follower_host(Some(&keyboard), Some(&follower), 0), 0);
    }

    #[test]
    fn only_an_explicitly_empty_slot_is_empty() {
        let keyboard = table(&[Some("Desk"), None]);
        assert!(!keyboard.is_empty_slot(0));
        assert!(keyboard.is_empty_slot(1));
        assert!(
            !keyboard.is_empty_slot(2),
            "a slot outside the table is not known to be empty"
        );
    }
}
