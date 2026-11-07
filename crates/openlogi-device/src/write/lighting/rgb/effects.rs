//! The effects an `0x8071` engine offers, and trying one by hand, so a person
//! can work out what each effect's parameters do. Diagnostic only.

use std::time::Duration;

use hidpp::feature::rgb_effects::{
    CLUSTER_EFFECT_PARAM_COUNT, ONBOARD_INFO_PARAM_COUNT, PowerModeTarget, RgbPersistence,
    SlotInfoType,
};

use super::super::{WriteScope, release_rgb_control};
use super::{FEATURE, RgbControlLease, RgbEngine, classify, probe};
use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use crate::write::{WriteError, with_route};

/// How long a tried effect may hold the device while a person watches it.
const TRY_BUDGET: Duration = Duration::from_hours(1);

/// One effect a cluster offers, as `getInfo` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbEffectEntry {
    /// The cluster's index.
    pub cluster: u8,
    /// Where the cluster sits on the device (raw `locationEffect`).
    pub location: u16,
    /// The effect's index within the cluster, which `setRgbClusterEffect`
    /// takes.
    pub index: u8,
    /// The effect type (raw `effectID`), the same across clusters.
    pub effect_id: u16,
    /// The effect's capability bits (raw `effectCapabilities`).
    pub capabilities: u16,
    /// The effect's period in milliseconds, or `0` when it reports none.
    pub period: u16,
    /// What the firmware reports about each onboard slot of this effect, by
    /// slot and kind of info: raw bytes, or `None` where it refuses the read.
    pub onboard: Vec<OnboardSlotInfo>,
}

/// One onboard slot read of an effect, as `getInfo` returns it raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardSlotInfo {
    /// The onboard slot.
    pub slot: u8,
    /// The kind of info: `0` slot state, `1` default parameters, `4` the
    /// start of the effect's name.
    pub kind: u8,
    /// The reply's parameter bytes, or `None` when the device refused.
    pub bytes: Option<[u8; ONBOARD_INFO_PARAM_COUNT]>,
}

/// The onboard reads tried per effect: slot state, default parameters and
/// the start of the name, for the first two slots.
const ONBOARD_READS: [(u8, SlotInfoType); 6] = [
    (0, SlotInfoType::SlotState),
    (0, SlotInfoType::Defaults),
    (0, SlotInfoType::EffectName0To10),
    (1, SlotInfoType::SlotState),
    (1, SlotInfoType::Defaults),
    (1, SlotInfoType::EffectName0To10),
];

/// Every effect of every cluster the device on `route` offers, read without
/// changing anything. Empty when it has no `0x8071` engine.
pub async fn rgb_effects(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<Vec<RgbEffectEntry>, WriteError> {
    with_route(backend, route, |channel| async move {
        let shared = SharedChannel::new(channel, route.clone());
        let scope = scope();
        let Some(engine) = probe(shared.channel(), shared.device_index(), &scope).await? else {
            return Ok(Vec::new());
        };
        engine.effects(&scope).await
    })
    .await
}

/// Run effect `effect_id` with `params` on every cluster that offers it,
/// under software control, until `hold` returns; then hand control back to
/// the firmware. Returns how many clusters took the effect: `0` when none
/// offers it, and nothing was changed.
///
/// Diagnostic only: it does not take the lighting lock the agent's writes
/// use, so run it with the agent stopped.
pub async fn try_rgb_effect(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    effect_id: u16,
    params: [u8; CLUSTER_EFFECT_PARAM_COUNT],
    hold: impl AsyncFnOnce(),
) -> Result<usize, WriteError> {
    with_route(backend, route, |channel| async move {
        let shared = SharedChannel::new(channel, route.clone());
        let index = shared.device_index();
        let scope = scope();
        let Some(engine) = probe(shared.channel(), index, &scope).await? else {
            return Err(WriteError::FeatureUnsupported {
                feature_hex: FEATURE,
            });
        };
        let targets = engine.effect_targets(effect_id, &scope).await?;
        if targets.is_empty() {
            return Ok(0);
        }
        let lease = engine
            .under_control(index, &scope, async |_| {
                engine.set_effect(&targets, params, &scope).await?;
                hold().await;
                Ok(())
            })
            .await?;
        release_rgb_control(&shared, lease, || true).await?;
        Ok(targets.len())
    })
    .await
}

fn scope() -> WriteScope<impl Fn() -> bool, impl Fn() -> bool> {
    WriteScope {
        deadline: tokio::time::Instant::now() + TRY_BUDGET,
        cancelled: || false,
        available: || true,
    }
}

impl RgbEngine {
    /// Run effect `effect_id` with `params` on every cluster that offers it,
    /// under software control, and keep that control. `None` when no cluster
    /// offers it: nothing was claimed or changed.
    pub(in crate::write::lighting) async fn apply_animation(
        &self,
        index: u8,
        effect_id: u16,
        params: [u8; CLUSTER_EFFECT_PARAM_COUNT],
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
    ) -> Result<Option<RgbControlLease>, WriteError> {
        let targets = self.effect_targets(effect_id, scope).await?;
        if targets.is_empty() {
            return Ok(None);
        }
        self.under_control(index, scope, async |_| {
            self.set_effect(&targets, params, scope).await
        })
        .await
        .map(Some)
    }

    /// `(cluster, effect index)` of every cluster that offers `effect_id`.
    async fn effect_targets(
        &self,
        effect_id: u16,
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
    ) -> Result<Vec<(u8, u8)>, WriteError> {
        let feature = &self.feature;
        let info = scope
            .read(async { feature.get_device_info().await.map_err(classify) })
            .await?;
        let mut targets = Vec::new();
        for cluster in 0..info.cluster_count {
            let cluster_info = scope
                .read(async { feature.get_cluster_info(cluster).await.map_err(classify) })
                .await?;
            for index in 0..cluster_info.effects_number {
                let effect = scope
                    .read(async {
                        feature
                            .get_effect_info(cluster, index)
                            .await
                            .map_err(classify)
                    })
                    .await?;
                if effect.effect_id == effect_id {
                    targets.push((cluster, index));
                    break;
                }
            }
        }
        Ok(targets)
    }

    async fn set_effect(
        &self,
        targets: &[(u8, u8)],
        params: [u8; CLUSTER_EFFECT_PARAM_COUNT],
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
    ) -> Result<(), WriteError> {
        for &(cluster, effect) in targets {
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
        }
        Ok(())
    }

    async fn effects(
        &self,
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
    ) -> Result<Vec<RgbEffectEntry>, WriteError> {
        let feature = &self.feature;
        let info = scope
            .read(async { feature.get_device_info().await.map_err(classify) })
            .await?;
        let mut effects = Vec::new();
        for cluster in 0..info.cluster_count {
            let cluster_info = scope
                .read(async { feature.get_cluster_info(cluster).await.map_err(classify) })
                .await?;
            for index in 0..cluster_info.effects_number {
                let effect = scope
                    .read(async {
                        feature
                            .get_effect_info(cluster, index)
                            .await
                            .map_err(classify)
                    })
                    .await?;
                let mut onboard = Vec::with_capacity(ONBOARD_READS.len());
                for (slot, kind) in ONBOARD_READS {
                    scope.check()?;
                    // A refusal is an answer here, not a failure of the dump.
                    let bytes = feature
                        .get_onboard_effect_info(cluster, index, slot, kind)
                        .await
                        .ok();
                    onboard.push(OnboardSlotInfo {
                        slot,
                        kind: kind.into(),
                        bytes,
                    });
                }
                effects.push(RgbEffectEntry {
                    cluster,
                    location: cluster_info.location,
                    index,
                    effect_id: effect.effect_id,
                    capabilities: effect.effect_capabilities,
                    period: effect.effect_period,
                    onboard,
                });
            }
        }
        Ok(effects)
    }
}
