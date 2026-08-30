//! Sanitized route derivation and strict production-operation self-replay.

use anyhow::{Result, bail};
use openlogi_device::fixture::{
    ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReceiverLinkState,
    ReceiverSlot, ReceiverSlotState, ReplayBackend, ReplayChannel, ReplayNode, ReplayTopology,
};
use openlogi_device::{DeviceRoute, NodeId, NodeInfo};
use openlogi_hid::recording::{HidCassetteAudit, SanitizedIdentityKind};

use super::{
    FixtureOperation, SanitizedCandidate, SemanticObservation, TargetCandidate, uppercase_hex,
};

pub(super) async fn select_self_replaying(
    operation: FixtureOperation,
    target: &TargetCandidate,
    captured: &SemanticObservation,
    candidates: Vec<SanitizedCandidate>,
) -> Result<openlogi_device::fixture::HidCassette> {
    let mut passing = Vec::new();
    for candidate in candidates {
        if candidate_passes(operation, target, captured, &candidate).await {
            passing.push(candidate);
        }
    }
    let selected = require_single_passing_candidate(passing)?;
    selected.cassette.validate()?;
    derive_replay_route(&target.route, &selected.audit)?;
    Ok(selected.cassette)
}

async fn candidate_passes(
    operation: FixtureOperation,
    target: &TargetCandidate,
    captured: &SemanticObservation,
    candidate: &SanitizedCandidate,
) -> bool {
    let Ok(route) = derive_replay_route(&target.route, &candidate.audit) else {
        return false;
    };
    let topology = replay_topology(target, &route, &candidate.cassette);
    let Ok(backend) = ReplayBackend::new(topology, vec![candidate.cassette.clone()]) else {
        return false;
    };
    let replayed = operation.observe(&backend, &route).await;
    replayed.ensure_replayable().is_ok()
        && replayed == *captured
        && backend.require_complete().is_ok()
}

fn require_single_passing_candidate(
    mut candidates: Vec<SanitizedCandidate>,
) -> Result<SanitizedCandidate> {
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => bail!(
            "no sanitized channel candidate reproduced the captured semantic observation and \
             completed strict replay; no fixture was written"
        ),
        count => bail!(
            "{count} sanitized channel candidates reproduced the capture; target channel \
             resolution is ambiguous, so no fixture was written"
        ),
    }
}

fn replay_topology(
    target: &TargetCandidate,
    route: &DeviceRoute,
    cassette: &openlogi_device::fixture::HidCassette,
) -> ReplayTopology {
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
                id: NodeId::from("openlogi-sanitized-replay-node".to_string()),
                vendor_id: target.receiver_vendor_id,
                product_id: target.receiver_product_id,
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

fn derive_replay_route(
    selected_route: &DeviceRoute,
    audit: &HidCassetteAudit,
) -> Result<DeviceRoute> {
    match selected_route {
        DeviceRoute::Bolt { slot, .. } => {
            let value = unique_replacement(audit, SanitizedIdentityKind::ReceiverUniqueId)?;
            if value.len() != 16 || !value.is_ascii() {
                bail!("sanitized Bolt receiver identity is not 16-byte ASCII");
            }
            let receiver_uid = std::str::from_utf8(value)
                .map_err(|_| anyhow::anyhow!("sanitized Bolt receiver identity is not ASCII"))?
                .to_string();
            Ok(DeviceRoute::Bolt {
                receiver_uid,
                slot: *slot,
            })
        }
        DeviceRoute::Unifying { slot, .. } => {
            let value = unique_replacement(audit, SanitizedIdentityKind::ReceiverSerialNumber)?;
            if value.len() != 4 {
                bail!("sanitized Unifying receiver identity is not four bytes");
            }
            Ok(DeviceRoute::Unifying {
                receiver_uid: uppercase_hex(value),
                slot: *slot,
            })
        }
        DeviceRoute::Direct {
            vendor_id,
            product_id,
        } => Ok(DeviceRoute::Direct {
            vendor_id: *vendor_id,
            product_id: *product_id,
        }),
        DeviceRoute::RawHid { .. } => {
            bail!("raw HID routes are outside HID++ fixture case capture")
        }
    }
}

fn unique_replacement(audit: &HidCassetteAudit, kind: SanitizedIdentityKind) -> Result<&[u8]> {
    let mut replacements = audit
        .replacements
        .iter()
        .filter(|replacement| replacement.kind == kind);
    let value = replacements
        .next()
        .map(|replacement| replacement.synthetic_value.as_slice())
        .ok_or_else(|| anyhow::anyhow!("cassette audit has no sanitized receiver identity"))?;
    if replacements.next().is_some() {
        bail!("cassette audit has multiple sanitized receiver identities");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use openlogi_device::fixture::{
        CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
    };
    use openlogi_device::write::FeatureEntry;
    use openlogi_hid::FeatureType;
    use openlogi_hid::recording::IdentityReplacement;

    use super::*;

    fn target(route: DeviceRoute, product_id: u16) -> TargetCandidate {
        TargetCandidate {
            route,
            name: "Synthetic target".to_string(),
            receiver_vendor_id: 0x046d,
            receiver_product_id: product_id,
        }
    }

    fn audit(kind: SanitizedIdentityKind, value: &[u8]) -> HidCassetteAudit {
        HidCassetteAudit {
            replacements: vec![IdentityReplacement {
                kind,
                synthetic_value: value.to_vec(),
                occurrences: 1,
            }],
        }
    }

    #[test]
    fn derives_only_sanitized_bolt_unifying_and_direct_routes() {
        let bolt = derive_replay_route(
            &DeviceRoute::Bolt {
                receiver_uid: "raw-bolt-id".to_string(),
                slot: 2,
            },
            &audit(SanitizedIdentityKind::ReceiverUniqueId, b"0000000000000001"),
        )
        .expect("sanitized Bolt route");
        assert_eq!(
            bolt,
            DeviceRoute::Bolt {
                receiver_uid: "0000000000000001".to_string(),
                slot: 2,
            }
        );

        let unifying = derive_replay_route(
            &DeviceRoute::Unifying {
                receiver_uid: "raw-unifying-id".to_string(),
                slot: 3,
            },
            &audit(
                SanitizedIdentityKind::ReceiverSerialNumber,
                &[0xa0, 0x00, 0x00, 0x01],
            ),
        )
        .expect("sanitized Unifying route");
        assert_eq!(
            unifying,
            DeviceRoute::Unifying {
                receiver_uid: "A0000001".to_string(),
                slot: 3,
            }
        );

        let direct = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        assert_eq!(
            derive_replay_route(&direct, &HidCassetteAudit::default())
                .expect("direct route retains only VID/PID"),
            direct
        );
    }

    #[test]
    fn receiver_route_derivation_rejects_missing_or_ambiguous_audit_identity() {
        let route = DeviceRoute::Bolt {
            receiver_uid: "raw".to_string(),
            slot: 1,
        };
        derive_replay_route(&route, &HidCassetteAudit::default())
            .expect_err("receiver replay requires sanitized identity evidence");

        let replacement = IdentityReplacement {
            kind: SanitizedIdentityKind::ReceiverUniqueId,
            synthetic_value: b"0000000000000001".to_vec(),
            occurrences: 1,
        };
        let ambiguous = HidCassetteAudit {
            replacements: vec![replacement.clone(), replacement],
        };
        derive_replay_route(&route, &ambiguous)
            .expect_err("multiple sanitized receiver identities are ambiguous");
    }

    #[test]
    fn passing_candidate_selection_rejects_zero_and_multiple() {
        require_single_passing_candidate(Vec::new())
            .expect_err("zero passing candidates fail closed");

        let candidate = SanitizedCandidate {
            cassette: feature_table_cassette(),
            audit: HidCassetteAudit::default(),
        };
        require_single_passing_candidate(vec![candidate.clone(), candidate])
            .expect_err("multiple passing candidates fail closed");
    }

    #[tokio::test]
    async fn production_feature_table_self_replay_matches_and_requires_completion() {
        let target = target(
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb35b,
            },
            0xb35b,
        );
        let candidate = SanitizedCandidate {
            cassette: feature_table_cassette(),
            audit: HidCassetteAudit::default(),
        };
        let captured = SemanticObservation::FeatureTable(Ok(vec![FeatureEntry {
            id: 0,
            version: 0,
            typ: FeatureType::empty(),
        }]));

        assert!(
            candidate_passes(
                FixtureOperation::FeatureTable,
                &target,
                &captured,
                &candidate,
            )
            .await,
            "the same production read must match semantically and consume the cassette"
        );
        let mut incomplete = candidate.clone();
        incomplete.cassette.exchanges.push(h20(
            short(0xff, 0x00, 0x00, [0x12, 0x34, 0]),
            short(0xff, 0x00, 0x00, [0x02, 0, 0]),
        ));
        assert!(
            !candidate_passes(
                FixtureOperation::FeatureTable,
                &target,
                &captured,
                &incomplete,
            )
            .await,
            "an unused required exchange must fail the completion gate"
        );
        let selected = select_self_replaying(
            FixtureOperation::FeatureTable,
            &target,
            &captured,
            vec![candidate],
        )
        .await
        .expect("exactly one candidate self-replays");
        assert_eq!(selected.name, "feature-table-self-replay");
    }

    fn feature_table_cassette() -> HidCassette {
        HidCassette {
            schema_version: FIXTURE_SCHEMA_VERSION,
            name: "feature-table-self-replay".to_string(),
            channel: "direct-feature-table".to_string(),
            report_support: ReportSupport::ShortAndLong,
            exchanges: vec![
                h20(
                    short(0xff, 0x00, 0x10, [0, 0, 0]),
                    short(0xff, 0x00, 0x10, [4, 0, 0]),
                ),
                h20(
                    short(0xff, 0x00, 0x00, [0x00, 0x01, 0]),
                    short(0xff, 0x00, 0x00, [0x01, 0, 0]),
                ),
                h20(
                    short(0xff, 0x01, 0x00, [0, 0, 0]),
                    short(0xff, 0x01, 0x00, [0, 0, 0]),
                ),
                h20(
                    short(0xff, 0x01, 0x10, [0, 0, 0]),
                    short(0xff, 0x01, 0x10, [0, 0, 0]),
                ),
            ],
        }
    }

    fn h20(request: Vec<u8>, response: Vec<u8>) -> CassetteExchange {
        CassetteExchange {
            request_match: RequestMatch::Hidpp20,
            request,
            response: Some(response),
            required: true,
        }
    }

    fn short(device: u8, feature: u8, function: u8, payload: [u8; 3]) -> Vec<u8> {
        vec![
            0x10, device, feature, function, payload[0], payload[1], payload[2],
        ]
    }
}
