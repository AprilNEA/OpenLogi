//! Configuration-aware battery duration & health estimator with historical
//! discharge learning (`~/.config/openlogi/battery-telemetry.json`).
//!
//! Computes an estimated remaining battery autonomy and battery health for
//! each paired device based on:
//! 1. Current battery percentage (`BatteryInfo`) and online/charging state.
//! 2. Active hardware configuration (`BacklightState` on keyboards such as
//!    MX Keys Mini, RGB `Lighting` on gaming mice/keyboards, `ReportRate` on
//!    high-polling mice).
//! 3. Historical discharge rate learned per `(config_key, config_signature)`
//!    across real-world discharge cycles, persisted atomically to a sidecar
//!    JSON file in `openlogi_core::paths::config_dir()` so `config.toml`'s
//!    strict schema is never altered and render paths perform zero disk I/O.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus, DeviceKind};
use openlogi_core::hid::{BacklightMode, BacklightState, BacklightStatus};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::state::{AppState, BacklightLoad, DeviceRecord, ReportRateLoad};

const TELEMETRY_FILE: &str = "battery-telemetry.json";
/// Minimum elapsed discharging window (30 minutes) and minimum drop (1%) before
/// updating the learned discharge rate EMA.
const MIN_LEARN_WINDOW_SECS: u64 = 1_800;
/// Maximum elapsed discharging window (14 days) before an anchor is considered
/// expired and re-anchored to the current reading.
const MAX_LEARN_WINDOW_SECS: u64 = 14 * 86_400;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TelemetryStore {
    #[serde(default)]
    devices: BTreeMap<String, DeviceBatteryHistory>,
}

/// Explicit state machine tracking the discharge observation anchor for a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum DischargePhase {
    /// No valid exact-percentage discharging anchor is active (e.g., coarse
    /// level-only device or newly created entry before first online reading).
    #[default]
    Unanchored,
    /// Device was observed charging; any prior discharging anchor is invalidated
    /// until the device transitions back to discharging.
    Charging,
    /// Device is actively discharging from an exact percentage anchor under `signature`.
    Discharging {
        percentage: u8,
        timestamp_secs: u64,
        signature: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceBatteryHistory {
    /// Current discharge tracking phase and anchor.
    #[serde(default)]
    phase: DischargePhase,
    /// Learned drain rate (`% per day`) per configuration signature.
    #[serde(default)]
    learned_drain_per_day: BTreeMap<String, LearnedDrain>,
    /// Estimated battery health (`50..=100`), refined from full/partial discharge cycles.
    #[serde(default = "default_health")]
    estimated_health_pct: u8,
    /// Total accumulated discharge percentage points observed across valid intervals.
    #[serde(default)]
    total_observed_drop_pct: u32,
}

impl Default for DeviceBatteryHistory {
    fn default() -> Self {
        Self {
            phase: DischargePhase::Unanchored,
            learned_drain_per_day: BTreeMap::new(),
            estimated_health_pct: default_health(),
            total_observed_drop_pct: 0,
        }
    }
}

const fn default_health() -> u8 {
    100
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct LearnedDrain {
    /// Exponential moving average of `% per day` under this configuration signature.
    pub pct_per_day: f32,
    /// Number of valid discharge intervals folded into this EMA.
    pub samples: u32,
}

/// Owned battery telemetry state stored inside [`AppState`].
///
/// All disk reads happen once at `AppState::new` (when persistence is enabled)
/// and atomic writes happen during state transitions (`observe_battery_telemetry`),
/// keeping `Render::render` 100% pure and free of disk I/O.
#[derive(Debug, Clone, Default)]
pub(crate) struct BatteryTelemetry {
    store: TelemetryStore,
    path: Option<PathBuf>,
    dirty: bool,
}

impl BatteryTelemetry {
    /// Load persisted telemetry when `persist` is `true`, or create an isolated
    /// in-memory store for tests / read-only sessions.
    #[must_use]
    pub fn load(persist: bool) -> Self {
        if !persist {
            return Self::default();
        }
        let path = telemetry_path();
        let store = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|raw| serde_json::from_str::<TelemetryStore>(&raw).ok())
            .unwrap_or_default();
        Self {
            store,
            path,
            dirty: false,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_path(path: PathBuf) -> Self {
        let store = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<TelemetryStore>(&raw).ok())
            .unwrap_or_default();
        Self {
            store,
            path: Some(path),
            dirty: false,
        }
    }

    /// Pure read of `(health_pct, learned_drain_for_signature, profile_samples)`
    /// for `config_key` and `signature`. Performs zero mutations and zero I/O.
    #[must_use]
    pub fn query(&self, config_key: &str, signature: &str) -> (u8, Option<LearnedDrain>, u32) {
        let Some(entry) = self.store.devices.get(config_key) else {
            return (100, None, 0);
        };
        let health_pct = entry.estimated_health_pct.clamp(50, 100);
        let learned = entry.learned_drain_per_day.get(signature).copied();
        let learned_samples = learned.map_or(0, |d| d.samples);
        (health_pct, learned, learned_samples)
    }

    /// Observe a single device's current battery and configuration state at `now_secs`.
    ///
    /// When `signature` is `None`, the device's power-affecting hardware settings
    /// (such as `BacklightLoad` or `ReportRateLoad`) have not finished loading yet.
    /// Charging transitions still immediately invalidate any prior discharging anchor,
    /// while discharging readings preserve the existing anchor until settings arrive.
    pub fn observe_device(
        &mut self,
        config_key: &str,
        online: bool,
        battery: Option<&BatteryInfo>,
        signature: Option<&str>,
        nominal_full_hours: u32,
        now_secs: u64,
    ) {
        if !online {
            return;
        }
        let Some(battery) = battery else {
            return;
        };

        let is_charging = matches!(
            battery.status,
            BatteryStatus::Charging | BatteryStatus::ChargingSlow
        );

        let entry = self
            .store
            .devices
            .entry(config_key.to_owned())
            .or_insert_with(|| {
                self.dirty = true;
                DeviceBatteryHistory::default()
            });

        // 1. Charging transitions always invalidate any prior discharging anchor immediately.
        if is_charging {
            if entry.phase != DischargePhase::Charging {
                entry.phase = DischargePhase::Charging;
                self.dirty = true;
            }
            return;
        }

        if matches!(battery.status, BatteryStatus::Error) {
            return;
        }

        // 2. Coarse level-only devices (no exact 0..=100 percentage) never feed
        // guessed percentages into discharge learning or health calculation.
        let Some(raw_pct) = battery.percentage else {
            if entry.phase != DischargePhase::Unanchored {
                entry.phase = DischargePhase::Unanchored;
                self.dirty = true;
            }
            return;
        };
        let percentage = raw_pct.min(100);
        if percentage == 0 {
            return;
        }

        // 3. Wait for known hardware settings before anchoring or comparing discharge profiles.
        let Some(signature) = signature else {
            return;
        };

        if advance_discharging_entry(entry, percentage, signature, nominal_full_hours, now_secs) {
            self.dirty = true;
        }
    }

    /// Persist pending telemetry updates atomically via a sibling `.tmp` file,
    /// `sync_all`, and `rename`. Retains `dirty = true` on failure so the next
    /// observation retries.
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = self.path.as_deref() else {
            return;
        };
        match write_telemetry_atomic(path, &self.store) {
            Ok(()) => {
                self.dirty = false;
            }
            Err(error) => {
                warn!(
                    error = %error,
                    path = %path.display(),
                    "could not persist battery telemetry"
                );
            }
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    #[cfg(test)]
    #[must_use]
    pub fn phase_for(&self, config_key: &str) -> Option<&DischargePhase> {
        self.store.devices.get(config_key).map(|d| &d.phase)
    }
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounded battery percentages, hours, and EMA weights fit well within f32/u8/u32"
)]
fn advance_discharging_entry(
    entry: &mut DeviceBatteryHistory,
    percentage: u8,
    signature: &str,
    nominal_full_hours: u32,
    now_secs: u64,
) -> bool {
    match &entry.phase {
        DischargePhase::Unanchored | DischargePhase::Charging => {
            entry.phase = DischargePhase::Discharging {
                percentage,
                timestamp_secs: now_secs,
                signature: signature.to_owned(),
            };
            true
        }
        DischargePhase::Discharging {
            percentage: last_pct,
            timestamp_secs: last_ts,
            signature: last_sig,
        } => {
            let last_pct = *last_pct;
            let last_ts = *last_ts;
            if last_sig != signature || percentage > last_pct {
                entry.phase = DischargePhase::Discharging {
                    percentage,
                    timestamp_secs: now_secs,
                    signature: signature.to_owned(),
                };
                return true;
            }
            if now_secs <= last_ts {
                return false;
            }
            let dt_secs = now_secs.saturating_sub(last_ts);
            if dt_secs > MAX_LEARN_WINDOW_SECS {
                entry.phase = DischargePhase::Discharging {
                    percentage,
                    timestamp_secs: now_secs,
                    signature: signature.to_owned(),
                };
                return true;
            }
            if percentage < last_pct && dt_secs >= MIN_LEARN_WINDOW_SECS {
                let drop_pct = last_pct - percentage;
                let dt_days = (dt_secs as f32) / 86_400.0;
                let observed_pct_per_day = (f32::from(drop_pct) / dt_days).clamp(0.15, 120.0);

                let drain = entry
                    .learned_drain_per_day
                    .entry(signature.to_owned())
                    .or_insert(LearnedDrain {
                        pct_per_day: observed_pct_per_day,
                        samples: 0,
                    });
                if drain.samples == 0 {
                    drain.pct_per_day = observed_pct_per_day;
                } else {
                    drain.pct_per_day = drain.pct_per_day * 0.70 + observed_pct_per_day * 0.30;
                }
                drain.samples = drain.samples.saturating_add(1);

                entry.total_observed_drop_pct = entry
                    .total_observed_drop_pct
                    .saturating_add(u32::from(drop_pct));

                if entry.total_observed_drop_pct >= 10 {
                    let nominal_pct_per_day = (100.0 * 24.0) / (nominal_full_hours.max(1) as f32);
                    let ratio = (nominal_pct_per_day / drain.pct_per_day).clamp(0.50, 1.05);
                    let implied_health = (ratio * 100.0).round().clamp(50.0, 100.0) as u8;
                    entry.estimated_health_pct = ((u16::from(entry.estimated_health_pct) * 4
                        + u16::from(implied_health))
                        / 5) as u8;
                }

                entry.phase = DischargePhase::Discharging {
                    percentage,
                    timestamp_secs: now_secs,
                    signature: signature.to_owned(),
                };
                return true;
            }
            false
        }
    }
}

fn write_telemetry_atomic(path: &Path, store: &TelemetryStore) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(store).map_err(std::io::Error::other)?;
    let tmp_path = path.with_extension("json.tmp");
    {
        let mut file = File::create(&tmp_path)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, path)
}

/// Complete battery duration and health estimate for a single device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatteryEstimate {
    /// Estimated remaining hours of autonomy (`>= 1`).
    pub remaining_hours: u32,
    /// Estimated full-charge (100%) hours under the active configuration.
    pub full_charge_hours: u32,
    /// Estimated battery health (`50..=100%`).
    pub health_pct: u8,
    /// Number of historical discharge intervals learned for the active configuration signature.
    pub learned_samples: u32,
    /// Short localized summary of the active power profile (e.g. `"Brightness 4/7 · 15s"`).
    pub config_note: Option<String>,
}

impl BatteryEstimate {
    /// Compact label for the Home screen list row & grid card.
    #[must_use]
    pub fn home_label(&self) -> String {
        let duration = format_duration_short(self.remaining_hours);
        match &self.config_note {
            Some(note) => tr!(
                "device.battery_estimate_home_with_note",
                duration => duration,
                note => note.clone(),
                health => self.health_pct
            )
            .into(),
            None => tr!(
                "device.battery_estimate_home",
                duration => duration,
                health => self.health_pct
            )
            .into(),
        }
    }

    /// Ultra-compact pill for the detail header bar or grid corner.
    #[must_use]
    pub fn inline_label(&self) -> String {
        let duration = format_duration_compact(self.remaining_hours);
        tr!("device.battery_estimate_inline", duration => duration).into()
    }

    /// Detailed multi-part summary for the Device Details card.
    #[must_use]
    pub fn detail_headline(&self) -> String {
        let duration = format_duration_short(self.remaining_hours);
        match &self.config_note {
            Some(note) => tr!(
                "device.battery_estimate_headline_with_note",
                duration => duration,
                note => note.clone()
            )
            .into(),
            None => tr!("device.battery_estimate_headline", duration => duration).into(),
        }
    }

    /// Secondary caption showing full-charge expectation, health, and learning state.
    #[must_use]
    pub fn detail_subline(&self) -> String {
        let full = format_duration_short(self.full_charge_hours);
        let source: String = if self.learned_samples > 0 {
            tr!(
                "device.battery_estimate_source_learned",
                samples => self.learned_samples
            )
            .into()
        } else {
            tr!("device.battery_estimate_source_config").into()
        };
        tr!(
            "device.battery_estimate_subline",
            health => self.health_pct,
            full => full,
            source => source
        )
        .into()
    }
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounded battery hours fit within f32 and round to positive u32 months"
)]
fn format_duration_compact(hours: u32) -> String {
    if hours < 24 {
        tr!("device.battery_duration_hours", hours => hours).into()
    } else if hours < 24 * 60 {
        let days = (hours + 12) / 24;
        tr!("device.battery_duration_days", days => days).into()
    } else {
        let months = ((hours as f32) / (24.0 * 30.0)).round().max(1.0) as u32;
        tr!("device.battery_duration_months_compact", months => months).into()
    }
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounded battery hours fit within f32 and round to positive u32 months"
)]
fn format_duration_short(hours: u32) -> String {
    if hours < 24 {
        tr!("device.battery_duration_hours", hours => hours).into()
    } else if hours < 24 * 45 {
        let days = (hours + 12) / 24;
        tr!("device.battery_duration_days", days => days).into()
    } else {
        let months = (hours as f32 / (24.0 * 30.0) * 10.0).round() / 10.0;
        let whole = (months.fract() - 0.0).abs() < 0.05;
        let months_str = if whole {
            format!("{}", months.round() as u32)
        } else {
            format!("{months:.1}")
        };
        tr!("device.battery_duration_months_short", months => months_str).into()
    }
}

/// Observe all current devices in `AppState` and flush any telemetry updates.
pub(crate) fn observe_state_telemetry(state: &mut AppState) {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    observe_state_telemetry_at(state, now_secs);
}

struct DeviceObservation {
    config_key: String,
    online: bool,
    battery: Option<BatteryInfo>,
    signature: Option<String>,
    nominal_full_hours: u32,
}

pub(crate) fn observe_state_telemetry_at(state: &mut AppState, now_secs: u64) {
    let observations: Vec<DeviceObservation> = state
        .devices()
        .iter()
        .filter(|record| record.persistent)
        .map(|record| {
            let (nominal_full_hours, signature, _) = nominal_profile_for_device(state, record);
            let known_sig = (!device_has_pending_power_read(state, record)).then_some(signature);
            DeviceObservation {
                config_key: record.config_key.clone(),
                online: record.is_online(),
                battery: record.battery.clone(),
                signature: known_sig,
                nominal_full_hours,
            }
        })
        .collect();

    let telemetry = state.battery_telemetry_mut();
    for obs in observations {
        telemetry.observe_device(
            &obs.config_key,
            obs.online,
            obs.battery.as_ref(),
            obs.signature.as_deref(),
            obs.nominal_full_hours,
            now_secs,
        );
    }
    telemetry.flush();
}

/// Returns `true` only when `record` is online, advertises a power-affecting
/// capability (`backlight` or `report_rate`), and that read is still pending
/// (`Unknown` or `Loading`). Devices that do not support the capability or
/// whose read already resolved (`Ready`, `Failed`, or `Unsupported`) are not
/// blocked from learning.
fn device_has_pending_power_read(state: &AppState, record: &DeviceRecord) -> bool {
    if !record.is_online() {
        return false;
    }
    let Some(caps) = record.capabilities else {
        return false;
    };
    let key = record.device_key();
    let backlight_pending = caps.backlight
        && matches!(
            state.backlight_load_for(&key),
            None | Some(BacklightLoad::Unknown | BacklightLoad::Loading)
        );
    let report_rate_pending = caps.report_rate
        && matches!(
            state.report_rate_load_for(&key),
            None | Some(ReportRateLoad::Unknown | ReportRateLoad::Loading)
        );
    backlight_pending || report_rate_pending
}

/// Compute the battery duration & health estimate for `record` given current `AppState`.
/// Pure in-memory calculation safe to call inside `Render::render`.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounded battery percentages, hours, and EMA sample counts fit within f32/u32"
)]
pub(crate) fn estimate_for_device(
    state: &AppState,
    record: &DeviceRecord,
) -> Option<BatteryEstimate> {
    let battery = record.battery.as_ref()?;
    if matches!(
        battery.status,
        BatteryStatus::Charging | BatteryStatus::ChargingSlow | BatteryStatus::Error
    ) {
        return None;
    }
    let percentage = effective_percentage(battery)?;
    if percentage == 0 {
        return None;
    }

    let (nominal_full_hours, signature, config_note) = nominal_profile_for_device(state, record);

    let (health_pct, learned_drain, learned_samples) = state
        .battery_telemetry()
        .query(&record.config_key, &signature);

    // Nominal % per day at 100% health:
    let nominal_pct_per_day = (100.0 * 24.0) / (nominal_full_hours.max(1) as f32);
    // Adjust nominal drain by battery health (lower health -> faster drain):
    let health_factor = f32::from(health_pct.clamp(50, 100)) / 100.0;
    let model_pct_per_day = nominal_pct_per_day / health_factor;

    // Blend physics/config model with historical learned drain for this exact signature:
    let effective_pct_per_day = match learned_drain {
        Some(learned) if learned.samples > 0 => {
            let weight = ((learned.samples as f32) * 0.20).clamp(0.20, 0.75);
            model_pct_per_day * (1.0 - weight) + learned.pct_per_day * weight
        }
        _ => model_pct_per_day,
    }
    .max(0.15);

    let full_charge_days = 100.0 / effective_pct_per_day;
    let remaining_days = f32::from(percentage) / effective_pct_per_day;

    let full_charge_hours = (full_charge_days * 24.0).round().max(1.0) as u32;
    let remaining_hours = (remaining_days * 24.0).round().max(1.0) as u32;

    Some(BatteryEstimate {
        remaining_hours,
        full_charge_hours,
        health_pct,
        learned_samples,
        config_note,
    })
}

fn effective_percentage(battery: &BatteryInfo) -> Option<u8> {
    if let Some(pct) = battery.percentage {
        return Some(pct.min(100));
    }
    match battery.level {
        BatteryLevel::Full => Some(100),
        BatteryLevel::Good => Some(70),
        BatteryLevel::Low => Some(20),
        BatteryLevel::Critical => Some(5),
        BatteryLevel::Unknown => None,
    }
}

/// Evaluated RGB power state for a device, distinguishing unset onboard lighting
/// from explicitly dark or lit saved settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RgbPowerState {
    /// Device does not advertise `Capabilities::lighting`.
    Unsupported,
    /// Device supports `Capabilities::lighting`, but no saved `Lighting` preference
    /// exists yet — hardware continues running its onboard lighting state.
    Unknown,
    /// Saved `Lighting` is disabled, has `brightness == 0`, or emits black (`0, 0, 0`).
    Off,
    /// Saved `Lighting` is enabled with non-zero brightness and a non-black color.
    On,
}

impl RgbPowerState {
    const fn token(self) -> &'static str {
        match self {
            Self::Unsupported | Self::Off => "off",
            Self::Unknown => "unknown",
            Self::On => "on",
        }
    }
}

fn lighting_emits_light(lighting: &openlogi_core::config::Lighting) -> bool {
    let (r, g, b) = lighting.color.components();
    lighting.enabled && lighting.brightness > 0 && (r != 0 || g != 0 || b != 0)
}

fn rgb_power_state_for_device(state: &AppState, record: &DeviceRecord) -> RgbPowerState {
    let has_lighting_cap = record.capabilities.is_some_and(|c| c.lighting);
    match state.lighting_for(&record.config_key, &record.route_key) {
        Some(lighting) if lighting_emits_light(&lighting) => RgbPowerState::On,
        Some(_) => RgbPowerState::Off,
        None if has_lighting_cap => RgbPowerState::Unknown,
        None => RgbPowerState::Unsupported,
    }
}

/// Returns `(nominal_full_charge_hours, config_signature, localized_config_note)`.
///
/// Strictly gates on `record.capabilities` and `Load::Ready` so unprobed or
/// failed reads never fabricate hardware settings.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped positive base_hours rounds into u32"
)]
pub(crate) fn nominal_profile_for_device(
    state: &AppState,
    record: &DeviceRecord,
) -> (u32, String, Option<String>) {
    let model_lower = record.model_name.to_ascii_lowercase();
    let key = record.device_key();

    match record.kind {
        DeviceKind::Keyboard => {
            let has_backlight = record.capabilities.is_some_and(|c| c.backlight);

            if has_backlight {
                if let Some(BacklightLoad::Ready(bl)) = state.backlight_load_for(&key) {
                    return keyboard_backlight_profile(&model_lower, **bl);
                }
                return (120 * 24, "kbd:unmeasured".to_owned(), None);
            }

            // Check RGB lighting for gaming keyboards
            match rgb_power_state_for_device(state, record) {
                RgbPowerState::On => {
                    let note = tr!("device.battery_note_rgb_on").into();
                    (40, "kbd:rgb-on".to_owned(), Some(note))
                }
                RgbPowerState::Off => {
                    let note = tr!("device.battery_note_rgb_off").into();
                    (180 * 24, "kbd:rgb-off".to_owned(), Some(note))
                }
                RgbPowerState::Unknown => (40, "kbd:rgb-unknown".to_owned(), None),
                RgbPowerState::Unsupported => (120 * 24, "kbd:standard".to_owned(), None),
            }
        }
        DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad => {
            let has_report_rate = record.capabilities.is_some_and(|c| c.report_rate);
            let rate_hz = if has_report_rate {
                match state.report_rate_load_for(&key) {
                    Some(ReportRateLoad::Ready(info)) => Some(u16::from(info.current)),
                    _ => None,
                }
            } else {
                None
            };
            let rgb_state = rgb_power_state_for_device(state, record);

            if model_lower.contains("mx master") {
                return (70 * 24, "mouse:mx-master".to_owned(), None);
            }
            if model_lower.contains("mx anywhere") {
                return (70 * 24, "mouse:mx-anywhere".to_owned(), None);
            }
            if model_lower.contains("mx vertical") || model_lower.contains("lift") {
                return (120 * 24, "mouse:ergo".to_owned(), None);
            }
            if has_report_rate
                || rgb_state != RgbPowerState::Unsupported
                || model_lower.contains("g502")
                || model_lower.contains("pro x")
                || model_lower.contains("g pro")
            {
                let mut base_hours: f32 = match rgb_state {
                    RgbPowerState::On => 45.0,
                    RgbPowerState::Unknown => 90.0,
                    RgbPowerState::Off | RgbPowerState::Unsupported => 130.0,
                };
                if let Some(hz) = rate_hz {
                    if hz >= 2000 {
                        base_hours *= 0.65;
                    } else if hz <= 250 {
                        base_hours *= 1.25;
                    }
                }
                let note = match (rgb_state, rate_hz) {
                    (RgbPowerState::On, Some(hz)) => {
                        Some(tr!("device.battery_note_rgb_rate", hz => hz).into())
                    }
                    (_, Some(hz)) => Some(tr!("device.battery_note_rate", hz => hz).into()),
                    (RgbPowerState::On, None) => Some(tr!("device.battery_note_rgb_on").into()),
                    (RgbPowerState::Off, None) => Some(tr!("device.battery_note_rgb_off").into()),
                    (RgbPowerState::Unknown | RgbPowerState::Unsupported, None) => None,
                };
                let rgb_token = rgb_state.token();
                let hz_token = if has_report_rate {
                    rate_hz.map_or_else(|| "unknown".to_owned(), |hz| hz.to_string())
                } else {
                    "fixed".to_owned()
                };
                let sig = format!("mouse:gaming:rgb={rgb_token}:hz={hz_token}");
                return (base_hours.round().max(10.0) as u32, sig, note);
            }

            (65 * 24, "mouse:generic".to_owned(), None)
        }
        DeviceKind::Headset => (24, "headset:default".to_owned(), None),
        _ => (30 * 24, "other:default".to_owned(), None),
    }
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounded backlight hours and drain factors fit within f32/u32"
)]
fn keyboard_backlight_profile(
    model_lower: &str,
    bl: BacklightState,
) -> (u32, String, Option<String>) {
    let off_hours: u32 = if model_lower.contains("mx keys") {
        150 * 24
    } else {
        120 * 24
    };

    let effectively_off = !bl.enabled
        || matches!(
            bl.status,
            BacklightStatus::DisabledBySoftware | BacklightStatus::DisabledByCriticalBattery
        )
        || (!bl.is_automatic() && bl.current_level == 0);

    if effectively_off {
        let note = tr!("device.battery_note_backlight_off").into();
        return (off_hours, "kbd:bl-off".to_owned(), Some(note));
    }

    let max_level = bl.nb_levels.saturating_sub(1).max(1);
    let level = bl.current_level.min(max_level);
    let timeout_secs = bl
        .duration
        .map_or(15, openlogi_core::hid::BacklightDuration::seconds)
        .max(5);

    let level_ratio = if bl.mode == BacklightMode::Automatic {
        0.50
    } else {
        f32::from(level) / f32::from(max_level)
    };
    let level_drain_factor = 0.30 + 0.95 * level_ratio.powf(1.25);
    let duration_drain_factor = (f32::from(timeout_secs) / 15.0).powf(0.32).clamp(0.68, 3.2);
    let power_save_bonus = if bl.power_save { 0.92 } else { 1.0 };

    let total_drain_factor = level_drain_factor * duration_drain_factor * power_save_bonus;
    let estimated_hours = ((10.0 * 24.0) / total_drain_factor)
        .round()
        .clamp(48.0, (off_hours as f32) * 0.85) as u32;

    let dur_str: String = if timeout_secs < 60 {
        tr!("device.backlight_duration_seconds", seconds => timeout_secs).into()
    } else if timeout_secs < 3600 {
        tr!("device.backlight_duration_minutes", minutes => timeout_secs / 60).into()
    } else {
        tr!("device.backlight_duration_hours", hours => timeout_secs / 3600).into()
    };

    let note = if bl.mode == BacklightMode::Automatic {
        tr!("device.battery_note_backlight_auto", duration => dur_str).into()
    } else {
        tr!(
            "device.battery_note_backlight_level",
            level => level,
            max => max_level,
            duration => dur_str
        )
        .into()
    };

    let sig = if bl.mode == BacklightMode::Automatic {
        format!(
            "kbd:bl-on:mode=Auto:dur={timeout_secs}:ps={}",
            bl.power_save
        )
    } else {
        format!(
            "kbd:bl-on:mode={:?}:lvl={level}/{max_level}:dur={timeout_secs}:ps={}",
            bl.mode, bl.power_save
        )
    };
    (estimated_hours, sig, Some(note))
}

fn telemetry_path() -> Option<PathBuf> {
    openlogi_core::paths::config_dir()
        .ok()
        .map(|dir| dir.join(TELEMETRY_FILE))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use openlogi_core::config::Config;
    use openlogi_core::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceInventory, DeviceKind,
        DeviceModelInfo, DeviceTransports, PairedDevice, ReceiverInfo,
    };
    use openlogi_core::hid::{
        BacklightControls, BacklightDuration, BacklightMode, BacklightState, BacklightStatus,
        ReportRate, ReportRateInfo,
    };

    use super::{
        BatteryTelemetry, DischargePhase, MAX_LEARN_WINDOW_SECS, MIN_LEARN_WINDOW_SECS,
        estimate_for_device, nominal_profile_for_device, observe_state_telemetry_at,
    };
    use crate::services::assets::AssetResolver;
    use crate::services::i18n::LOCALE_LOCK;
    use crate::state::{AppState, Sources};

    fn test_state_with_devices(paired: Vec<PairedDevice>) -> AppState {
        let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
        let resolver = AssetResolver::new();
        let inventory = DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".into(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("bolt-1".into()),
            },
            paired,
        };
        AppState::new(Sources {
            inventories: &[inventory],
            ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
        })
    }

    fn mx_keys_mini_device(
        capabilities: Option<Capabilities>,
        battery: Option<BatteryInfo>,
    ) -> PairedDevice {
        PairedDevice {
            slot: 1,
            wpid: Some(0xb369),
            kind: DeviceKind::Keyboard,
            codename: Some("MX Keys Mini".into()),
            online: true,
            battery,
            capabilities,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: Some("2444CEB3PWH8".into()),
                unit_id: [0x24, 0x44, 0xce, 0xb3],
                transports: DeviceTransports::default(),
                model_ids: [0xb369, 0, 0],
                extended_model_id: 0,
            }),
            settings_revision: 0,
            hidpp10: false,
        }
    }

    fn gaming_mouse_device(
        capabilities: Option<Capabilities>,
        battery: Option<BatteryInfo>,
    ) -> PairedDevice {
        PairedDevice {
            slot: 2,
            wpid: Some(0x407f),
            kind: DeviceKind::Mouse,
            codename: Some("G502 X".into()),
            online: true,
            battery,
            capabilities,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: Some("G502SERIAL01".into()),
                unit_id: [0x11, 0x22, 0x33, 0x44],
                transports: DeviceTransports::default(),
                model_ids: [0x407f, 0, 0],
                extended_model_id: 0,
            }),
            settings_revision: 0,
            hidpp10: false,
        }
    }

    fn manual_backlight(level: u8, duration_secs: u16) -> BacklightState {
        BacklightState {
            enabled: true,
            power_save: false,
            mode: BacklightMode::PermanentManual,
            controls: BacklightControls::AUTOMATIC
                | BacklightControls::LEVEL
                | BacklightControls::POWER_SAVE
                | BacklightControls::DURATION,
            status: BacklightStatus::PermanentManual,
            current_level: level,
            nb_levels: 8,
            duration: Some(BacklightDuration::try_from(duration_secs).unwrap()),
        }
    }

    #[test]
    fn locale_switch_translates_all_estimate_labels_and_notes() {
        let _lock = LOCALE_LOCK.lock().unwrap();
        let caps = Capabilities {
            backlight: true,
            ..Default::default()
        };
        let battery = BatteryInfo {
            percentage: Some(80),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        let mut state =
            test_state_with_devices(vec![mx_keys_mini_device(Some(caps), Some(battery))]);
        let record = state.devices()[0].clone();
        state.device_reads_mut().set_backlight_load_for_test(
            record.device_key(),
            crate::state::BacklightLoad::Ready(Arc::new(manual_backlight(4, 15))),
        );

        rust_i18n::set_locale("en");
        let en_est = estimate_for_device(&state, &record).expect("estimate in en");
        assert!(
            en_est
                .config_note
                .as_deref()
                .unwrap()
                .contains("Brightness 4/7"),
            "unexpected en note: {:?}",
            en_est.config_note
        );
        assert!(en_est.home_label().contains("Health 100%"));
        assert!(en_est.detail_headline().contains("remaining"));
        assert!(en_est.detail_subline().contains("Based on active config"));

        rust_i18n::set_locale("pt-BR");
        let pt_est = estimate_for_device(&state, &record).expect("estimate in pt-BR");
        assert!(
            pt_est
                .config_note
                .as_deref()
                .unwrap()
                .contains("Brilho 4/7"),
            "unexpected pt-BR note: {:?}",
            pt_est.config_note
        );
        assert!(pt_est.home_label().contains("Saúde 100%"));
        assert!(pt_est.detail_headline().contains("restantes"));
        assert!(pt_est.detail_subline().contains("Baseado na config atual"));
        rust_i18n::set_locale("en");
    }

    #[test]
    fn absent_or_unready_backlight_never_fabricates_level_4_fallback() {
        let _lock = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let battery = BatteryInfo {
            percentage: Some(90),
            level: BatteryLevel::Full,
            status: BatteryStatus::Discharging,
        };

        // Case 1: capabilities == None on MX Keys Mini
        let state_no_caps =
            test_state_with_devices(vec![mx_keys_mini_device(None, Some(battery.clone()))]);
        let rec_no_caps = &state_no_caps.devices()[0];
        let (hours, sig, note) = nominal_profile_for_device(&state_no_caps, rec_no_caps);
        assert_eq!(hours, 120 * 24);
        assert_eq!(sig, "kbd:standard");
        assert_eq!(note, None);

        // Case 2: capabilities.backlight == true, but BacklightLoad is Unknown / not Ready
        let caps = Capabilities {
            backlight: true,
            ..Default::default()
        };
        let state_unready =
            test_state_with_devices(vec![mx_keys_mini_device(Some(caps), Some(battery))]);
        let rec_unready = &state_unready.devices()[0];
        let (hours2, sig2, note2) = nominal_profile_for_device(&state_unready, rec_unready);
        assert_eq!(hours2, 120 * 24);
        assert_eq!(sig2, "kbd:unmeasured");
        assert_eq!(note2, None);
    }

    #[test]
    fn charging_interruption_invalidates_anchor_and_learns_only_post_charge_drop() {
        let mut telemetry = BatteryTelemetry::load(false);
        let key = "serial:2444ceb3pwh8";
        let sig = "kbd:bl-off";
        let nominal_hours = 150 * 24;
        let t0 = 1_700_000_000;

        let discharging = |pct: u8| BatteryInfo {
            percentage: Some(pct),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        let charging = |pct: u8| BatteryInfo {
            percentage: Some(pct),
            level: BatteryLevel::Good,
            status: BatteryStatus::Charging,
        };

        // 1. Anchor at 80% discharging
        telemetry.observe_device(
            key,
            true,
            Some(&discharging(80)),
            Some(sig),
            nominal_hours,
            t0,
        );
        assert_eq!(
            telemetry.phase_for(key),
            Some(&DischargePhase::Discharging {
                percentage: 80,
                timestamp_secs: t0,
                signature: sig.to_owned(),
            })
        );

        // 2. Partial charge observed at 75% -> must transition phase to Charging
        let t1 = t0 + 3_600;
        telemetry.observe_device(key, true, Some(&charging(75)), Some(sig), nominal_hours, t1);
        assert_eq!(telemetry.phase_for(key), Some(&DischargePhase::Charging));

        // 3. Unplugged at 75% discharging -> re-anchors at 75% without learning 80% -> 75%
        let t2 = t1 + 600;
        telemetry.observe_device(
            key,
            true,
            Some(&discharging(75)),
            Some(sig),
            nominal_hours,
            t2,
        );
        assert_eq!(
            telemetry.query(key, sig).2,
            0,
            "no sample learned on unplug"
        );
        assert_eq!(
            telemetry.phase_for(key),
            Some(&DischargePhase::Discharging {
                percentage: 75,
                timestamp_secs: t2,
                signature: sig.to_owned(),
            })
        );

        // 4. Subsequent 5% drop (75% -> 70%) over 2 days learns strictly from 5% / 2 days (2.5%/day)
        let t3 = t2 + 2 * 86_400;
        telemetry.observe_device(
            key,
            true,
            Some(&discharging(70)),
            Some(sig),
            nominal_hours,
            t3,
        );
        let (_, learned, samples) = telemetry.query(key, sig);
        assert_eq!(samples, 1);
        let drain = learned.expect("learned drain present");
        assert!(
            (drain.pct_per_day - 2.5).abs() < 0.01,
            "expected 2.5%/day from 75->70% over 2d, got {}",
            drain.pct_per_day
        );
    }

    #[test]
    fn expired_14_day_window_reanchors_so_subsequent_drop_learns() {
        let mut telemetry = BatteryTelemetry::load(false);
        let key = "serial:mouse1";
        let sig = "mouse:mx-master";
        let nominal_hours = 70 * 24;
        let t0 = 1_700_000_000;

        let discharging = |pct: u8| BatteryInfo {
            percentage: Some(pct),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };

        telemetry.observe_device(
            key,
            true,
            Some(&discharging(90)),
            Some(sig),
            nominal_hours,
            t0,
        );

        // 15 days later (> MAX_LEARN_WINDOW_SECS) at 60%: must re-anchor without learning
        let t_gap = t0 + MAX_LEARN_WINDOW_SECS + 86_400;
        telemetry.observe_device(
            key,
            true,
            Some(&discharging(60)),
            Some(sig),
            nominal_hours,
            t_gap,
        );
        assert_eq!(telemetry.query(key, sig).2, 0);
        assert_eq!(
            telemetry.phase_for(key),
            Some(&DischargePhase::Discharging {
                percentage: 60,
                timestamp_secs: t_gap,
                signature: sig.to_owned(),
            })
        );

        // 2 hours later (>= MIN_LEARN_WINDOW_SECS) at 58%: now learns normally from 60% -> 58%
        let t_next = t_gap + MIN_LEARN_WINDOW_SECS * 4;
        telemetry.observe_device(
            key,
            true,
            Some(&discharging(58)),
            Some(sig),
            nominal_hours,
            t_next,
        );
        assert_eq!(telemetry.query(key, sig).2, 1);
    }

    #[test]
    fn coarse_battery_level_returns_nominal_estimate_without_learning_or_health_mutation() {
        let _lock = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let coarse_good = BatteryInfo {
            percentage: None,
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        let coarse_low = BatteryInfo {
            percentage: None,
            level: BatteryLevel::Low,
            status: BatteryStatus::Discharging,
        };

        let mut state =
            test_state_with_devices(vec![mx_keys_mini_device(None, Some(coarse_good.clone()))]);
        let key = state.devices()[0].config_key.clone();
        let t0 = 1_700_000_000;
        observe_state_telemetry_at(&mut state, t0);
        assert_eq!(
            state.battery_telemetry().phase_for(&key),
            Some(&DischargePhase::Unanchored)
        );

        // Simulate drop to Low 2 days later: still Unanchored, 0 learned samples, 100% health
        state.battery_telemetry_mut().observe_device(
            &key,
            true,
            Some(&coarse_low),
            Some("kbd:standard"),
            120 * 24,
            t0 + 2 * 86_400,
        );
        let (health, learned, samples) = state.battery_telemetry().query(&key, "kbd:standard");
        assert_eq!(health, 100);
        assert_eq!(learned, None);
        assert_eq!(samples, 0);

        // Estimate still works from nominal model
        let est = estimate_for_device(&state, &state.devices()[0]).expect("coarse estimate");
        assert!(est.remaining_hours > 0);
        assert_eq!(est.learned_samples, 0);
    }

    #[test]
    fn unselected_mouse_report_rate_and_profile_specific_samples_are_accurate() {
        let _lock = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let battery = BatteryInfo {
            percentage: Some(80),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        let mouse_caps = Capabilities {
            report_rate: true,
            ..Default::default()
        };

        // Keyboard at index 0 (selected), gaming mouse at index 1 (unselected)
        let mut state = test_state_with_devices(vec![
            mx_keys_mini_device(None, Some(battery.clone())),
            gaming_mouse_device(Some(mouse_caps), Some(battery.clone())),
        ]);
        let mouse_record = state.devices()[1].clone();

        // Before report rate is Ready: signature uses hz=unknown (never unwrap_or(1000))
        let (_, sig_unknown, note_unknown) = nominal_profile_for_device(&state, &mouse_record);
        assert_eq!(sig_unknown, "mouse:gaming:rgb=off:hz=unknown");
        assert_eq!(note_unknown, None);

        // Once report rate is Ready at 2000Hz on the unselected mouse:
        state.device_reads_mut().set_report_rate_load_for_test(
            mouse_record.device_key(),
            crate::state::ReportRateLoad::Ready(Arc::new(ReportRateInfo {
                current: ReportRate::from(2000),
                supported: vec![ReportRate::from(1000), ReportRate::from(2000)],
            })),
        );
        let (hours_2k, sig_2k, note_2k) = nominal_profile_for_device(&state, &mouse_record);
        assert_eq!(sig_2k, "mouse:gaming:rgb=off:hz=2000");
        assert_eq!(note_2k.as_deref(), Some("2000Hz"));
        assert!(hours_2k < 130);

        // Learn 1 sample on 2000Hz profile, then switch to 1000Hz profile -> learned_samples must be 0
        let t0 = 1_700_000_000;
        observe_state_telemetry_at(&mut state, t0);
        state.battery_telemetry_mut().observe_device(
            &mouse_record.config_key,
            true,
            Some(&BatteryInfo {
                percentage: Some(70),
                ..battery
            }),
            Some(&sig_2k),
            hours_2k,
            t0 + 3_600 * 4,
        );
        let est_2k = estimate_for_device(&state, &mouse_record).unwrap();
        assert_eq!(est_2k.learned_samples, 1);

        state.device_reads_mut().set_report_rate_load_for_test(
            mouse_record.device_key(),
            crate::state::ReportRateLoad::Ready(Arc::new(ReportRateInfo {
                current: ReportRate::from(1000),
                supported: vec![ReportRate::from(1000), ReportRate::from(2000)],
            })),
        );
        let est_1k = estimate_for_device(&state, &mouse_record).unwrap();
        assert_eq!(
            est_1k.learned_samples, 0,
            "switching to an unlearned profile signature must report 0 samples"
        );
    }

    #[test]
    fn atomic_persistence_retains_dirty_on_failure_and_recovers() {
        let temp_dir =
            std::env::temp_dir().join(format!("openlogi-battery-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Point telemetry path at a directory where `.json.tmp` creation or rename will fail
        // by creating a directory at `battery-telemetry.json.tmp`
        let file_path = temp_dir.join("battery-telemetry.json");
        let blocker_tmp = temp_dir.join("battery-telemetry.json.tmp");
        std::fs::create_dir_all(&blocker_tmp).unwrap();

        let mut telemetry = BatteryTelemetry::with_path(file_path.clone());
        let battery = BatteryInfo {
            percentage: Some(85),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        telemetry.observe_device(
            "serial:test",
            true,
            Some(&battery),
            Some("kbd:standard"),
            2880,
            1000,
        );
        assert!(telemetry.is_dirty());

        // Flush fails because `.json.tmp` is a directory -> dirty remains true
        telemetry.flush();
        assert!(
            telemetry.is_dirty(),
            "dirty flag must remain true when atomic write fails"
        );

        // Remove blocker directory and flush again -> succeeds and clears dirty
        std::fs::remove_dir_all(&blocker_tmp).unwrap();
        telemetry.flush();
        assert!(!telemetry.is_dirty());
        assert!(file_path.exists());

        let reloaded = BatteryTelemetry::with_path(file_path);
        assert_eq!(
            reloaded.phase_for("serial:test"),
            Some(&DischargePhase::Discharging {
                percentage: 85,
                timestamp_secs: 1000,
                signature: "kbd:standard".into(),
            })
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn restart_preserves_discharge_interval_until_async_settings_reply_arrives() {
        let caps = Capabilities {
            backlight: true,
            ..Default::default()
        };
        let battery_80 = BatteryInfo {
            percentage: Some(80),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        let mut state =
            test_state_with_devices(vec![mx_keys_mini_device(Some(caps), Some(battery_80))]);
        let dev_key = state.devices()[0].device_key();
        let cfg_key = state.devices()[0].config_key.clone();
        let t0 = 1_700_000_000;

        // Session 1: Backlight is Ready -> anchors at 80% with kbd:bl-on:...
        state.device_reads_mut().set_backlight_load_for_test(
            dev_key.clone(),
            crate::state::BacklightLoad::Ready(Arc::new(manual_backlight(4, 15))),
        );
        observe_state_telemetry_at(&mut state, t0);
        let (_, expected_sig, _) = nominal_profile_for_device(&state, &state.devices()[0]);
        assert_eq!(
            state.battery_telemetry().phase_for(&cfg_key),
            Some(&DischargePhase::Discharging {
                percentage: 80,
                timestamp_secs: t0,
                signature: expected_sig.clone(),
            })
        );

        // Simulate app restart 2 hours later: battery is now 75%, but BacklightLoad is Unknown
        // when the first inventory update arrives before `load_all_backlights` replies.
        let t_restart = t0 + 2 * 3_600;
        state
            .device_reads_mut()
            .set_backlight_load_for_test(dev_key.clone(), crate::state::BacklightLoad::Unknown);
        let battery_75 = BatteryInfo {
            percentage: Some(75),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        let inv = DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".into(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: Some("bolt-1".into()),
            },
            paired: vec![mx_keys_mini_device(Some(caps), Some(battery_75))],
        };
        let resolver = AssetResolver::new();
        let _ = state.refresh_inventories(&[inv], &[], &resolver, &[]);
        observe_state_telemetry_at(&mut state, t_restart);

        // Anchor must STILL be the 80% @ t0 reading (not overwritten by kbd:unmeasured)!
        assert_eq!(
            state.battery_telemetry().phase_for(&cfg_key),
            Some(&DischargePhase::Discharging {
                percentage: 80,
                timestamp_secs: t0,
                signature: expected_sig.clone(),
            })
        );

        // Now the async backlight read completes at t_restart + 1s:
        // The 80% -> 75% interval is preserved and learned!
        state.device_reads_mut().set_backlight_load_for_test(
            dev_key,
            crate::state::BacklightLoad::Ready(Arc::new(manual_backlight(4, 15))),
        );
        observe_state_telemetry_at(&mut state, t_restart + 1);
        let (_, _, samples) = state.battery_telemetry().query(&cfg_key, &expected_sig);
        assert_eq!(
            samples, 1,
            "restart must preserve the prior anchor until settings reply arrives"
        );
    }

    #[test]
    fn automatic_brightness_sensor_changes_preserve_learning_window() {
        let caps = Capabilities {
            backlight: true,
            ..Default::default()
        };
        let battery_90 = BatteryInfo {
            percentage: Some(90),
            level: BatteryLevel::Full,
            status: BatteryStatus::Discharging,
        };
        let mut state =
            test_state_with_devices(vec![mx_keys_mini_device(Some(caps), Some(battery_90))]);
        let dev_key = state.devices()[0].device_key();
        let cfg_key = state.devices()[0].config_key.clone();
        let t0 = 1_700_000_000;

        let auto_bl = |level: u8| BacklightState {
            enabled: true,
            power_save: false,
            mode: BacklightMode::Automatic,
            controls: BacklightControls::AUTOMATIC | BacklightControls::DURATION,
            status: BacklightStatus::AlsAutomatic,
            current_level: level,
            nb_levels: 8,
            duration: Some(BacklightDuration::try_from(15u16).unwrap()),
        };

        // 1. Anchor at 90% in Automatic mode with ambient sensor level 2
        state.device_reads_mut().set_backlight_load_for_test(
            dev_key.clone(),
            crate::state::BacklightLoad::Ready(Arc::new(auto_bl(2))),
        );
        observe_state_telemetry_at(&mut state, t0);
        let (_, sig_lvl2, _) = nominal_profile_for_device(&state, &state.devices()[0]);

        // 2. 10 minutes later, ambient room light changes sensor current_level to 6 (still 90%)
        state.device_reads_mut().set_backlight_load_for_test(
            dev_key.clone(),
            crate::state::BacklightLoad::Ready(Arc::new(auto_bl(6))),
        );
        let (_, sig_lvl6, _) = nominal_profile_for_device(&state, &state.devices()[0]);
        assert_eq!(
            sig_lvl2, sig_lvl6,
            "automatic mode signature must not depend on transient ambient sensor level"
        );
        observe_state_telemetry_at(&mut state, t0 + 600);
        assert_eq!(
            state.battery_telemetry().phase_for(&cfg_key),
            Some(&DischargePhase::Discharging {
                percentage: 90,
                timestamp_secs: t0,
                signature: sig_lvl2.clone(),
            }),
            "ambient sensor change must not reset the discharge anchor timestamp"
        );

        // 3. At t0 + 40 minutes (>= 30m window), battery drops to 88% with sensor at level 5 -> learns!
        let battery_88 = BatteryInfo {
            percentage: Some(88),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        state.device_reads_mut().set_backlight_load_for_test(
            dev_key,
            crate::state::BacklightLoad::Ready(Arc::new(auto_bl(5))),
        );
        let (hours_lvl5, sig_lvl5, _) = nominal_profile_for_device(&state, &state.devices()[0]);
        state.battery_telemetry_mut().observe_device(
            &cfg_key,
            true,
            Some(&battery_88),
            Some(&sig_lvl5),
            hours_lvl5,
            t0 + 2_400,
        );
        assert_eq!(state.battery_telemetry().query(&cfg_key, &sig_lvl2).2, 1);
    }

    #[test]
    fn unset_rgb_and_zero_brightness_or_black_color_select_correct_profiles() {
        let _lock = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let battery = BatteryInfo {
            percentage: Some(80),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        let rgb_kbd_caps = Capabilities {
            lighting: true,
            ..Default::default()
        };
        let mut g915 = mx_keys_mini_device(Some(rgb_kbd_caps), Some(battery));
        g915.codename = Some("G915 TKL".into());

        let mut state = test_state_with_devices(vec![g915]);
        let record = state.devices()[0].clone();

        // 1. Fresh RGB keyboard with `lighting: None` -> unknown lighting profile (40h, kbd:rgb-unknown), NOT kbd:standard
        let (hours_unset, sig_unset, note_unset) = nominal_profile_for_device(&state, &record);
        assert_eq!(hours_unset, 40);
        assert_eq!(sig_unset, "kbd:rgb-unknown");
        assert_eq!(note_unset, None);

        // 2. Saved lighting with enabled: true, brightness: 80, white -> lit profile (40h, kbd:rgb-on)
        let _ = state.commit_lighting(openlogi_core::config::Lighting {
            enabled: true,
            color: openlogi_core::color::Rgb::WHITE,
            brightness: 80,
        });
        let (hours_on, sig_on, note_on) = nominal_profile_for_device(&state, &record);
        assert_eq!(hours_on, 40);
        assert_eq!(sig_on, "kbd:rgb-on");
        assert_eq!(note_on.as_deref(), Some("RGB on"));

        // 3. Saved lighting with enabled: true, brightness: 0 -> dark profile (180d, kbd:rgb-off)
        let _ = state.commit_lighting(openlogi_core::config::Lighting {
            enabled: true,
            color: openlogi_core::color::Rgb::WHITE,
            brightness: 0,
        });
        let (hours_zero, sig_zero, note_zero) = nominal_profile_for_device(&state, &record);
        assert_eq!(hours_zero, 180 * 24);
        assert_eq!(sig_zero, "kbd:rgb-off");
        assert_eq!(note_zero.as_deref(), Some("RGB off"));

        // 4. Saved lighting with enabled: true, brightness: 100, color: 000000 (black) -> dark profile
        let _ = state.commit_lighting(openlogi_core::config::Lighting {
            enabled: true,
            color: openlogi_core::color::Rgb::new(0, 0, 0),
            brightness: 100,
        });
        let (hours_black, sig_black, note_black) = nominal_profile_for_device(&state, &record);
        assert_eq!(hours_black, 180 * 24);
        assert_eq!(sig_black, "kbd:rgb-off");
        assert_eq!(note_black.as_deref(), Some("RGB off"));
    }

    #[test]
    fn rgb_mouse_without_report_rate_capability_learns_discharge_interval() {
        let battery_80 = BatteryInfo {
            percentage: Some(80),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        // RGB gaming mouse that supports lighting (`lighting: true`) but NOT adjustable report rate (`report_rate: false`)
        let rgb_only_mouse_caps = Capabilities {
            lighting: true,
            report_rate: false,
            ..Default::default()
        };
        let mut state = test_state_with_devices(vec![gaming_mouse_device(
            Some(rgb_only_mouse_caps),
            Some(battery_80),
        )]);
        let cfg_key = state.devices()[0].config_key.clone();

        // 1. Anchor at 80% on initial state creation — even though report_rate is unsupported,
        // learning must NOT be blocked and the device immediately enters Discharging phase!
        let (nominal_hours, sig, _) = nominal_profile_for_device(&state, &state.devices()[0]);
        assert_eq!(sig, "mouse:gaming:rgb=unknown:hz=fixed");
        let Some(DischargePhase::Discharging {
            percentage: 80,
            timestamp_secs: t0,
            signature: anchored_sig,
        }) = state.battery_telemetry().phase_for(&cfg_key).cloned()
        else {
            panic!(
                "expected Discharging anchor at 80%, got {:?}",
                state.battery_telemetry().phase_for(&cfg_key)
            );
        };
        assert_eq!(anchored_sig, sig);

        // 2. Observe a valid discharge interval (80% -> 74% over 3 hours) -> learns 1 sample!
        let battery_74 = BatteryInfo {
            percentage: Some(74),
            level: BatteryLevel::Good,
            status: BatteryStatus::Discharging,
        };
        state.battery_telemetry_mut().observe_device(
            &cfg_key,
            true,
            Some(&battery_74),
            Some(&sig),
            nominal_hours,
            t0 + 3 * 3_600,
        );

        let (_, learned, samples) = state.battery_telemetry().query(&cfg_key, &sig);
        assert_eq!(
            samples, 1,
            "RGB mouse without report_rate capability must learn discharge intervals (nominal_hours={nominal_hours})"
        );
        assert!(learned.is_some());
    }
}
