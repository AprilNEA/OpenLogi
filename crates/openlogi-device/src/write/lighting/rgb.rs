//! Preflight and compensated ownership for `0x8071` software control: solid
//! Static effects, `0x8081` per-key frames painted under the same claim, and
//! the hand-back a long-lived host owes on exit.

use std::sync::Arc;

use hidpp::channel::{ChannelError, HidppChannel};
use hidpp::device::Device;
use hidpp::feature::CreatableFeature;
use hidpp::feature::device_information::DeviceInformationFeature;
use hidpp::feature::rgb_effects::{
    CLUSTER_EFFECT_PARAM_COUNT, EventsNotificationFlags, PowerModeTarget, RgbEffectsFeature,
    RgbPersistence, SwControlFlags,
};
use hidpp::protocol::v20::Hidpp20Error;
use openlogi_core::color::Rgb;
use tracing::{debug, warn};

use super::{
    FRAME_GAP, HidppOperation, WriteError, WriteScope, classify_hidpp_error, open_feature,
};

const FEATURE: u16 = 0x8071;
// The effect ID is stable; each cluster's index for it is discovered separately.
const STATIC_RGB: u16 = 1;

#[cfg(test)]
mod tests;

/// `0x8071` software control a lighting write left asserted, with the
/// control and event flags the device reported before that claim.
///
/// A volatile effect shows only while software control is held, so a
/// successful write keeps it. A long-lived host that leaves owes it back
/// through [`release_rgb_control`](super::release_rgb_control): G HUB and
/// Solaar both return LED control to the firmware on exit. Disconnects and
/// crashes cannot send it; the firmware drops volatile state on power loss.
///
/// Every write also records the unit ID the device reported (`0x0003`), so a
/// later write and the release can tell the claimed device from another one
/// paired to the same receiver slot since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgbControlLease {
    control: SwControlFlags,
    events: EventsNotificationFlags,
    device: Option<[u8; 4]>,
}

impl RgbControlLease {
    /// Whether this lease, from a later write through the same connection,
    /// continues `earlier`'s claim: both writes reached the same device. A
    /// write to another device does not, even one that found control already
    /// held, and its flags replace the recorded ones so that device is still
    /// handed back.
    #[must_use]
    pub fn continues(&self, earlier: &Self) -> bool {
        self.device == earlier.device
    }
}

/// The `0x8071` effect engine and the Static effect each cluster offers,
/// read before any mutation.
pub(super) struct RgbEngine {
    feature: Arc<RgbEffectsFeature>,
    unit: Option<[u8; 4]>,
    statics: Vec<(u8, u8)>,
    complete: bool,
}

/// Open `0x8071`, identify the device, and read every cluster's Static
/// effect. `None` when the device has no RGB effect engine.
///
/// Every write reads the unit ID, including one that will find control
/// already held: a device paired into the slot since the last write may be
/// held by this session's earlier claim or by another host, and its lease
/// must name it. The read is part of the read-only preflight, so a failure
/// leaves nothing to roll back.
pub(super) async fn probe(
    channel: &Arc<HidppChannel>,
    index: u8,
    scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
) -> Result<Option<RgbEngine>, WriteError> {
    let opened = scope
        .read(async {
            let mut device = device(channel, index).await?;
            let feature = open_feature::<RgbEffectsFeature>(&mut device).await?;
            Ok((device, feature))
        })
        .await;
    let (mut device, feature) = match opened {
        Ok(opened) => opened,
        Err(WriteError::FeatureUnsupported {
            feature_hex: FEATURE,
        }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let unit = scope.read(unit_id(&mut device)).await?;
    let (statics, complete) = discover(&feature, scope).await?;
    Ok(Some(RgbEngine {
        feature,
        unit,
        statics,
        complete,
    }))
}

impl RgbEngine {
    /// Whether every cluster offers a Static effect, so one claim colours the
    /// whole device without a per-key frame.
    pub(super) fn is_complete(&self) -> bool {
        self.complete
    }

    /// Colour every cluster with its Static effect. Callers check
    /// [`Self::is_complete`] first: a partial engine cannot colour the device
    /// by itself and must fall back before claiming.
    pub(super) async fn apply(
        &self,
        index: u8,
        color: Rgb,
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
    ) -> Result<RgbControlLease, WriteError> {
        debug_assert!(self.complete, "a partial engine must not claim alone");
        self.under_control(index, scope, async |_| {
            self.paint_statics(color, scope).await
        })
        .await
    }

    /// Paint a `0x8081` frame under software control. Per-key paint is a
    /// sub-mode of the Static effect: while the firmware owns a cluster, it
    /// draws its own effect over host frames. Every cluster that offers Static
    /// gets it as the base colour first. The first frame after a fresh claim
    /// can land before the firmware leaves onboard mode, so it is replayed
    /// once. Solaar's per-key setting follows the same order.
    pub(super) async fn apply_per_key(
        &self,
        index: u8,
        color: Rgb,
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
        paint: impl AsyncFn() -> Result<(), WriteError>,
    ) -> Result<RgbControlLease, WriteError> {
        self.under_control(index, scope, async |fresh| {
            self.paint_statics(color, scope).await?;
            paint().await?;
            if fresh {
                scope.check()?;
                paint().await?;
            }
            Ok(())
        })
        .await
    }

    async fn paint_statics(
        &self,
        color: Rgb,
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
    ) -> Result<(), WriteError> {
        let (r, g, b) = color.components();
        let mut params = [0; CLUSTER_EFFECT_PARAM_COUNT];
        params[..3].copy_from_slice(&[r, g, b]);
        for &(cluster, effect) in &self.statics {
            scope.check()?;
            self.feature
                .set_rgb_cluster_effect(
                    cluster,
                    effect,
                    params,
                    RgbPersistence::VOLATILE,
                    PowerModeTarget::FullPower,
                )
                .await
                .map_err(classify)?;
            tokio::time::sleep(FRAME_GAP).await;
        }
        scope.check()
    }

    /// Snapshot the control flags, claim every cluster, run `body`, and on any
    /// failure restore the snapshot. `body` learns whether this claim took
    /// control from the firmware rather than finding it already held.
    async fn under_control(
        &self,
        index: u8,
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
        body: impl AsyncFnOnce(bool) -> Result<(), WriteError>,
    ) -> Result<RgbControlLease, WriteError> {
        let feature = &self.feature;
        let previous = scope
            .read(async { feature.get_sw_control().await.map_err(classify) })
            .await?;
        scope.check()?;
        let fresh = !previous.control.contains(SwControlFlags::ALL_CLUSTERS);

        // Recovery is owed from BEFORE sending the claim: losing its reply does
        // not mean firmware ignored it. The owner retains this future until all
        // submitted native writes and any compensation finish; no async Drop task.
        let result = async {
            feature
                .set_sw_control(
                    previous.control | SwControlFlags::ALL_CLUSTERS,
                    previous.events,
                )
                .await
                .map_err(classify)?;
            body(fresh).await?;
            scope.check()
        }
        .await;

        if let Err(error) = result {
            // Cancellation/deadline does not cancel compensation. Retirement or
            // suspension does: never send an old snapshot through a replacement.
            let restored = async {
                scope.check_available()?;
                feature
                    .set_sw_control(previous.control, previous.events)
                    .await
                    .map_err(classify)
            }
            .await;
            if let Err(restore_error) = restored {
                warn!(
                    index,
                    ?error,
                    ?restore_error,
                    "RGB control restoration incomplete"
                );
                return Err(restore_error);
            }
            return Err(error);
        }
        // A successful volatile effect needs software control to remain asserted.
        Ok(RgbControlLease {
            control: previous.control,
            events: previous.events,
            device: self.unit,
        })
    }
}

/// Return the cluster control in `lease` to the firmware. Clears only
/// [`SwControlFlags::ALL_CLUSTERS`] and keeps the other flags the device had
/// before the claim. A lease taken while an earlier process still held
/// control, such as an agent that never handed it back, therefore still
/// releases it.
///
/// The lease is released only to the device that still reports the unit ID
/// it recorded. Another device on the route, such as one paired to the same
/// receiver slot since, is not one this lease claimed; it is left alone with
/// [`WriteError::DeviceNotFound`]. A lease from a device without `0x0003`
/// goes to a device that still has none.
pub(super) async fn release(
    channel: &Arc<HidppChannel>,
    index: u8,
    lease: RgbControlLease,
    scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
) -> Result<(), WriteError> {
    let mut device = scope.read(device(channel, index)).await?;
    if scope.read(unit_id(&mut device)).await? != lease.device {
        debug!(
            index,
            "another device answers on the route; leaving its RGB control"
        );
        return Err(WriteError::DeviceNotFound);
    }
    scope.check()?;
    let feature = scope
        .read(open_feature::<RgbEffectsFeature>(&mut device))
        .await?;
    scope.check()?;
    feature
        .set_sw_control(
            lease.control.difference(SwControlFlags::ALL_CLUSTERS),
            lease.events,
        )
        .await
        .map_err(classify)?;
    debug!(index, "returned 0x8071 cluster control to the firmware");
    Ok(())
}

async fn device(channel: &Arc<HidppChannel>, index: u8) -> Result<Device, WriteError> {
    Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })
}

/// The unit ID `device` reports through `0x0003`, which tells two devices
/// on one receiver slot apart. `None` when the device has no
/// `DeviceInformation` feature.
async fn unit_id(device: &mut Device) -> Result<Option<[u8; 4]>, WriteError> {
    let feature = match open_feature::<DeviceInformationFeature>(device).await {
        Ok(feature) => feature,
        Err(WriteError::FeatureUnsupported { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let info = feature
        .get_device_info()
        .await
        .map_err(|error| match error {
            Hidpp20Error::Channel(ChannelError::Timeout) => WriteError::RequestTimedOut {
                operation: HidppOperation::Lighting,
            },
            other => classify_hidpp_error(
                other,
                HidppOperation::Lighting,
                DeviceInformationFeature::ID,
            ),
        })?;
    Ok(Some(info.unit_id))
}

/// Each cluster's Static effect `(cluster, effect index)`, and whether every
/// cluster has one. A device that reports no clusters is never complete.
async fn discover(
    feature: &RgbEffectsFeature,
    scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
) -> Result<(Vec<(u8, u8)>, bool), WriteError> {
    let info = scope
        .read(async { feature.get_device_info().await.map_err(classify) })
        .await?;
    let mut statics = Vec::with_capacity(usize::from(info.cluster_count));
    let mut complete = info.cluster_count > 0;
    for cluster in 0..info.cluster_count {
        let info = scope
            .read(async { feature.get_cluster_info(cluster).await.map_err(classify) })
            .await?;
        let mut found = None;
        for effect in 0..info.effects_number {
            let info = scope
                .read(async {
                    feature
                        .get_effect_info(cluster, effect)
                        .await
                        .map_err(classify)
                })
                .await?;
            if info.effect_id == STATIC_RGB {
                found = Some(effect);
                break;
            }
        }
        match found {
            Some(effect) => statics.push((cluster, effect)),
            None => complete = false,
        }
    }
    Ok((statics, complete))
}

fn classify(error: Hidpp20Error) -> WriteError {
    match error {
        Hidpp20Error::Channel(ChannelError::Timeout) => WriteError::RequestTimedOut {
            operation: HidppOperation::Lighting,
        },
        other => classify_hidpp_error(other, HidppOperation::Lighting, FEATURE),
    }
}
