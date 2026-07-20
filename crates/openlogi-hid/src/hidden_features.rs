//! Route-level diagnostics for `0x1e00 EnableHiddenFeatures` and the
//! `0x19c0 ForceSensingButton` probe surface (MX Master 4 Action Ring panel).
//!
//! Diag-only plumbing for the CLI: errors stay in a local type and never
//! cross the agent↔GUI IPC, so [`crate::write::WriteError`] (wire format)
//! stays untouched.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use hidpp::device::Device;
use hidpp::feature::enable_hidden_features::EnableHiddenFeaturesFeature;
use hidpp::feature::force_sensing_button::ForceSensingButtonFeature;
use hidpp::feature::reprog_controls::{
    CidReportingChange, ReprogControlsEvent, decode_event as decode_reprog_event,
};
use hidpp::protocol::v20;
use thiserror::Error;

use crate::reprog_controls::{FEATURE_ID as REPROG_FEATURE_ID, ReprogControlsV4};
use crate::route::{DeviceRoute, open_route_channel};
use crate::write::open_feature;

pub use crate::reprog_controls::PANEL_DIAG_ANALYTICS_CIDS as PANEL_ANALYTICS_CIDS;

/// Hard wall-clock budget for one whole diagnostic (open + calls). A cold
/// BTLE link can swallow a request without ever answering, and the underlying
/// channel read has no timeout of its own — without this bound a single call
/// can hang a diagnostic process forever (observed on real hardware).
const DIAG_BUDGET: Duration = Duration::from_secs(8);

async fn bounded<T>(
    fut: impl Future<Output = Result<T, HiddenDiagError>>,
) -> Result<T, HiddenDiagError> {
    tokio::time::timeout(DIAG_BUDGET, fut)
        .await
        .map_err(|_| HiddenDiagError::Hidpp("call timed out (link cold?)".into()))?
}

/// Failure modes for the hidden-features / force-button diagnostics.
#[derive(Debug, Error)]
pub enum HiddenDiagError {
    /// No HID node matched the route.
    #[error("no connected device matched the route")]
    DeviceNotFound,
    /// Transport-level failure opening the route.
    #[error("HID transport error: {0}")]
    Hid(String),
    /// The node opened but the HID++ device index did not answer.
    #[error("device at index {index:#04x} did not respond to HID++")]
    DeviceUnreachable {
        /// HID++ device index that failed to answer.
        index: u8,
    },
    /// A feature lookup or call failed.
    #[error("HID++ error: {0}")]
    Hidpp(String),
}

async fn open_device(route: &DeviceRoute) -> Result<Device, HiddenDiagError> {
    let chan = open_route_channel(route)
        .await
        .map_err(|e| HiddenDiagError::Hid(format!("{e:?}")))?
        .ok_or(HiddenDiagError::DeviceNotFound)?;
    let index = route.device_index();
    Device::new(Arc::clone(&chan), index)
        .await
        .map_err(|_| HiddenDiagError::DeviceUnreachable { index })
}

/// Reads the current `0x1e00` enabled state.
pub async fn hidden_features_enabled(route: &DeviceRoute) -> Result<bool, HiddenDiagError> {
    bounded(async {
        let mut device = open_device(route).await?;
        let feature = open_feature::<EnableHiddenFeaturesFeature>(&mut device)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(e.to_string()))?;
        feature
            .get_enabled()
            .await
            .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))
    })
    .await
}

/// Writes the `0x1e00` enabled state and returns the read-back value.
pub async fn set_hidden_features_enabled(
    route: &DeviceRoute,
    enabled: bool,
) -> Result<bool, HiddenDiagError> {
    bounded(async {
        let mut device = open_device(route).await?;
        let feature = open_feature::<EnableHiddenFeaturesFeature>(&mut device)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(e.to_string()))?;
        feature
            .set_enabled(enabled)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))?;
        feature
            .get_enabled()
            .await
            .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))
    })
    .await
}

/// Arm the Action Ring panel with the Options+ recipe — `analyticsKeyEvents`
/// reporting on [`PANEL_ANALYTICS_CIDS`], no diversion — then listen for
/// `seconds` and invoke `on_event(cid, event_code)` for each analytics entry
/// (`event_code`: non-zero = press, `0` = release). Restores reporting on exit.
///
/// This is the reverse-engineered path that makes the panel emit at all; the
/// production ring in `gesture.rs` should adopt the same config + decode.
pub async fn watch_panel(
    route: &DeviceRoute,
    seconds: u64,
    on_event: impl Fn(u16, u8) + Send + Sync + 'static,
) -> Result<(), HiddenDiagError> {
    let chan = open_route_channel(route)
        .await
        .map_err(|e| HiddenDiagError::Hid(format!("{e:?}")))?
        .ok_or(HiddenDiagError::DeviceNotFound)?;
    let device_index = route.device_index();
    let mut device = Device::new(Arc::clone(&chan), device_index)
        .await
        .map_err(|_| HiddenDiagError::DeviceUnreachable {
            index: device_index,
        })?;
    let info = device
        .root()
        .get_feature(REPROG_FEATURE_ID)
        .await
        .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))?
        .ok_or_else(|| HiddenDiagError::Hidpp("device does not expose 0x1b04".into()))?;
    let feature_index = info.index;
    eprintln!("[watch_panel] 0x1b04 feature index = {feature_index}");
    let rc = ReprogControlsV4::new(Arc::clone(&chan), device_index, feature_index);

    // The physical force pad is dormant until its threshold is written. Options+
    // sends 0x19c0 (ForceSensingButton) fn3 with `00 15 a3` at init; without it
    // the pad generates no 0x0050 control events for analytics to report.
    let fsb = open_feature::<ForceSensingButtonFeature>(&mut device)
        .await
        .ok();
    eprintln!(
        "[watch_panel] force-sensing feature present = {}",
        fsb.is_some()
    );

    // Listen before arming so nothing is missed once the config lands.
    let on_event = Arc::new(on_event);
    let listener = chan.add_msg_listener_guarded({
        let on_event = Arc::clone(&on_event);
        move |raw, matched| {
            if matched {
                return;
            }
            let msg = v20::Message::from(raw);
            if let Some(ReprogControlsEvent::AnalyticsKeyEvents(entries)) =
                decode_reprog_event(&msg, device_index, feature_index)
            {
                for entry in entries {
                    let cid: u16 = entry.cid.into();
                    if cid != 0 {
                        on_event(cid, entry.event);
                    }
                }
            }
        }
    });

    // Re-arm on a short cadence: a single arm at t=0 is lost if the BTLE link
    // is asleep, and the config does not survive the device sleeping mid-run.
    // Re-applying every few seconds guarantees it lands once the link is hot.
    let arm_on = CidReportingChange {
        analytics_key_events: Some(true),
        ..CidReportingChange::default()
    };
    let ticks = seconds.div_ceil(3).max(1);
    for tick in 0..ticks {
        // Arm the force pad (threshold 0x15a3, button 0), then enable analytics
        // reporting on its control IDs — the full Options+ activation order.
        if let Some(fsb) = &fsb {
            match fsb.raw_call(3, [0x00, 0x15, 0xa3]).await {
                Ok(r) if tick == 0 => {
                    eprintln!("[watch_panel] force threshold set -> {:02x?}", &r[..4]);
                }
                Err(e) if tick == 0 => eprintln!("[watch_panel] force threshold set failed: {e:?}"),
                _ => {}
            }
        }
        for cid in PANEL_ANALYTICS_CIDS {
            match rc.set_cid_reporting_full(cid, arm_on).await {
                Ok(_) if tick == 0 => eprintln!("[watch_panel] analytics armed on 0x{cid:04x}"),
                Err(e) if tick == 0 => eprintln!("[watch_panel] arm 0x{cid:04x} failed: {e:?}"),
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    drop(listener);
    for cid in PANEL_ANALYTICS_CIDS {
        let _ = rc
            .set_cid_reporting_full(
                cid,
                CidReportingChange {
                    analytics_key_events: Some(false),
                    ..CidReportingChange::default()
                },
            )
            .await;
    }
    Ok(())
}

/// Sends one raw short-form call to ANY HID++ 2.0 feature by ID. Returns
/// `Ok(None)` when the device does not expose the feature.
/// Reverse-engineering aid; interpretation is the caller's.
pub async fn raw_feature_call(
    route: &DeviceRoute,
    feature_id: u16,
    function: u8,
    args: [u8; 3],
) -> Result<Option<[u8; 16]>, HiddenDiagError> {
    bounded(async {
        let device = open_device(route).await?;
        device
            .raw_feature_call(feature_id, function, args)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))
    })
    .await
}

/// Sends one raw `0x19c0 ForceSensingButton` call and returns the 16-byte
/// response payload. Reverse-engineering aid; interpretation is the caller's.
pub async fn force_button_raw_call(
    route: &DeviceRoute,
    function: u8,
    args: [u8; 3],
) -> Result<[u8; 16], HiddenDiagError> {
    bounded(async {
        let mut device = open_device(route).await?;
        let feature = open_feature::<ForceSensingButtonFeature>(&mut device)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(e.to_string()))?;
        feature
            .raw_call(function, args)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))
    })
    .await
}
