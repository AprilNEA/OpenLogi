//! Host-drawn per-key animation: a fresh `0x8081` frame for every tick, for
//! effects a keyboard's firmware does not run itself (a wave moving across the
//! keys, with chosen keys held still). [`KeyFrames`] is the agent's stream;
//! [`animate_per_key`] runs one end to end for `diag wave`.

use std::collections::BTreeMap;
use std::time::Duration;

use hidpp::feature::CreatableFeature as _;
use hidpp::feature::per_key_lighting::{FramePersistence, MAX_SINGLE_VALUE_ZONES, Rgb};

use super::{
    PerKeyV2Target, RgbControlLease, WriteScope, ZONE_WALK_BUDGET, hidpp_rgb, release_rgb_control,
    retry_busy, rgb,
};
use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use crate::write::{WriteError, with_route};

/// The shortest time between two frames, so a fast link does not paint faster
/// than an eye can tell apart.
const MIN_FRAME_TIME: Duration = Duration::from_millis(33);

/// How an animation run went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationRun {
    /// Frames painted and committed.
    pub frames: u32,
    /// Time from the first frame to the end of the last.
    pub elapsed: Duration,
    /// The requests the busiest frame took.
    pub max_requests: usize,
}

/// Paint a frame from `render` again and again until `keep_going` says stop,
/// then hand lighting back to the firmware. `render` gets the time since the
/// start and returns the colour of each key by HID usage; a key it leaves out
/// is dark. With `delta`, a frame after the first sends only the zones whose
/// colour changed, which holds only where the keyboard keeps the last frame
/// between commits.
///
/// Needs `0x8081` and a known zone map. Diagnostic only: it does not take the
/// lighting lock the agent's writes use, so run it with the agent stopped.
pub async fn animate_per_key(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    render: impl FnMut(Duration) -> Vec<(u8, openlogi_core::color::Rgb)>,
    keep_going: impl Fn() -> bool,
    delta: bool,
) -> Result<AnimationRun, WriteError> {
    with_route(backend, route, |channel| async move {
        let shared = SharedChannel::new(channel, route.clone());
        animate_on(&shared, render, keep_going, delta).await
    })
    .await
}

async fn animate_on(
    shared: &SharedChannel,
    mut render: impl FnMut(Duration) -> Vec<(u8, openlogi_core::color::Rgb)>,
    keep_going: impl Fn() -> bool,
    delta: bool,
) -> Result<AnimationRun, WriteError> {
    let channel = shared.channel();
    let index = shared.device_index();
    let scope = WriteScope {
        deadline: tokio::time::Instant::now() + ZONE_WALK_BUDGET,
        cancelled: || false,
        available: || true,
    };
    let engine = rgb::probe(channel, index, &scope).await?;
    let target = scope.read(PerKeyV2Target::open(channel, index)).await?;
    if target.zone_map.is_none() {
        return Err(WriteError::FeatureUnsupported {
            feature_hex: hidpp::feature::per_key_lighting::PerKeyLightingFeature::ID,
        });
    }
    let mut run = AnimationRun {
        frames: 0,
        elapsed: Duration::ZERO,
        max_requests: 0,
    };
    let start = tokio::time::Instant::now();
    let mut shown: Option<BTreeMap<u8, Rgb>> = None;
    let mut frames = async || {
        while keep_going() {
            let tick = tokio::time::Instant::now();
            let keys = render(tick - start);
            let zones = target.key_zones(&keys);
            let previous = if delta { shown.as_ref() } else { None };
            let (requests, frame) = target.paint_zones(&zones, previous, &scope).await?;
            shown = Some(frame);
            run.frames += 1;
            run.max_requests = run.max_requests.max(requests);
            if let Some(rest) = MIN_FRAME_TIME.checked_sub(tick.elapsed()) {
                tokio::time::sleep(rest).await;
            }
        }
        run.elapsed = start.elapsed();
        Ok(())
    };
    match engine {
        None => frames().await?,
        Some(engine) => {
            let black = openlogi_core::color::Rgb::new(0, 0, 0);
            let lease = engine
                .under_per_key_control(index, black, &scope, frames)
                .await?;
            release_rgb_control(shared, lease, || true).await?;
        }
    }
    Ok(run)
}

/// A stream of per-key frames on one keyboard, for an effect the host draws.
/// [`Self::open`] only reads; the first [`Self::paint`] takes software
/// control through the `0x8071` effect engine, when the keyboard has one, and
/// the ones after it only paint.
///
/// Each call is one bounded transaction: the owner MUST poll it to completion
/// and hold the route's lighting lock, as for [`super::LightingWrite::apply_on`].
pub struct KeyFrames {
    target: PerKeyV2Target,
    engine: Option<rgb::RgbEngine>,
    claimed: bool,
}

impl KeyFrames {
    /// Probe the keyboard on `shared` without changing anything.
    /// `FeatureUnsupported` when it has no `0x8081`, or is not a keyboard
    /// whose zones and key positions [`openlogi_device_registry::keyboard`]
    /// knows.
    pub async fn open(
        shared: &SharedChannel,
        available: impl Fn() -> bool,
    ) -> Result<Self, WriteError> {
        let channel = shared.channel();
        let index = shared.device_index();
        let scope = frame_scope(|| false, available);
        let engine = rgb::probe(channel, index, &scope).await?;
        let target = scope.read(PerKeyV2Target::open(channel, index)).await?;
        if target.zone_map.is_none() || target.columns.is_none() {
            return Err(WriteError::FeatureUnsupported {
                feature_hex: hidpp::feature::per_key_lighting::PerKeyLightingFeature::ID,
            });
        }
        Ok(Self {
            target,
            engine,
            claimed: false,
        })
    }

    /// Each key's horizontal centre, `(HID usage, 0..=1000 left to right)`.
    #[must_use]
    pub fn columns(&self) -> &'static [(u8, u16)] {
        self.target.columns.unwrap_or_default()
    }

    /// Paint `keys` (HID usage and colour; a key left out is dark) as one
    /// frame. The first frame on a keyboard with an effect engine takes
    /// software control and returns its lease, owed back to the firmware.
    pub async fn paint(
        &mut self,
        shared: &SharedChannel,
        keys: &[(u8, openlogi_core::color::Rgb)],
        cancelled: impl Fn() -> bool,
        available: impl Fn() -> bool,
    ) -> Result<Option<RgbControlLease>, WriteError> {
        let index = shared.device_index();
        let scope = frame_scope(cancelled, available);
        let zones = self.target.key_zones(keys);
        let target = &self.target;
        let lease = if let (Some(engine), false) = (&self.engine, self.claimed) {
            let black = openlogi_core::color::Rgb::new(0, 0, 0);
            Some(
                engine
                    .apply_per_key(index, black, &scope, async || {
                        target.paint_zones(&zones, None, &scope).await.map(drop)
                    })
                    .await?,
            )
        } else {
            scope
                .read(async { target.paint_zones(&zones, None, &scope).await })
                .await?;
            None
        };
        self.claimed = true;
        Ok(lease)
    }
}

/// One frame's budget: a frame is a handful of requests, so five seconds
/// only runs out when the keyboard stops answering.
fn frame_scope<C: Fn() -> bool, A: Fn() -> bool>(cancelled: C, available: A) -> WriteScope<C, A> {
    WriteScope {
        deadline: tokio::time::Instant::now() + Duration::from_secs(5),
        cancelled,
        available,
    }
}

impl PerKeyV2Target {
    /// Paint each present zone its colour in `zones`, dark where it has none,
    /// grouping zones that share a colour, and commit the frame. Given the
    /// `previous` frame, only zones whose colour changed are sent. Returns the
    /// requests it took and the frame now shown.
    async fn paint_zones(
        &self,
        zones: &[(u8, Rgb)],
        previous: Option<&BTreeMap<u8, Rgb>>,
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
    ) -> Result<(usize, BTreeMap<u8, Rgb>), WriteError> {
        let dark = hidpp_rgb(openlogi_core::color::Rgb::new(0, 0, 0));
        let mut by_zone: BTreeMap<u8, Rgb> = self.zones.iter().map(|&zone| (zone, dark)).collect();
        for &(zone, color) in zones {
            by_zone.insert(zone, color);
        }
        let mut by_color: Vec<(Rgb, Vec<u8>)> = Vec::new();
        for (&zone, &color) in &by_zone {
            if previous.and_then(|frame| frame.get(&zone)) == Some(&color) {
                continue;
            }
            match by_color.iter_mut().find(|(known, _)| *known == color) {
                Some((_, zones)) => zones.push(zone),
                None => by_color.push((color, vec![zone])),
            }
        }
        let feature = &self.feature;
        let mut requests = 0;
        for (color, zones) in &by_color {
            for chunk in zones.chunks(MAX_SINGLE_VALUE_ZONES) {
                scope.check()?;
                retry_busy(scope, async || {
                    feature.set_rgb_zones_single_value(*color, chunk).await
                })
                .await?;
                requests += 1;
            }
        }
        scope.check()?;
        retry_busy(scope, async || {
            feature.frame_end(FramePersistence::Volatile, 0, 0).await
        })
        .await?;
        Ok((requests + 1, by_zone))
    }
}
