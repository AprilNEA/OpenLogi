//! Preflight and compensated ownership for `0x8071` solid-colour writes.

use std::sync::Arc;

use hidpp::channel::{ChannelError, HidppChannel};
use hidpp::device::Device;
use hidpp::feature::rgb_effects::{
    CLUSTER_EFFECT_PARAM_COUNT, PowerModeTarget, RgbEffectsFeature, RgbPersistence, SwControlFlags,
};
use hidpp::protocol::v20::Hidpp20Error;
use openlogi_core::color::Rgb;
use tracing::warn;

use super::{
    FRAME_GAP, HidppOperation, WriteError, WriteScope, classify_hidpp_error, open_feature,
};

const FEATURE: u16 = 0x8071;
// The effect ID is stable; each cluster's index for it is discovered separately.
const STATIC_RGB: u16 = 1;

#[cfg(test)]
mod tests;

pub(super) async fn apply(
    channel: &Arc<HidppChannel>,
    index: u8,
    color: Rgb,
    scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
) -> Result<(), WriteError> {
    let feature = scope
        .read(async {
            let mut device = Device::new(Arc::clone(channel), index)
                .await
                .map_err(|_| WriteError::DeviceUnreachable { index })?;
            open_feature::<RgbEffectsFeature>(&mut device).await
        })
        .await?;
    let effects = discover(&feature, scope).await?;
    let previous = scope
        .read(async { feature.get_sw_control().await.map_err(classify) })
        .await?;
    scope.check()?;

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
        let (r, g, b) = color.components();
        let mut params = [0; CLUSTER_EFFECT_PARAM_COUNT];
        params[..3].copy_from_slice(&[r, g, b]);
        for (cluster, effect) in effects {
            scope.check()?;
            feature
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
    Ok(())
}

async fn discover(
    feature: &RgbEffectsFeature,
    scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
) -> Result<Vec<(u8, u8)>, WriteError> {
    let info = scope
        .read(async { feature.get_device_info().await.map_err(classify) })
        .await?;
    let mut effects = Vec::with_capacity(usize::from(info.cluster_count));
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
        let effect = found.ok_or(WriteError::FeatureUnsupported {
            feature_hex: FEATURE,
        })?;
        effects.push((cluster, effect));
    }
    if effects.is_empty() {
        return Err(WriteError::FeatureUnsupported {
            feature_hex: FEATURE,
        });
    }
    Ok(effects)
}

fn classify(error: Hidpp20Error) -> WriteError {
    match error {
        Hidpp20Error::Channel(ChannelError::Timeout) => WriteError::RequestTimedOut {
            operation: HidppOperation::Lighting,
        },
        other => classify_hidpp_error(other, HidppOperation::Lighting, FEATURE),
    }
}
