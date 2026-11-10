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
//!    across real-world discharge cycles, persisted to a sidecar JSON file in
//!    `openlogi_core::paths::config_dir()` so `config.toml`'s strict schema is
//!    never altered.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus, DeviceKind};
use openlogi_core::hid::{BacklightMode, BacklightState, BacklightStatus};
use serde::{Deserialize, Serialize};

use crate::state::{AppState, BacklightLoad, DeviceRecord, ReportRateLoad};

const TELEMETRY_FILE: &str = "battery-telemetry.json";
/// Minimum elapsed discharging window (30 minutes) and minimum drop (1%) before
/// updating the learned discharge rate EMA.
const MIN_LEARN_WINDOW_SECS: u64 = 1_800;
const MAX_LEARN_WINDOW_SECS: u64 = 14 * 86_400;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TelemetryStore {
    #[serde(default)]
    devices: BTreeMap<String, DeviceBatteryHistory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceBatteryHistory {
    /// Last observed battery percentage (`0..=100`).
    last_percentage: u8,
    /// Last observation Unix timestamp (seconds).
    last_timestamp_secs: u64,
    /// Configuration signature active at the last observation.
    last_signature: String,
    /// Whether the device was charging at the last observation.
    was_charging: bool,
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

const fn default_health() -> u8 {
    100
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct LearnedDrain {
    /// Exponential moving average of `% per day` under this configuration signature.
    pct_per_day: f32,
    /// Number of valid discharge intervals folded into this EMA.
    samples: u32,
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
    /// Number of historical discharge intervals learned for this device/config.
    pub learned_samples: u32,
    /// Short human-readable summary of the active power profile (e.g. `"Brilho 4/7 · 15s"`).
    pub config_note: Option<String>,
}

impl BatteryEstimate {
    /// Compact label for the Home screen list row & grid card (Image 1).
    /// Example (PT): `"~9 dias restantes · Saúde 100%"` or `"~5 meses (Luz off) · Saúde 100%"`
    /// Example (EN): `"~9d left · Health 100%"`
    #[must_use]
    pub fn home_label(&self) -> String {
        let pt = is_portuguese();
        let dur = format_duration_short(self.remaining_hours, pt);
        if pt {
            match &self.config_note {
                Some(note) => format!("~{dur} ({note}) · Saúde {}%", self.health_pct),
                None => format!("~{dur} restantes · Saúde {}%", self.health_pct),
            }
        } else {
            match &self.config_note {
                Some(note) => format!("~{dur} left ({note}) · Health {}%", self.health_pct),
                None => format!("~{dur} left · Health {}%", self.health_pct),
            }
        }
    }

    /// Ultra-compact pill for the detail header bar or grid corner.
    #[must_use]
    pub fn inline_label(&self) -> String {
        let pt = is_portuguese();
        format!("~{}", format_duration_compact(self.remaining_hours, pt))
    }

    /// Detailed multi-part summary for the Device Details card (Image 2).
    #[must_use]
    pub fn detail_headline(&self) -> String {
        let pt = is_portuguese();
        let dur = format_duration_long(self.remaining_hours, pt);
        if pt {
            match &self.config_note {
                Some(note) => format!("~{dur} restantes ({note})"),
                None => format!("~{dur} restantes"),
            }
        } else {
            match &self.config_note {
                Some(note) => format!("~{dur} remaining ({note})"),
                None => format!("~{dur} remaining"),
            }
        }
    }

    /// Secondary caption showing full-charge expectation, health, and learning state.
    #[must_use]
    pub fn detail_subline(&self) -> String {
        let pt = is_portuguese();
        let full = format_duration_short(self.full_charge_hours, pt);
        if pt {
            let source = if self.learned_samples > 0 {
                format!("Histórico ({} amostras)", self.learned_samples)
            } else {
                "Baseado na config atual".to_owned()
            };
            format!(
                "Saúde: {}% · Carga 100%: ~{full} · {source}",
                self.health_pct
            )
        } else {
            let source = if self.learned_samples > 0 {
                format!("Learned ({} samples)", self.learned_samples)
            } else {
                "Based on active config".to_owned()
            };
            format!(
                "Health: {}% · 100% charge: ~{full} · {source}",
                self.health_pct
            )
        }
    }
}

fn is_portuguese() -> bool {
    let loc = rust_i18n::locale();
    loc.starts_with("pt")
}

fn format_duration_compact(hours: u32, pt: bool) -> String {
    if hours < 24 {
        format!("{hours}h")
    } else if hours < 24 * 60 {
        let days = (hours + 12) / 24;
        format!("{days}d")
    } else {
        let months = ((hours as f32) / (24.0 * 30.0)).round().max(1.0) as u32;
        if pt {
            format!("{months}m")
        } else {
            format!("{months}mo")
        }
    }
}

fn format_duration_short(hours: u32, pt: bool) -> String {
    if hours < 24 {
        if pt {
            format!("{hours} h")
        } else {
            format!("{hours}h")
        }
    } else if hours < 24 * 45 {
        let days = (hours + 12) / 24;
        if pt {
            if days == 1 {
                "1 dia".to_owned()
            } else {
                format!("{days} dias")
            }
        } else {
            format!("{days}d")
        }
    } else {
        let months = (hours as f32 / (24.0 * 30.0) * 10.0).round() / 10.0;
        let whole = (months.fract() - 0.0).abs() < 0.05;
        if pt {
            if whole {
                let m = months.round() as u32;
                if m == 1 {
                    "1 mês".to_owned()
                } else {
                    format!("{m} meses")
                }
            } else {
                format!("{months:.1} meses")
            }
        } else if whole {
            format!("{} mo", months.round() as u32)
        } else {
            format!("{months:.1} mo")
        }
    }
}

fn format_duration_long(hours: u32, pt: bool) -> String {
    format_duration_short(hours, pt)
}

/// Compute the battery duration & health estimate for `record` given current `AppState`.
pub(crate) fn estimate_for_device(state: &AppState, record: &DeviceRecord) -> Option<BatteryEstimate> {
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

    let (nominal_full_hours, signature, config_note) =
        nominal_profile_for_device(state, record);

    let (health_pct, learned_drain, learned_samples) = record_and_query_telemetry(
        &record.config_key,
        percentage,
        false,
        &signature,
        nominal_full_hours,
        record.is_online(),
    );

    // Nominal % per day at 100% health:
    let nominal_pct_per_day = (100.0 * 24.0) / (nominal_full_hours.max(1) as f32);
    // Adjust nominal drain by battery health (lower health -> faster drain):
    let health_factor = (health_pct.clamp(50, 100) as f32) / 100.0;
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
    let remaining_days = (percentage as f32) / effective_pct_per_day;

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

/// Returns `(nominal_full_charge_hours, config_signature, human_config_note)`
fn nominal_profile_for_device(
    state: &AppState,
    record: &DeviceRecord,
) -> (u32, String, Option<String>) {
    let pt = is_portuguese();
    let model_lower = record.model_name.to_ascii_lowercase();
    let key = record.device_key();

    match record.kind {
        DeviceKind::Keyboard => {
            let has_backlight = record
                .capabilities
                .is_some_and(|c| c.backlight)
                || model_lower.contains("mx keys");

            if has_backlight {
                if let Some(BacklightLoad::Ready(bl)) = state.backlight_load_for(&key) {
                    return keyboard_backlight_profile(&model_lower, **bl, pt);
                }
                // Default MX Keys Mini profile when backlight was configured to Manual 4/7 · 15s
                if model_lower.contains("mx keys mini") {
                    let note = if pt {
                        "Brilho 4/7 · 15s".to_owned()
                    } else {
                        "Level 4/7 · 15s".to_owned()
                    };
                    return (10 * 24, "kbd:bl-default-l4-15s".to_owned(), Some(note));
                }
            }

            // Check RGB lighting for gaming keyboards
            if let Some(lighting) = state.lighting_for(&record.config_key, &record.route_key) {
                if lighting.enabled {
                    let note = if pt { "RGB ativo" } else { "RGB on" };
                    return (40, "kbd:rgb-on".to_owned(), Some(note.to_owned()));
                }
                let note = if pt { "RGB desligado" } else { "RGB off" };
                return (180 * 24, "kbd:rgb-off".to_owned(), Some(note.to_owned()));
            }

            // Standard non-backlit / unknown keyboard
            (120 * 24, "kbd:standard".to_owned(), None)
        }
        DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad => {
            // Check report rate if available
            let rate_hz = match state.report_rate_load_for(&key) {
                Some(ReportRateLoad::Ready(info)) => Some(u16::from(info.current)),
                _ => None,
            };
            let rgb_on = state
                .lighting_for(&record.config_key, &record.route_key)
                .is_some_and(|l| l.enabled);

            if model_lower.contains("mx master") {
                // MX Master 3 / 3S: Logitech spec is 70 days (1680 hours) on a full charge
                return (70 * 24, "mouse:mx-master".to_owned(), None);
            }
            if model_lower.contains("mx anywhere") {
                return (70 * 24, "mouse:mx-anywhere".to_owned(), None);
            }
            if model_lower.contains("mx vertical") || model_lower.contains("lift") {
                return (120 * 24, "mouse:ergo".to_owned(), None);
            }
            if model_lower.contains("g502") || model_lower.contains("pro x") || model_lower.contains("g pro") {
                let mut base_hours: f32 = if rgb_on { 45.0 } else { 130.0 };
                if let Some(hz) = rate_hz {
                    if hz >= 2000 {
                        base_hours *= 0.65;
                    } else if hz <= 250 {
                        base_hours *= 1.25;
                    }
                }
                let note = match (rgb_on, rate_hz) {
                    (true, Some(hz)) => Some(format!("RGB · {hz}Hz")),
                    (false, Some(hz)) => Some(format!("{hz}Hz")),
                    (true, None) => Some(if pt { "RGB ativo".to_owned() } else { "RGB on".to_owned() }),
                    (false, None) => None,
                };
                let sig = format!("mouse:gaming:rgb={rgb_on}:hz={}", rate_hz.unwrap_or(1000));
                return (base_hours.round().max(10.0) as u32, sig, note);
            }

            (65 * 24, "mouse:generic".to_owned(), None)
        }
        DeviceKind::Headset => (24, "headset:default".to_owned(), None),
        _ => (30 * 24, "other:default".to_owned(), None),
    }
}

fn keyboard_backlight_profile(
    model_lower: &str,
    bl: BacklightState,
    pt: bool,
) -> (u32, String, Option<String>) {
    // MX Keys Mini spec: up to 10 days (240h) with backlight ON, up to 5 months (150 days = 3600h) with backlight OFF.
    // Full-size MX Keys S: similar (10 days backlit / 150 days off).
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
        let note = if pt {
            "Luz desligada".to_owned()
        } else {
            "Backlight off".to_owned()
        };
        return (off_hours, "kbd:bl-off".to_owned(), Some(note));
    }

    let max_level = bl.nb_levels.saturating_sub(1).max(1);
    let level = bl.current_level.min(max_level);
    let timeout_secs = bl.duration.map_or(15, |d| d.seconds()).max(5);

    // Base full-charge hours at mid-level (4/7) and 15s timeout is 10 days (240h).
    // At level 1/7 + 5s timeout, battery lasts ~24 days (~576h).
    // At level 7/7 + 300s timeout, battery lasts ~3.5 days (~84h).
    let level_ratio = if bl.mode == BacklightMode::Automatic {
        0.50
    } else {
        (level as f32) / (max_level as f32)
    };
    let level_drain_factor = 0.30 + 0.95 * level_ratio.powf(1.25);
    let duration_drain_factor = ((timeout_secs as f32) / 15.0).powf(0.32).clamp(0.68, 3.2);
    let power_save_bonus = if bl.power_save { 0.92 } else { 1.0 };

    let total_drain_factor = level_drain_factor * duration_drain_factor * power_save_bonus;
    let estimated_hours = ((10.0 * 24.0) / total_drain_factor)
        .round()
        .clamp(48.0, (off_hours as f32) * 0.85) as u32;

    let dur_str = if timeout_secs < 60 {
        format!("{timeout_secs}s")
    } else if timeout_secs < 3600 {
        format!("{}m", timeout_secs / 60)
    } else {
        format!("{}h", timeout_secs / 3600)
    };

    let note = if bl.mode == BacklightMode::Automatic {
        if pt {
            format!("Luz Auto · {dur_str}")
        } else {
            format!("Auto light · {dur_str}")
        }
    } else if pt {
        format!("Brilho {level}/{max_level} · {dur_str}")
    } else {
        format!("Brightness {level}/{max_level} · {dur_str}")
    };

    let sig = format!(
        "kbd:bl-on:mode={:?}:lvl={level}/{max_level}:dur={timeout_secs}:ps={}",
        bl.mode, bl.power_save
    );
    (estimated_hours, sig, Some(note))
}

fn telemetry_path() -> Option<PathBuf> {
    openlogi_core::paths::config_dir()
        .ok()
        .map(|dir| dir.join(TELEMETRY_FILE))
}

fn telemetry_cache() -> &'static Mutex<TelemetryStore> {
    static STORE: OnceLock<Mutex<TelemetryStore>> = OnceLock::new();
    STORE.get_or_init(|| {
        let loaded = telemetry_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|raw| serde_json::from_str::<TelemetryStore>(&raw).ok())
            .unwrap_or_default();
        Mutex::new(loaded)
    })
}

fn record_and_query_telemetry(
    config_key: &str,
    percentage: u8,
    charging: bool,
    signature: &str,
    nominal_full_hours: u32,
    online: bool,
) -> (u8, Option<LearnedDrain>, u32) {
    let Ok(mut guard) = telemetry_cache().lock() else {
        return (100, None, 0);
    };

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    let mut dirty = false;
    let entry = guard
        .devices
        .entry(config_key.to_owned())
        .or_insert_with(|| {
            dirty = true;
            DeviceBatteryHistory {
                last_percentage: percentage,
                last_timestamp_secs: now_secs,
                last_signature: signature.to_owned(),
                was_charging: charging,
                learned_drain_per_day: BTreeMap::new(),
                estimated_health_pct: 100,
                total_observed_drop_pct: 0,
            }
        });

    if online && now_secs > entry.last_timestamp_secs {
        let dt_secs = now_secs.saturating_sub(entry.last_timestamp_secs);
        if charging || percentage > entry.last_percentage {
            // Recharged or currently charging: reset baseline to new top percentage
            if entry.last_percentage != percentage
                || entry.was_charging != charging
                || entry.last_signature != signature
            {
                entry.last_percentage = percentage;
                entry.last_timestamp_secs = now_secs;
                entry.last_signature = signature.to_owned();
                entry.was_charging = charging;
                dirty = true;
            }
        } else if entry.last_signature != signature {
            // Configuration profile changed: reset observation anchor for the new signature
            entry.last_percentage = percentage;
            entry.last_timestamp_secs = now_secs;
            entry.last_signature = signature.to_owned();
            entry.was_charging = false;
            dirty = true;
        } else if percentage < entry.last_percentage && !entry.was_charging {
            let drop_pct = entry.last_percentage - percentage;
            if (MIN_LEARN_WINDOW_SECS..=MAX_LEARN_WINDOW_SECS).contains(&dt_secs) && drop_pct >= 1 {
                let dt_days = (dt_secs as f32) / 86_400.0;
                let observed_pct_per_day = ((drop_pct as f32) / dt_days).clamp(0.15, 120.0);

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

                entry.total_observed_drop_pct =
                    entry.total_observed_drop_pct.saturating_add(u32::from(drop_pct));

                // Update battery health estimate once we have at least 10% total observed drop:
                if entry.total_observed_drop_pct >= 10 {
                    let nominal_pct_per_day =
                        (100.0 * 24.0) / (nominal_full_hours.max(1) as f32);
                    let ratio = (nominal_pct_per_day / drain.pct_per_day).clamp(0.50, 1.05);
                    let implied_health = (ratio * 100.0).round().clamp(50.0, 100.0) as u8;
                    entry.estimated_health_pct =
                        ((u16::from(entry.estimated_health_pct) * 4 + u16::from(implied_health))
                            / 5) as u8;
                }

                entry.last_percentage = percentage;
                entry.last_timestamp_secs = now_secs;
                dirty = true;
            }
        }
    }

    let health_pct = entry.estimated_health_pct.clamp(50, 100);
    let learned = entry.learned_drain_per_day.get(signature).copied();
    let total_samples: u32 = entry
        .learned_drain_per_day
        .values()
        .map(|d| d.samples)
        .sum();

    if dirty {
        if let Some(path) = telemetry_path() {
            if let Ok(json) = serde_json::to_string_pretty(&*guard) {
                let _ = fs::write(path, json);
            }
        }
    }

    (health_pct, learned, total_samples)
}
