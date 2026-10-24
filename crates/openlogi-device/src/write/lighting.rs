use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hidpp::{
    channel::{ChannelError, HidppChannel},
    device::Device,
    feature::{
        CreatableFeature,
        color_led_effects::{
            BreathingWaveform, ColorLedEffectsFeature, EffectIntensity, Persistence,
            ZoneEffectParams,
        },
        per_key_lighting::{
            FramePersistence, MAX_SINGLE_VALUE_ZONES, PerKeyLightingFeature, Rgb,
            ZONE_PRESENCE_PAGE_LEN, ZonePresencePage,
        },
        root::RootFeature,
    },
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};
use openlogi_core::config::{Lighting, LightingEffect};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

mod rgb;

pub use rgb::RgbControlLease;

/// HID++ `PerKeyLighting` (`0x8080`) — streams each key's colour individually.
/// Its feature *index* varies per device, so it's resolved at runtime.
const PER_KEY_LIGHTING_FEATURE: u16 = 0x8080;
/// HID++ `ColorLedEffects` (`0x8070`) — the keyboard's effect engine. Writing a
/// *fixed* effect here replaces a running onboard profile, which a per-key
/// (`0x8080`) write can't override on G-series keyboards (the firmware keeps
/// replaying its stored effect). Preferred for a solid colour for that reason.
const COLOR_LED_EFFECTS_FEATURE: u16 = 0x8070;

// HID++ 2.0 report id 0x12 is the 64-byte "very long" report that streams a
// batch of (keyID, R, G, B) entries. The frame commit is an ordinary 20-byte
// request and goes through `send_v20`.
const REPORT_SET_KEYS: u8 = 0x12;
// Function byte = `function_id << 4 | software_id`. Software id 0xa just tags our
// requests; for 0x8080: function 0x3 streams a key range, 0x5 commits the frame.
const SW_ID: u8 = 0x0a;
const FN_SET_KEY_RANGE: u8 = 0x3;
const FN_FRAME_END: u8 = 0x5;
// Fixed bytes of the "set key range" payload: a mode flag (byte 5) and the
// most entries one frame carries, which is also the chunk size below. Bytes
// 6..8 hold each frame's own big-endian entry count.
const SET_RANGE_MODE: u8 = 0x01;
const KEYS_PER_FRAME: u8 = 0x0e;

// 0x8070 `ColorLedEffects`: the fixed/static single colour is applied
// volatilely (RAM only) so it shows live and overrides the running onboard
// profile without touching flash. Reboot survival comes from the agent
// re-applying the saved colour on device arrival (orchestrator reapply),
// avoiding flash wear on every colour pick.
//
// Each zone lists its own effects, so the fixed colour's index is read from
// that list. A zone whose list cannot be read is left alone, because no index
// there is known to be the fixed colour.
//
// Zones are paced apart because the controller can drop closely-spaced reports.
const FRAME_GAP: Duration = Duration::from_millis(8);

/// Which HID++ lighting path drives a solid keyboard colour. [`Auto`] is what
/// the GUI/agent use; the explicit variants exist for the `diag` A/B test.
///
/// [`Auto`]: LightingMethod::Auto
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightingMethod {
    /// Prefer `ColorLedEffects` (`0x8070`), then `RgbEffects` (`0x8071`),
    /// `PerKeyLighting2` (`0x8081`) and `PerKeyLighting` (`0x8080`).
    Auto,
    /// Force `ColorLedEffects` (`0x8070`) — the fixed-effect override.
    Effects,
    /// Force `PerKeyLighting` (`0x8080`) — the raw per-key stream.
    PerKey,
    /// Force `PerKeyLighting2` (`0x8081`) — the zone-addressed successor to
    /// `0x8080`.
    PerKeyV2,
}

/// Set a device to a solid `(r, g, b)` colour, choosing the HID++ path
/// automatically: `0x8070` → `0x8071` → `0x8081` → `0x8080`.
///
/// Drive this future to completion; native callers should use the owned
/// lighting job in `openlogi-hid` so cancellation cannot discard RGB rollback.
pub async fn set_keyboard_color(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    set_keyboard_color_with(backend, route, LightingMethod::Auto, r, g, b).await
}

/// [`set_keyboard_color`] with an explicit [`LightingMethod`]. Unsupported
/// discovery permits fallback; failures after RGB ownership is claimed do not.
/// Like [`LightingWrite::apply_on`], this future must be driven to completion.
pub async fn set_keyboard_color_with(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    method: LightingMethod,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    LightingWrite {
        method,
        color: openlogi_core::color::Rgb::new(r, g, b),
        animation: LightingAnimation::Static,
    }
    .apply(backend, route, || false, || true)
    .await
    .map(drop)
}

/// One complete lighting transaction, including RGB ownership rollback.
#[derive(Clone, Copy)]
pub struct LightingWrite {
    /// Preferred protocol path.
    pub method: LightingMethod,
    /// The colour after applying the configured brightness.
    pub color: openlogi_core::color::Rgb,
    /// How [`Self::color`] animates. Only `0x8070` plays an animation; every
    /// other path, and a device whose zones list none, shows the colour.
    pub animation: LightingAnimation,
}

/// The animation a [`LightingWrite`] asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightingAnimation {
    /// The colour held steady.
    #[default]
    Static,
    /// The colour fading in and out once every `period_ms`.
    Breathing {
        /// One breath, in milliseconds.
        period_ms: u16,
    },
    /// The colour wheel, once every `period_ms`, at `intensity` — it has no
    /// colour to carry the brightness.
    ColorCycle {
        /// One turn of the colour wheel, in milliseconds.
        period_ms: u16,
        /// The configured brightness.
        intensity: EffectIntensity,
    },
}

impl LightingWrite {
    /// The write that shows `lighting`, choosing the protocol path
    /// automatically. Off, or at zero brightness, is a black static colour.
    /// Brightness scales the colour, except in a colour cycle, which has no
    /// colour and takes it as the effect's intensity.
    #[must_use]
    pub fn for_settings(lighting: &Lighting) -> Self {
        let dark = Self {
            method: LightingMethod::Auto,
            color: openlogi_core::color::Rgb::new(0, 0, 0),
            animation: LightingAnimation::Static,
        };
        if !lighting.enabled || lighting.brightness == 0 {
            return dark;
        }
        let (r, g, b) = lighting.color.components();
        let scale =
            |c: u8| u8::try_from(u16::from(c) * u16::from(lighting.brightness) / 100).unwrap_or(c);
        let period_ms = lighting.effect_period_ms;
        let animation = match lighting.effect {
            LightingEffect::Static => LightingAnimation::Static,
            LightingEffect::Breathing => LightingAnimation::Breathing { period_ms },
            LightingEffect::ColorCycle => LightingAnimation::ColorCycle {
                period_ms,
                intensity: EffectIntensity::from_percent(lighting.brightness),
            },
        };
        Self {
            color: openlogi_core::color::Rgb::new(scale(r), scale(g), scale(b)),
            animation,
            ..dark
        }
    }

    /// Open a standalone route and apply with the same ownership contract as
    /// [`Self::apply_on`]. The native worker owns this channel through cleanup.
    pub async fn apply(
        self,
        backend: &dyn HidBackend,
        route: &DeviceRoute,
        cancelled: impl Fn() -> bool,
        available: impl Fn() -> bool,
    ) -> Result<Option<RgbControlLease>, WriteError> {
        with_route(backend, route, |channel| async move {
            let shared = SharedChannel::new(channel, route.clone());
            self.apply_on(&shared, cancelled, available).await
        })
        .await
    }

    /// Apply on the supplied channel, observing caller cancellation and channel
    /// availability between requests. `available` must reject retired agent
    /// publications and suspended host I/O, including during compensation.
    ///
    /// The owner MUST poll to completion and serialize lighting transactions on
    /// this route. Do not wrap this future in `timeout` or abort its task. Reads
    /// have a five-second overall deadline; a started RGB native write drains
    /// before cancellation/deadline errors trigger rollback. Its response and
    /// the rollback response each have the channel's own bounded wait. A native
    /// write that never completes cannot safely release ownership on a timer.
    ///
    /// Returns the `0x8071` software control the write left held, if any. A
    /// long-lived host keeps the first lease per device and hands it back with
    /// [`release_rgb_control`] when it leaves; a standalone command leaves the
    /// colour in place.
    pub async fn apply_on(
        self,
        shared: &SharedChannel,
        cancelled: impl Fn() -> bool,
        available: impl Fn() -> bool,
    ) -> Result<Option<RgbControlLease>, WriteError> {
        let scope = WriteScope {
            deadline: tokio::time::Instant::now() + Duration::from_secs(5),
            cancelled,
            available,
        };
        let channel = shared.channel();
        let index = shared.device_index();
        let (r, g, b) = self.color.components();
        let fixed = ZoneEffectParams::FixedColor {
            color: color_led_rgb(self.color),
        };
        let wanted = self.zone_effect().unwrap_or(fixed);
        match self.method {
            LightingMethod::PerKey => {
                return scope
                    .read(set_color_per_key(channel, index, r, g, b))
                    .await
                    .map(|()| None);
            }
            LightingMethod::PerKeyV2 => {
                let engine = rgb::probe(channel, index, &scope).await?;
                return self
                    .per_key_v2(channel, index, engine.as_ref(), &scope)
                    .await;
            }
            LightingMethod::Effects => {
                return scope
                    .read(set_zone_effects(channel, index, wanted, fixed))
                    .await
                    .map(|()| None);
            }
            LightingMethod::Auto => {}
        }
        if wanted != fixed {
            match scope
                .read(set_zone_effects(channel, index, wanted, fixed))
                .await
            {
                Err(WriteError::FeatureUnsupported { feature_hex })
                    if feature_hex == COLOR_LED_EFFECTS_FEATURE =>
                {
                    debug!(
                        animation = ?self.animation,
                        "no 0x8070 zone plays the animation — showing the colour"
                    );
                }
                other => return other.map(|()| None),
            }
        }
        match scope
            .read(set_zone_effects(channel, index, fixed, fixed))
            .await
        {
            Err(WriteError::FeatureUnsupported { feature_hex })
                if feature_hex == COLOR_LED_EFFECTS_FEATURE =>
            {
                debug!("no 0x8070 fixed colour — trying 0x8071 RGB effects");
            }
            other => return other.map(|()| None),
        }
        // Every probe precedes the first claim, so an unusable effect engine
        // falls back without mutating the device.
        let engine = rgb::probe(channel, index, &scope).await?;
        if let Some(engine) = engine.as_ref().filter(|engine| engine.is_complete()) {
            return engine.apply(index, self.color, &scope).await.map(Some);
        }
        debug!("no supported 0x8071 static effect — trying per-key paths");
        match self
            .per_key_v2(channel, index, engine.as_ref(), &scope)
            .await
        {
            Err(WriteError::FeatureUnsupported { feature_hex })
                if feature_hex == PerKeyLightingFeature::ID =>
            {
                scope
                    .read(set_color_per_key(channel, index, r, g, b))
                    .await
                    .map(|()| None)
            }
            other => other,
        }
    }

    /// The `0x8070` parameters of [`Self::animation`], or `None` for a static
    /// colour.
    fn zone_effect(self) -> Option<ZoneEffectParams> {
        match self.animation {
            LightingAnimation::Static => None,
            LightingAnimation::Breathing { period_ms } => Some(ZoneEffectParams::Breathing {
                color: color_led_rgb(self.color),
                period_ms,
                waveform: BreathingWaveform::Default,
                intensity: EffectIntensity::FULL,
            }),
            LightingAnimation::ColorCycle {
                period_ms,
                intensity,
            } => Some(ZoneEffectParams::Cycling {
                period_ms,
                intensity,
            }),
        }
    }

    /// Paint through `0x8081`. With an `0x8071` effect engine the frame goes
    /// under its software control, since the firmware otherwise draws its own
    /// effect over host frames; without one there is nothing to claim. Zone
    /// discovery precedes the claim, so a device without usable zones still
    /// falls back without mutation.
    async fn per_key_v2(
        self,
        channel: &Arc<HidppChannel>,
        index: u8,
        engine: Option<&rgb::RgbEngine>,
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
    ) -> Result<Option<RgbControlLease>, WriteError> {
        let target = scope.read(PerKeyV2Target::open(channel, index)).await?;
        let (r, g, b) = self.color.components();
        let color = Rgb {
            red: r,
            green: g,
            blue: b,
        };
        match engine {
            None => scope
                .read(target.paint(index, color, scope))
                .await
                .map(|()| None),
            Some(engine) => engine
                .apply_per_key(index, self.color, scope, async || {
                    target.paint(index, color, scope).await
                })
                .await
                .map(Some),
        }
    }
}

/// Hand `lease` back to the firmware on `shared`, as a long-lived host does
/// when it leaves. `available` must reject retired publications and suspended
/// host I/O. Bounded by the same five-second read deadline as a write.
///
/// The owner MUST poll to completion and hold the route's lighting lock, as
/// for [`LightingWrite::apply_on`].
pub async fn release_rgb_control(
    shared: &SharedChannel,
    lease: RgbControlLease,
    available: impl Fn() -> bool,
) -> Result<(), WriteError> {
    let scope = WriteScope {
        deadline: tokio::time::Instant::now() + Duration::from_secs(5),
        cancelled: || false,
        available,
    };
    rgb::release(shared.channel(), shared.device_index(), lease, &scope).await
}

struct WriteScope<C, A> {
    deadline: tokio::time::Instant,
    cancelled: C,
    available: A,
}

impl<C: Fn() -> bool, A: Fn() -> bool> WriteScope<C, A> {
    fn check_available(&self) -> Result<(), WriteError> {
        if (self.available)() {
            Ok(())
        } else {
            Err(WriteError::DeviceNotFound)
        }
    }

    fn check(&self) -> Result<(), WriteError> {
        self.check_available()?;
        if (self.cancelled)() || tokio::time::Instant::now() >= self.deadline {
            Err(WriteError::RequestTimedOut {
                operation: HidppOperation::Lighting,
            })
        } else {
            Ok(())
        }
    }

    async fn read<T>(
        &self,
        future: impl std::future::Future<Output = Result<T, WriteError>>,
    ) -> Result<T, WriteError> {
        self.check()?;
        let result = tokio::time::timeout_at(self.deadline, future)
            .await
            .map_err(|_| WriteError::RequestTimedOut {
                operation: HidppOperation::Lighting,
            })?;
        self.check()?;
        result
    }
}

/// Resolve `route`'s runtime feature *index* for HID++ `feature_id`. `Ok(None)`
/// when the device doesn't expose it; the index differs per device, so callers
/// can't hard-code it.
async fn resolve_feature_index(
    channel: &Arc<HidppChannel>,
    device_index: u8,
    feature_id: u16,
) -> Result<Option<u8>, WriteError> {
    let device = Device::new(Arc::clone(channel), device_index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable {
            index: device_index,
        })?;
    let info = device
        .root()
        .get_feature(feature_id)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, feature_id))?;
    Ok(info.map(|i| i.index))
}

/// Play `wanted` through `ColorLedEffects` (`0x8070`): on every zone that
/// lists its effect, and `fallback` on a zone that lists only that one — a
/// zone that cannot breathe still shows the colour. Stored in RAM only
/// (overrides the running onboard profile without touching flash).
/// `FeatureUnsupported` when the device exposes no `0x8070` or no zone lists
/// `wanted`'s effect, including a device that reports no zones.
///
/// For a solid colour `wanted` and `fallback` are the same fixed colour.
///
/// Uses the typed [`ColorLedEffectsFeature`] wrapper: the real zone count is read
/// first so only existing zones are driven (a typed `set_zone_effect` awaits the
/// device's reply, so unlike the former raw fire-and-forget path a write to a
/// non-existent zone would surface as an error rather than a silent no-op).
/// Every reported zone is enumerated, as Logi Options+ and Solaar do; the
/// caller's write deadline, not a zone cap, bounds a malformed count.
/// Every target is located before the first write, so a device that cannot
/// play `wanted` is left untouched for the next path.
async fn set_zone_effects(
    channel: &Arc<HidppChannel>,
    index: u8,
    wanted: ZoneEffectParams,
    fallback: ZoneEffectParams,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ColorLedEffectsFeature>(&mut device).await?;
    let zone_count = feature
        .get_info()
        .await
        .map_err(classify_lighting_error)?
        .zone_count;

    let targets = zone_targets(&feature, index, zone_count, wanted, fallback).await?;
    if !targets.iter().any(|&(_, _, params)| params == wanted) {
        debug!(
            index,
            zone_count,
            effect = ?wanted.effect_id(),
            "no 0x8070 zone lists the effect"
        );
        return Err(WriteError::FeatureUnsupported {
            feature_hex: COLOR_LED_EFFECTS_FEATURE,
        });
    }
    for &(zone, effect, params) in &targets {
        feature
            .set_zone_effect(zone, effect, params.to_params(), Persistence::Volatile)
            .await
            .map_err(classify_lighting_error)?;
        tokio::time::sleep(FRAME_GAP).await;
    }
    debug!(
        index,
        zone_count,
        zones_written = targets.len(),
        ?wanted,
        "set lighting via typed 0x8070"
    );
    Ok(())
}

/// The `(zone, effect index, parameters)` to write in each of zones
/// `0..zone_count`: `wanted` where the zone lists its effect, else `fallback`
/// where it lists that one. Each index is read from the zone's own effect
/// list, as Logi Options+ does — the lists are not in one order across
/// devices (a G203 lists the colour cycle before breathing, a G733 after). A
/// zone listing neither is skipped: a guessed index would drive whatever
/// effect sits there with these bytes as its parameters.
///
/// An effect id this crate does not model is neither, so it is passed over.
/// A zone whose info is rejected (an unmodelled location) is skipped too: its
/// effect list cannot be read, so no index is known to hold either effect.
async fn zone_targets(
    feature: &ColorLedEffectsFeature,
    index: u8,
    zone_count: u8,
    wanted: ZoneEffectParams,
    fallback: ZoneEffectParams,
) -> Result<Vec<(u8, u8, ZoneEffectParams)>, WriteError> {
    let mut targets = Vec::new();
    for zone in 0..zone_count {
        let effects_number = match feature.get_zone_info(zone).await {
            Ok(info) => info.effects_number,
            Err(Hidpp20Error::UnsupportedResponse) => {
                debug!(
                    index,
                    zone, "0x8070 zone info rejected; leaving the zone unwritten"
                );
                continue;
            }
            Err(error) => return Err(classify_lighting_error(error)),
        };
        let mut fallback_index = None;
        let mut target = None;
        for effect in 0..effects_number {
            match feature.get_zone_effect_info(zone, effect).await {
                Ok(info) if info.effect_id == wanted.effect_id() => {
                    target = Some((zone, effect, wanted));
                    break;
                }
                Ok(info) if info.effect_id == fallback.effect_id() => {
                    fallback_index.get_or_insert(effect);
                }
                Ok(_) | Err(Hidpp20Error::UnsupportedResponse) => {}
                Err(error) => return Err(classify_lighting_error(error)),
            }
        }
        targets.extend(target.or_else(|| fallback_index.map(|effect| (zone, effect, fallback))));
    }
    Ok(targets)
}

/// The `0x8070` form of a colour.
fn color_led_rgb(color: openlogi_core::color::Rgb) -> hidpp::feature::color_led_effects::Rgb {
    let (red, green, blue) = color.components();
    hidpp::feature::color_led_effects::Rgb { red, green, blue }
}

/// Classify a HID++ error from the `ColorLedEffects` functions.
fn classify_lighting_error(error: Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::Lighting, ColorLedEffectsFeature::ID)
}

/// A solid colour via `PerKeyLighting2` (`0x8081`): every zone the device
/// reports as present, painted and then committed as one frame.
///
/// `0x8081` supersedes `0x8080`. It addresses *zones* rather than HID key
/// usages and answers each request, so unlike the raw `0x8080` stream a write
/// to a zone the device does not have surfaces as an error instead of being
/// swallowed. Nothing had ever driven it, which left a keyboard exposing only
/// `0x8081` with no way to set its colour at all.
///
/// Committed volatilely for the same reason as the `0x8070` path: the colour
/// shows live without a flash write on every colour pick, and the agent
/// re-applies the saved colour on device arrival.
struct PerKeyV2Target {
    feature: Arc<PerKeyLightingFeature>,
    zones: Vec<u8>,
}

impl PerKeyV2Target {
    /// Open `0x8081` and read its present zones without mutating anything.
    /// `FeatureUnsupported` when the device exposes no `0x8081` or reports no
    /// zones.
    async fn open(channel: &Arc<HidppChannel>, index: u8) -> Result<Self, WriteError> {
        let mut device = Device::new(Arc::clone(channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<PerKeyLightingFeature>(&mut device).await?;
        let zones = present_zones(&feature).await?;
        if zones.is_empty() {
            // The device announces 0x8081 but claims no zones, so there is
            // nothing to paint — and that won't change on retry. Reported as
            // unsupported so `Auto` falls through to the 0x8080 stream.
            debug!(index, "0x8081 reported no present zones");
            return Err(WriteError::FeatureUnsupported {
                feature_hex: PerKeyLightingFeature::ID,
            });
        }
        Ok(Self { feature, zones })
    }

    /// Paint every present zone `color`, then commit the frame. `scope` is
    /// checked before each request, so a retired channel, suspended host I/O,
    /// cancellation or the deadline stops the frame between requests without
    /// cutting one off in flight.
    async fn paint(
        &self,
        index: u8,
        color: Rgb,
        scope: &WriteScope<impl Fn() -> bool, impl Fn() -> bool>,
    ) -> Result<(), WriteError> {
        // One request carries at most MAX_SINGLE_VALUE_ZONES ids and silently
        // ignores the rest, so the chunking is the caller's job.
        for chunk in self.zones.chunks(MAX_SINGLE_VALUE_ZONES) {
            scope.check()?;
            self.feature
                .set_rgb_zones_single_value(color, chunk)
                .await
                .map_err(classify_per_key_v2_error)?;
        }
        scope.check()?;
        self.feature
            .frame_end(FramePersistence::Volatile, 0, 0)
            .await
            .map_err(classify_per_key_v2_error)?;
        debug!(
            index,
            zone_count = self.zones.len(),
            ?color,
            "set keyboard colour via typed 0x8081"
        );
        Ok(())
    }
}

/// Every zone id `0x8081` reports as present, read across all three presence
/// pages.
///
/// Ids `0` and `0xff` are end-of-list sentinels the feature rejects, so they
/// are skipped even if a device sets their bits.
async fn present_zones(feature: &PerKeyLightingFeature) -> Result<Vec<u8>, WriteError> {
    let mut zones = Vec::new();
    for (page, base) in [
        (ZonePresencePage::Zones0To111, 0u16),
        (ZonePresencePage::Zones112To223, 112),
        (ZonePresencePage::Zones224To255, 224),
    ] {
        let bitfield = feature
            .get_rgb_zone_presence(page)
            .await
            .map_err(classify_per_key_v2_error)?;
        collect_present_zones(base, &bitfield, &mut zones);
    }
    Ok(zones)
}

/// Appends the zone ids whose presence bit is set in `bitfield`, a 112-bit
/// field covering ids `base..base + 112` (bit `i` LSB-first within each byte).
///
/// The last page covers only 224..=255, so its high bits are padding; ids past
/// 255 are skipped rather than wrapped. Ids `0` and `0xff` are the feature's
/// end-of-list sentinels and are skipped even if a device sets their bits.
pub(super) fn collect_present_zones(
    base: u16,
    bitfield: &[u8; ZONE_PRESENCE_PAGE_LEN],
    zones: &mut Vec<u8>,
) {
    for (byte_index, byte) in bitfield.iter().enumerate() {
        for bit in 0..8u16 {
            if byte & (1 << bit) == 0 {
                continue;
            }
            let Ok(offset) = u16::try_from(byte_index * 8) else {
                continue;
            };
            let Ok(zone_id) = u8::try_from(base + offset + bit) else {
                continue;
            };
            if !matches!(zone_id, 0 | 0xff) {
                zones.push(zone_id);
            }
        }
    }
}

/// Classify a HID++ error from the `PerKeyLighting2` functions.
fn classify_per_key_v2_error(error: hidpp::protocol::v20::Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::Lighting, PerKeyLightingFeature::ID)
}

/// Set a solid colour via `PerKeyLighting` (`0x8080`): stream every key's colour
/// in 64-byte `0x12` frames, then commit. `FeatureUnsupported` when the device
/// exposes no `0x8080`.
///
/// The frames are written raw: which report id the device answers a 64-byte
/// request with is not established (g810-led never reads a reply), so there is
/// no success reply to wait for. A HID++ feature error names its request in
/// the header, so errors addressed to the frames are collected while they
/// stream. A root ping after the last frame fences them: the device answers
/// in order, so once the ping returns, any error for an earlier frame has been
/// read. A rejected frame fails the write before the commit. The commit is a
/// 20-byte request that awaits its own reply, as Logi Options+'s `FlushLEDS`
/// does.
async fn set_color_per_key(
    channel: &Arc<HidppChannel>,
    device_index: u8,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    let feature_index = resolve_feature_index(channel, device_index, PER_KEY_LIGHTING_FEATURE)
        .await?
        .ok_or(WriteError::FeatureUnsupported {
            feature_hex: PER_KEY_LIGHTING_FEATURE,
        })?;

    let frame = per_key_header(device_index, feature_index, FN_SET_KEY_RANGE);
    let frame_error: Arc<Mutex<Option<Hidpp20Error>>> = Arc::default();
    let _frame_errors = {
        let frame_error = Arc::clone(&frame_error);
        channel.add_msg_listener_guarded(move |message, _| {
            if let Some(error) = v20::Message::from(message).feature_error_for(frame) {
                frame_error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_or_insert(error);
            }
        })
    };

    for report in per_key_frames(device_index, feature_index, r, g, b) {
        let written = channel
            .write_raw_report(&report)
            .await
            .map_err(classify_raw_lighting_error)?;
        if written != report.len() {
            return Err(WriteError::Hidpp(format!(
                "raw lighting report wrote {written} of {} bytes",
                report.len()
            )));
        }
    }

    RootFeature::new(Arc::clone(channel), device_index, 0)
        .ping(0)
        .await
        .map_err(classify_per_key_error)?;
    let rejected = frame_error
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(error) = rejected {
        debug!(
            device_index,
            feature_index,
            ?error,
            "0x8080 rejected a frame; not committing"
        );
        return Err(classify_per_key_error(error));
    }

    channel
        .send_v20(per_key_commit(device_index, feature_index))
        .await
        .map_err(classify_per_key_error)?;
    debug!(
        device_index,
        feature_index, r, g, b, "set keyboard colour via 0x8080"
    );
    Ok(())
}

/// The 64-byte `0x12` frames that set every key to `(r, g, b)`.
pub(super) fn per_key_frames(
    device_index: u8,
    feature_index: u8,
    r: u8,
    g: u8,
    b: u8,
) -> Vec<Vec<u8>> {
    let mut reports = Vec::new();
    // Each 64-byte `0x12` "set group keys" packet carries up to 14
    // `(keyID, R, G, B)` entries; keyIDs are HID usage codes. Cover the whole
    // keyboard usage range (incl. modifiers at `0xe0..`) so every key lights.
    let key_ids: Vec<u8> = (0x00u8..=0xe8).collect();
    for chunk in key_ids.chunks(KEYS_PER_FRAME as usize) {
        let mut rep = vec![0u8; 64];
        rep[0] = REPORT_SET_KEYS;
        rep[1] = device_index;
        rep[2] = feature_index;
        rep[3] = (FN_SET_KEY_RANGE << 4) | SW_ID;
        rep[5] = SET_RANGE_MODE;
        // Only the last frame is short (233 keys = 16 × 14 + 9). Declaring 14
        // there would hand the firmware five zeroed entries as real ones.
        rep[7] = u8::try_from(chunk.len()).unwrap_or(KEYS_PER_FRAME);
        for (i, &key) in chunk.iter().enumerate() {
            let off = 8 + i * 4;
            rep[off] = key;
            rep[off + 1] = r;
            rep[off + 2] = g;
            rep[off + 3] = b;
        }
        reports.push(rep);
    }
    reports
}

/// The `0x8080` frameEnd request that commits the streamed frames.
pub(super) fn per_key_commit(device_index: u8, feature_index: u8) -> v20::Message {
    v20::Message::Long(
        per_key_header(device_index, feature_index, FN_FRAME_END),
        [0; 16],
    )
}

fn per_key_header(device_index: u8, feature_index: u8, function: u8) -> v20::MessageHeader {
    v20::MessageHeader {
        device_index,
        feature_index,
        function_id: U4::from_lo(function),
        software_id: U4::from_lo(SW_ID),
    }
}

/// Classify a HID++ error from the `PerKeyLighting` request path.
fn classify_per_key_error(error: Hidpp20Error) -> WriteError {
    classify_hidpp_error(error, HidppOperation::Lighting, PER_KEY_LIGHTING_FEATURE)
}

fn classify_raw_lighting_error(error: ChannelError) -> WriteError {
    match error {
        ChannelError::Timeout => WriteError::RequestTimedOut {
            operation: HidppOperation::Lighting,
        },
        other => WriteError::Hidpp(format!("{other:?}")),
    }
}

/// Set a solid keyboard colour on an already-open [`SharedChannel`], using
/// [`LightingMethod::Auto`].
pub async fn set_keyboard_color_on(
    shared: &SharedChannel,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    set_keyboard_color_with_on(shared, LightingMethod::Auto, r, g, b).await
}

/// Set a solid keyboard colour on an already-open [`SharedChannel`] with an
/// explicit lighting method. Drive to completion; see [`LightingWrite::apply_on`].
pub async fn set_keyboard_color_with_on(
    shared: &SharedChannel,
    method: LightingMethod,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    LightingWrite {
        method,
        color: openlogi_core::color::Rgb::new(r, g, b),
        animation: LightingAnimation::Static,
    }
    .apply_on(shared, || false, || shared.channel().is_connected())
    .await
    .map(drop)
}
