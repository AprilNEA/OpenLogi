use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hidpp::protocol::v10::{Message, MessageHeader};
use hidpp::receiver::unifying::{Event as UnifyingEvent, decode_notification};
use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
    ReceiverInfo,
};

use super::cache::{
    CACHE_MISS_GRACE, CacheKey, CacheOutcome, Cached, REFRESH_INTERVAL, backfill_identity,
    is_stale, keep_known_capabilities,
};
use super::events::EventFeatureIndices;
use super::features::ProbedFeatures;
use super::ledger;
use super::probe::{
    NodeProbe, PassContext, ProbeVerdict, assemble_bolt_probe, assemble_unifying_device,
    parse_codename, preferred_direct_codename, probe_one, probe_unifying_slot,
    retry_arrival_trigger, unifying_probe_budget,
};
use super::{
    ChannelCache, Enumerator, ONESHOT_ATTEMPTS, OneShotScan, ProbeTimeouts, ScanPass,
    UNIFYING_CACHED_SLOT_PROBE_TIMEOUT, UNIFYING_SLOT_PROBE_TIMEOUT, retained_nodes,
    routes_for_inventories, settle_probe, settle_unhealthy_node,
};
use crate::backend::{NodeId, NodeInfo};
use crate::channel::scripted::{
    ScriptedBackend, ScriptedOpen, ScriptedRawHidChannel, scripted_channel, scripted_node_info,
};
use crate::host_lock;
use crate::{DIRECT_DEVICE_INDEX, DeviceRoute};

mod backfill;
mod bolt;
mod cache;
mod deferral;
mod publication;
mod retry;
mod unifying;

fn cache_entry() -> Cached {
    Cached {
        probe: ProbedFeatures::default(),
        battery: None,
        events: EventFeatureIndices::default(),
        probed_at: Instant::now(),
    }
}

fn inventory(slots: &[u8]) -> Vec<DeviceInventory> {
    vec![DeviceInventory {
        receiver: ReceiverInfo {
            name: "Unifying Receiver".to_string(),
            vendor_id: 0x046d,
            product_id: 0xc52b,
            unique_id: Some("receiver-1".to_string()),
        },
        paired: slots
            .iter()
            .copied()
            .map(|slot| PairedDevice {
                slot,
                codename: Some(format!("device-{slot}")),
                wpid: Some(0xb000 + u16::from(slot)),
                kind: DeviceKind::Mouse,
                online: true,
                battery: None,
                model_info: None,
                capabilities: None,
            })
            .collect(),
    }]
}

fn bolt_receiver_info() -> ReceiverInfo {
    ReceiverInfo {
        name: "Logi Bolt Receiver".to_string(),
        vendor_id: 0x046d,
        product_id: 0xc548,
        unique_id: Some("bolt-1".to_string()),
    }
}

/// A readable slot's probe result. `Seen` models the fallback a feature-walk
/// timeout produces (#251): the device still surfaces from its pairing-register
/// identity, so a timed-out slot counts as readable here.
fn bolt_slot(slot: u8) -> (PairedDevice, CacheOutcome) {
    (
        PairedDevice {
            slot,
            codename: Some(format!("device-{slot}")),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: None,
            capabilities: None,
        },
        CacheOutcome::Seen(CacheKey::Bolt {
            unit_id: [0, 0, 0, slot],
        }),
    )
}

fn model(unit_id: [u8; 4], serial: Option<&str>) -> DeviceModelInfo {
    DeviceModelInfo {
        entity_count: 1,
        serial_number: serial.map(str::to_string),
        unit_id,
        transports: DeviceTransports::default(),
        model_ids: [0xc09d, 0, 0],
        extended_model_id: 1,
    }
}

fn probed(model_info: Option<DeviceModelInfo>, identity_incomplete: bool) -> ProbedFeatures {
    ProbedFeatures {
        model_info,
        identity_incomplete,
        kind: Some(DeviceKind::Mouse),
        ..ProbedFeatures::default()
    }
}
