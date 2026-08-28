//! Per-route host lighting loops.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openlogi_core::config::Lighting;
use openlogi_core::hid::LightingEffect;
use openlogi_hid::{
    CaptureChannel, ChannelRegistry, DeviceRoute, LightingApply, WriteError, apply_lighting_on,
    per_key_zone_ids_on, set_led_software_control_on, set_per_key_colors_on, set_zonal_colors_on,
};
use tracing::{debug, warn};

use super::render::{self, HostInputs};
use super::{audio, millis_since_press, screen};
use crate::hardware::{DeviceOp, authoritative_channel, one_shot_runtime};
use crate::orchestrator::SharedRuntime;
use crate::receiver_access::ReceiverAccess;

const FRAME: Duration = Duration::from_millis(50);

#[derive(Default)]
pub(super) struct Sessions {
    stops: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl Sessions {
    pub(super) fn stop(&self, route: &DeviceRoute) {
        if let Ok(mut stops) = self.stops.lock()
            && let Some(stop) = stops.remove(&route.to_string())
        {
            stop.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn apply(&self, shared: &SharedRuntime, route: DeviceRoute, lighting: Lighting) {
        let stop = self.install_stop(&route);
        spawn_apply(
            Arc::clone(&shared.capture_channel),
            shared.channel_registry.clone(),
            shared.receiver_access.clone(),
            route,
            lighting,
            stop,
        );
    }

    pub(super) fn apply_op(&self, op: &DeviceOp<'_>, lighting: Lighting) {
        let stop = self.install_stop(&op.route);
        spawn_apply(
            Arc::clone(op.capture),
            op.registry.clone(),
            op.receiver_access.clone(),
            op.route.clone(),
            lighting,
            stop,
        );
    }

    fn install_stop(&self, route: &DeviceRoute) -> Arc<AtomicBool> {
        let stop = Arc::new(AtomicBool::new(false));
        if let Ok(mut stops) = self.stops.lock()
            && let Some(previous) = stops.insert(route.to_string(), Arc::clone(&stop))
        {
            previous.store(true, Ordering::Relaxed);
        }
        stop
    }
}

fn spawn_apply(
    capture: CaptureChannel,
    registry: ChannelRegistry,
    receiver_access: ReceiverAccess,
    route: DeviceRoute,
    lighting: Lighting,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let Some(rt) = one_shot_runtime("lighting session") else {
            return;
        };
        rt.block_on(run_apply(
            capture,
            registry,
            receiver_access,
            route,
            lighting,
            stop,
        ));
    });
}

async fn run_apply(
    capture: CaptureChannel,
    registry: ChannelRegistry,
    receiver_access: ReceiverAccess,
    route: DeviceRoute,
    lighting: Lighting,
    stop: Arc<AtomicBool>,
) {
    let apply = {
        let _lease = receiver_access.acquire_for_io().await;
        let Ok(shared) = authoritative_channel(Some(&capture), &registry, &route) else {
            debug!(%route, "lighting apply skipped — no channel");
            return;
        };
        match apply_lighting_on(&shared, &lighting).await {
            Ok(apply) => apply,
            Err(error) => {
                warn!(error = ?error, %route, "lighting apply failed");
                return;
            }
        }
    };
    if apply != LightingApply::Host || !lighting.enabled || stop.load(Ordering::Relaxed) {
        return;
    }
    run_host_loop(capture, registry, receiver_access, route, lighting, stop).await;
}

async fn run_host_loop(
    capture: CaptureChannel,
    registry: ChannelRegistry,
    receiver_access: ReceiverAccess,
    route: DeviceRoute,
    lighting: Lighting,
    stop: Arc<AtomicBool>,
) {
    let started = Instant::now();
    let mut zone_ids: Option<(bool, Vec<u8>)> = None;
    let mut control_taken = false;
    while !stop.load(Ordering::Relaxed) {
        {
            let _lease = receiver_access.acquire_for_io().await;
            let Ok(shared) = authoritative_channel(Some(&capture), &registry, &route) else {
                debug!(%route, "host lighting stopped — device gone");
                break;
            };
            if !control_taken {
                if let Err(error) = set_led_software_control_on(&shared, true).await {
                    warn!(error = ?error, %route, "could not take software LED control");
                    break;
                }
                control_taken = true;
            }
            if zone_ids.is_none() {
                zone_ids = Some(discover_zones(&shared, &lighting).await);
            }
            let Some((per_key, ids)) = zone_ids.as_ref() else {
                break;
            };
            if ids.is_empty() {
                break;
            }
            let seconds = started.elapsed().as_secs_f32();
            let inputs = HostInputs {
                screen: (lighting.effect == LightingEffect::ScreenSampler)
                    .then(screen::sample_primary)
                    .flatten(),
                audio_rms: audio::rms(),
                audio_bands: audio::bands(),
                press_age_ms: millis_since_press(),
            };
            let frame = render::frame(
                lighting.effect,
                seconds,
                lighting.speed,
                lighting.color.components(),
                lighting.brightness,
                ids,
                *per_key,
                &inputs,
            );
            let result = if *per_key {
                set_per_key_colors_on(&shared, &frame).await
            } else {
                set_zonal_colors_on(&shared, &frame).await
            };
            if let Err(error) = result {
                if matches!(
                    error,
                    WriteError::DeviceNotFound | WriteError::DeviceUnreachable { .. }
                ) {
                    break;
                }
                debug!(error = ?error, %route, "host lighting frame failed");
            }
        }
        tokio::time::sleep(FRAME).await;
    }
}

async fn discover_zones(
    shared: &openlogi_hid::SharedChannel,
    lighting: &Lighting,
) -> (bool, Vec<u8>) {
    match per_key_zone_ids_on(shared).await {
        Ok(ids) if !ids.is_empty() => (true, ids),
        _ => {
            let ids = if lighting.zones.is_empty() {
                (0..4).collect()
            } else {
                lighting.zones.clone()
            };
            (false, ids)
        }
    }
}
