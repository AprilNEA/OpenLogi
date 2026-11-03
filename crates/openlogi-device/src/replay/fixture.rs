//! Shared one-device topology for sanitized capture self-replay and corpus replay.

use super::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReceiverLinkState,
    ReceiverSlot, ReceiverSlotState, ReplayBackend, ReplayChannel, ReplayError, ReplayNode,
    ReplayTopology,
};
use crate::{DeviceRoute, NodeId, NodeInfo};
use openlogi_fixture::{DeviceProfile, HidCassette};

const NODE: &str = "openlogi-sanitized-replay-node";

impl ReplayTopology {
    /// Build the isolated HID++ node used by capture self-replay and corpus tests.
    #[must_use]
    pub fn for_device(
        route: &DeviceRoute,
        vendor_id: u16,
        product_id: u16,
        cassette: &HidCassette,
    ) -> Self {
        let receiver_slots = match route {
            DeviceRoute::Bolt { slot, .. } | DeviceRoute::Unifying { slot, .. } => {
                vec![ReceiverSlot {
                    slot: *slot,
                    state: ReceiverSlotState::Paired(ReceiverLinkState::Online),
                }]
            }
            DeviceRoute::Direct { .. } | DeviceRoute::RawHid { .. } => Vec::new(),
        };
        ReplayTopology {
            nodes: vec![ReplayNode {
                info: NodeInfo {
                    id: NodeId::from(NODE.to_string()),
                    vendor_id,
                    product_id,
                    usage_page: 0xff00,
                    usage_id: 0x0001,
                    name: "OpenLogi sanitized replay node".to_string(),
                    manufacturer: Some("OpenLogi synthetic fixture".to_string()),
                    serial_number: None,
                },
                presence: NodePresence::Present,
                open_outcome: OpenOutcome::Hidpp,
                channel: Some(cassette.channel.clone()),
                raw_writer: RawWriterAvailability::Unavailable,
                receiver_slots,
            }],
            channels: vec![ReplayChannel {
                id: cassette.channel.clone(),
                connection: ChannelConnection::Connected,
                report_support: cassette.report_support,
            }],
        }
    }
}

impl ReplayBackend {
    /// Replay one operation against one paired HID++ device of a contribution.
    ///
    /// A receiver contribution records every paired device, so the caller names
    /// the target instead of this function guessing one by position. Corpus
    /// tests take it from the case relationship through
    /// [`openlogi_fixture::FixtureManifest::case_settings`]. Raw-HID routes and
    /// routes absent from the profile's inventories are rejected.
    pub fn from_profile(
        profile: &DeviceProfile,
        route: &DeviceRoute,
        cassette: &HidCassette,
    ) -> Result<Self, ReplayError> {
        let inventory = profile
            .inventories
            .iter()
            .find(|inventory| {
                inventory.paired.iter().any(|device| {
                    DeviceRoute::for_slot(inventory, device.slot).as_ref() == Some(route)
                })
            })
            .ok_or_else(|| {
                ReplayError::invalid(
                    "corpus replay",
                    "target route is not a paired HID++ device of the profile",
                )
            })?;
        let topology = ReplayTopology::for_device(
            route,
            inventory.receiver.vendor_id,
            inventory.receiver.product_id,
            cassette,
        );
        Self::new(topology, vec![cassette.clone()])
    }
}

impl ReplayBackend {
    /// Publish a recorded operation's route without an unrecorded inventory probe.
    ///
    /// Use this setup with a backend created by [`Self::from_profile`].
    /// This fixture setup exercises the real route opener and channel registry.
    /// Inventory discovery itself requires separate enumeration cassettes.
    pub async fn publish_route(
        &self,
        registry: &crate::ChannelRegistry,
        route: &DeviceRoute,
    ) -> Result<(), crate::WriteError> {
        let channel = crate::channel::route::open_route_channel(self, route)
            .await?
            .ok_or(crate::WriteError::DeviceNotFound)?;
        registry.replace_node(NodeId::from(NODE.to_string()), [route.clone()], channel);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multi_device_profile_replays_only_the_named_paired_route() {
        let profile: DeviceProfile =
            serde_json::from_str(openlogi_fixture::CANONICAL_DEVICE_PROFILE_JSON)
                .expect("canonical profile");
        let cassette = HidCassette {
            schema_version: openlogi_fixture::FIXTURE_SCHEMA_VERSION,
            name: "operation".into(),
            channel: "target".into(),
            report_support: openlogi_fixture::ReportSupport::ShortAndLong,
            exchanges: vec![openlogi_fixture::CassetteExchange {
                request_match: openlogi_fixture::RequestMatch::Hidpp20,
                request: vec![0x10, 3, 0, 0x10, 0, 0, 0],
                response: Some(vec![0x10, 3, 0, 0x10, 4, 0, 0]),
                required: true,
            }],
        };
        // The canonical Bolt receiver lists a mouse at slot 1 before the keyboard at slot 3.
        let keyboard = &profile.settings[2].route;
        assert!(matches!(keyboard, DeviceRoute::Bolt { slot: 3, .. }));
        ReplayBackend::from_profile(&profile, keyboard, &cassette)
            .expect("a named paired device replays");

        let unpaired = DeviceRoute::Bolt {
            receiver_uid: "OL-BOLT-UID-0001".to_string(),
            slot: 6,
        };
        let raw_hid = &profile.settings[4].route;
        assert!(matches!(raw_hid, DeviceRoute::RawHid { .. }));
        for route in [&unpaired, raw_hid] {
            let error = ReplayBackend::from_profile(&profile, route, &cassette)
                .err()
                .expect("a route outside the HID++ inventories must fail");
            assert!(error.to_string().contains("not a paired HID++ device"));
        }
    }
}
