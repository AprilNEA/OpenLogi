//! Shared topology for sanitized single-device capture and corpus replay.

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
    /// Replay one operation from a single-device HID++ contribution.
    ///
    /// Reject multi-device and raw-HID profiles instead of guessing a target.
    pub fn from_profile(
        profile: &DeviceProfile,
        cassette: &HidCassette,
    ) -> Result<(Self, DeviceRoute), ReplayError> {
        let ([inventory], [settings]) =
            (profile.inventories.as_slice(), profile.settings.as_slice())
        else {
            return Err(ReplayError::invalid(
                "corpus replay",
                "requires one HID++ inventory and one settings route",
            ));
        };
        if !profile.standalone.is_empty() || inventory.paired.len() != 1 {
            return Err(ReplayError::invalid(
                "corpus replay",
                "requires one HID++ device",
            ));
        }
        let route =
            DeviceRoute::for_slot(inventory, inventory.paired[0].slot).ok_or_else(|| {
                ReplayError::invalid("corpus replay", "profile device has no HID++ route")
            })?;
        if route != settings.route {
            return Err(ReplayError::invalid(
                "corpus replay",
                "settings route does not match inventory",
            ));
        }
        let topology = ReplayTopology::for_device(
            &route,
            inventory.receiver.vendor_id,
            inventory.receiver.product_id,
            cassette,
        );
        Ok((Self::new(topology, vec![cassette.clone()])?, route))
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
    fn a_multi_device_profile_cannot_silently_select_the_first_route() {
        let profile: DeviceProfile =
            serde_json::from_str(openlogi_fixture::CANONICAL_DEVICE_PROFILE_JSON)
                .expect("canonical profile");
        let cassette = HidCassette {
            schema_version: openlogi_fixture::FIXTURE_SCHEMA_VERSION,
            name: "operation".into(),
            channel: "target".into(),
            report_support: openlogi_fixture::ReportSupport::LongOnly,
            exchanges: Vec::new(),
        };
        let error = ReplayBackend::from_profile(&profile, &cassette)
            .err()
            .expect("ambiguous profile must fail");
        assert!(error.to_string().contains("one HID++ inventory"));
    }
}
