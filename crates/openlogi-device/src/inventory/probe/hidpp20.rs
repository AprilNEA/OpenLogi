//! Probing a receiver that speaks HID++ 2.0 itself: no pairing registers and
//! no arrival events, so its device list is whichever slots answer a ping.

use std::{sync::Arc, time::Duration};

use futures_concurrency::future::Join as _;
use hidpp::{
    channel::HidppChannel,
    receiver::hidpp20::{MAX_SLOT, Receiver as Hidpp20Receiver},
};
use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
use tokio::time::timeout;
use tracing::debug;

use super::{NodeProbe, PassContext, ProbeVerdict, RECEIVER_OPERATION_TIMEOUT};
use crate::backend::NodeInfo;
use crate::inventory::cache::{CacheKey, CacheOutcome, probe_or_reuse};
use crate::inventory::features::ProbedFeatures;
use crate::inventory::mappings::resolve_device_kind;
use crate::inventory::probe::unifying::unifying_probe_budget;

/// How long an occupied slot gets to answer its ping. An empty or offline slot
/// never answers, so every probe pays this once (slots are pinged
/// concurrently); an online PRO X3 answers in a few milliseconds.
const SLOT_PING_TIMEOUT: Duration = Duration::from_millis(750);

/// Probe a HID++ 2.0 receiver. Its unit id is the health gate: a receiver that
/// does not answer it is a failed probe. A slot whose device answers a ping is
/// listed online. A silent slot is either empty or holds an offline device, and
/// the receiver cannot tell which: one this probe cache has seen before stays
/// listed as offline, any other is skipped.
pub(super) async fn probe_hidpp20_receiver(
    channel: Arc<HidppChannel>,
    info: NodeInfo,
    receiver: Hidpp20Receiver,
    pass: PassContext<'_>,
) -> NodeProbe {
    let unique_id = match timeout(RECEIVER_OPERATION_TIMEOUT, receiver.get_unique_id()).await {
        Ok(Ok(uid)) => uid,
        Ok(Err(error)) => {
            debug!(?error, "HID++ 2.0 receiver unit-id read failed");
            return NodeProbe::failed();
        }
        Err(_) => {
            debug!(budget = ?RECEIVER_OPERATION_TIMEOUT, "HID++ 2.0 receiver unit-id read timed out");
            return NodeProbe::failed();
        }
    };

    let (paired, outcomes): (Vec<_>, Vec<_>) = (1..=MAX_SLOT)
        .map(|slot| {
            let (channel, receiver, unique_id) = (&channel, &receiver, unique_id.as_str());
            async move {
                let online = matches!(
                    timeout(SLOT_PING_TIMEOUT, receiver.ping_slot(slot)).await,
                    Ok(Ok(()))
                );
                probe_slot(channel, unique_id, slot, online, pass).await
            }
        })
        .collect::<Vec<_>>()
        .join()
        .await
        .into_iter()
        .flatten()
        .unzip();

    NodeProbe {
        inventory: Some(DeviceInventory {
            receiver: ReceiverInfo {
                name: crate::channel::route::receiver_display_name(info.product_id).to_string(),
                vendor_id: info.vendor_id,
                product_id: info.product_id,
                unique_id: Some(unique_id),
            },
            paired,
        }),
        verdict: ProbeVerdict::Healthy { complete: true },
        outcomes,
    }
}

/// Walk the features of the device that answered at `slot`, or replay the
/// cached probe of one that went silent. `None` for a silent slot without a
/// cached device: nothing is known to be paired there.
///
/// The walk is bounded per slot like a Unifying slot's: a device that stops
/// answering mid-walk must not run out the whole receiver budget, which
/// would fail the receiver probe and in time retire a working channel. On a
/// timeout the slot keeps its last-known data.
async fn probe_slot(
    channel: &Arc<HidppChannel>,
    receiver_uid: &str,
    slot: u8,
    online: bool,
    pass: PassContext<'_>,
) -> Option<(PairedDevice, CacheOutcome)> {
    let id = CacheKey::Hidpp20Slot {
        receiver_uid: receiver_uid.to_string(),
        slot,
    };
    let cached = pass.cache.get(&id);
    if !online && cached.is_none() {
        return None;
    }
    debug!(slot, online, "HID++ 2.0 receiver slot");
    let budget = unifying_probe_budget(cached, pass.now, pass.timeouts);
    let probed = timeout(
        budget,
        probe_or_reuse(
            channel,
            slot,
            Some(id.clone()),
            cached,
            online,
            pass.now,
            pass.subscriptions,
        ),
    )
    .await;
    let (probe, outcome) = if let Ok(probed) = probed {
        probed
    } else {
        debug!(
            slot,
            ?budget,
            "HID++ 2.0 slot probe timed out; using cached data if available"
        );
        let probe = cached.map_or_else(ProbedFeatures::default, |entry| entry.probe.clone());
        (probe, CacheOutcome::Seen(id))
    };
    let device = PairedDevice {
        slot,
        codename: probe.marketing_name.clone(),
        // No pairing register to read the wireless PID from.
        wpid: None,
        kind: resolve_device_kind(probe.kind, DeviceKind::Unknown),
        online,
        battery: probe.battery,
        model_info: probe.model_info,
        capabilities: probe.capabilities,
    };
    Some((device, outcome))
}
