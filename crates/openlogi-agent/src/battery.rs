//! Native battery projection and durable, once-per-discharge notifications.
//!
//! Inventory and config reloads feed one synchronized owner. Native menus only
//! clone its presentation snapshot; neither menu thread performs device I/O.

use std::collections::BTreeSet;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use atomic_write_file::AtomicWriteFile;
use openlogi_agent_core::battery::Observation;
use openlogi_core::battery::Severity;
use openlogi_core::device::BatteryInfo;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// A missed recovery scan must not leave an old reading looking current forever.
const MAX_READING_AGE: Duration = Duration::from_secs(90);

/// A native device row. Percent, charging and severity are already validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceBattery {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) percentage: Option<u8>,
    pub(crate) charging: bool,
    pub(crate) severity: Severity,
}

impl DeviceBattery {
    /// Full semantic label retained even when the row draws a compact badge.
    pub(crate) fn accessible_label(&self) -> String {
        let value = self.percentage.map_or_else(
            || rust_i18n::t!("device.battery_unknown").to_string(),
            |value| format!("{value}%"),
        );
        let status = if self.charging {
            rust_i18n::t!("device.battery_tray_charging").to_string()
        } else {
            match self.severity {
                Severity::Normal => String::new(),
                Severity::Low => rust_i18n::t!("device.battery_tray_low").to_string(),
                Severity::Critical => rust_i18n::t!("device.battery_tray_critical").to_string(),
            }
        };
        if status.is_empty() {
            format!("{}: {value}", self.name)
        } else {
            format!("{}: {value}, {status}", self.name)
        }
    }
}

/// Only selected menu rows, plus the worst state among all warning-enabled devices.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) devices: Vec<DeviceBattery>,
    pub(crate) severity: Severity,
}

/// An accepted transition into a low-battery episode.
#[derive(Debug)]
pub(crate) struct Alert {
    pub(crate) name: String,
    pub(crate) percentage: u8,
}

impl Alert {
    pub(crate) fn title(&self) -> String {
        rust_i18n::t!("device.battery_alert_title", name = &self.name).to_string()
    }

    pub(crate) fn body(&self) -> String {
        rust_i18n::t!("device.battery_alert_body", percentage = self.percentage).to_string()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct History {
    /// Canonical physical keys only. Anonymous devices never persist here.
    warned: BTreeSet<String>,
    battery_devices: BTreeSet<String>,
}

struct State {
    history: History,
    persisted_history: History,
    path: Option<PathBuf>,
    session_warned: BTreeSet<String>,
    session_batteries: BTreeSet<String>,
    observations: Vec<Observation>,
    observed_at: Option<Instant>,
    snapshot: Snapshot,
}

impl State {
    fn new(path: Option<PathBuf>) -> Self {
        let history =
            path.as_ref()
                .map_or_else(History::default, |path| match std::fs::read(path) {
                    Ok(bytes) => match serde_json::from_slice(&bytes) {
                        Ok(history) => history,
                        Err(error) => {
                            warn!(%error, "battery warning history could not be decoded");
                            History::default()
                        }
                    },
                    Err(error) if error.kind() == io::ErrorKind::NotFound => History::default(),
                    Err(error) => {
                        warn!(%error, "battery warning history could not be read");
                        History::default()
                    }
                });
        Self {
            persisted_history: history.clone(),
            history,
            path,
            session_warned: BTreeSet::new(),
            session_batteries: BTreeSet::new(),
            observations: Vec::new(),
            observed_at: None,
            snapshot: Snapshot::default(),
        }
    }

    fn reconcile(&mut self, now: Instant) -> (bool, Vec<Alert>) {
        // An unpaired slot may next contain another device, even the same model.
        // Sleeping paired devices remain in observations and keep their episode.
        let present: BTreeSet<&str> = self
            .observations
            .iter()
            .filter(|observation| observation.history_key.is_none())
            .map(|observation| observation.session_key.as_str())
            .collect();
        self.session_warned
            .retain(|key| present.contains(key.as_str()));
        self.session_batteries
            .retain(|key| present.contains(key.as_str()));
        let current = self
            .observed_at
            .is_some_and(|observed| now.saturating_duration_since(observed) <= MAX_READING_AGE);
        let mut next = Snapshot::default();
        let mut alerts = Vec::new();
        for observation in &self.observations {
            if !observation.online {
                continue;
            }
            let (id, known, warned) = match &observation.history_key {
                Some(key) => (
                    key,
                    &mut self.history.battery_devices,
                    &mut self.history.warned,
                ),
                None => (
                    &observation.session_key,
                    &mut self.session_batteries,
                    &mut self.session_warned,
                ),
            };
            if observation.battery.is_some() {
                known.insert(id.clone());
            }
            if !known.contains(id) {
                continue;
            }
            let reading = current.then_some(observation.battery.as_ref()).flatten();
            let percentage = reading.and_then(BatteryInfo::usable_percentage);
            let severity = reading.map_or(Severity::Normal, BatteryInfo::severity);
            let charging = reading.is_some_and(|battery| {
                battery.freshness != openlogi_core::device::BatteryFreshness::Cached
                    && battery.is_charging()
            });
            if reading.is_some_and(BatteryInfo::rearms_warning) {
                warned.remove(id);
            }
            if observation.preferences.warn_low {
                next.severity = next.severity.max(severity);
                if severity != Severity::Normal
                    && let Some(percentage) = percentage
                    && warned.insert(id.clone())
                {
                    alerts.push(Alert {
                        name: observation.name.clone(),
                        percentage,
                    });
                }
            }
            if observation.preferences.show_in_menu {
                next.devices.push(DeviceBattery {
                    key: observation.key.clone(),
                    name: observation.name.clone(),
                    percentage,
                    charging,
                    severity,
                });
            }
        }
        // Record the episode before dispatch. OS suppression or a restart during
        // delivery must not turn a low battery into a stream of notifications.
        if self.history != self.persisted_history {
            match self.save() {
                Ok(()) => self.persisted_history = self.history.clone(),
                Err(error) => {
                    warn!(%error, "battery warning history could not be saved; will retry while suppressing repeats in memory");
                }
            }
        }
        let changed = next != self.snapshot;
        self.snapshot = next;
        (changed, alerts)
    }

    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(&self.history).map_err(io::Error::other)?;
        #[cfg_attr(
            not(unix),
            expect(unused_mut, reason = "Unix applies private file permissions")
        )]
        let mut options = AtomicWriteFile::options();
        #[cfg(unix)]
        {
            use atomic_write_file::unix::OpenOptionsExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            options.preserve_mode(false).mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(&bytes)?;
        file.commit()
    }
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        let path = match openlogi_core::paths::state_dir() {
            Ok(path) => Some(path.join("battery-warnings.json")),
            Err(error) => {
                warn!(%error, "battery warning history unavailable; using session memory");
                None
            }
        };
        Mutex::new(State::new(path))
    })
}

/// The latest presentation, callable from either platform's native UI thread.
pub(crate) fn snapshot() -> Snapshot {
    state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .snapshot
        .clone()
}

fn change(update: impl FnOnce(&mut State)) {
    let (changed, alerts) = {
        let mut state = state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        update(&mut state);
        state.reconcile(Instant::now())
    };
    if changed {
        #[cfg(target_os = "macos")]
        crate::tray::battery_changed();
        #[cfg(target_os = "windows")]
        crate::tray_windows::battery_changed();
    }
    for alert in alerts {
        #[cfg(target_os = "macos")]
        crate::tray::notify_battery(&alert);
        #[cfg(target_os = "windows")]
        crate::tray_windows::notify_battery(&alert);
    }
}

/// A completed inventory pass supplies new observations and restarts their lease.
pub(crate) fn refresh_inventory(observations: Vec<Observation>) {
    change(|state| {
        state.observations = observations;
        state.observed_at = Some(Instant::now());
    });
}

/// Preferences change immediately without making an old measurement fresh again.
pub(crate) fn refresh_preferences(observations: Vec<Observation>) {
    change(|state| state.observations = observations);
}

/// Sleep/resume or unavailable inventory invalidates readings until a new probe.
pub(crate) fn invalidate() {
    change(|state| state.observed_at = None);
}

/// Bounded expiry also covers a stopped inventory watcher after prior success.
pub(crate) fn expire() {
    change(|_| {});
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::config::BatteryPreferences;
    use openlogi_core::device::{BatteryFreshness, BatteryLevel, BatteryStatus};

    fn observation(percentage: u8) -> Observation {
        Observation {
            key: "unit:01020304".into(),
            history_key: Some("unit:01020304".into()),
            session_key: "receiver:receiver:slot:1:model:keyboard".into(),
            online: true,
            name: "Keyboard".into(),
            battery: Some(BatteryInfo {
                percentage,
                level: BatteryLevel::Good,
                status: BatteryStatus::Discharging,
                freshness: BatteryFreshness::Current,
            }),
            preferences: BatteryPreferences::default(),
        }
    }

    fn read(state: &mut State, observation: Observation, now: Instant) -> Vec<Alert> {
        state.observed_at = Some(now);
        state.observations = vec![observation];
        state.reconcile(now).1
    }

    #[test]
    fn discharge_episode_survives_reconnect_restart_and_brief_charging() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let now = Instant::now();
        let mut state = State::new(Some(path.clone()));
        assert_eq!(read(&mut state, observation(20), now).len(), 1);
        assert!(read(&mut state, observation(8), now).is_empty());
        state.observations.clear();
        state.reconcile(now);
        let mut state = State::new(Some(path));
        assert!(read(&mut state, observation(8), now).is_empty());
        let mut charging = observation(8);
        charging.battery.as_mut().unwrap().status = BatteryStatus::Charging;
        assert!(read(&mut state, charging, now).is_empty());
        assert_eq!(state.snapshot.severity, Severity::Normal);
        assert!(read(&mut state, observation(8), now).is_empty());
        assert!(read(&mut state, observation(25), now).is_empty());
        assert!(read(&mut state, observation(20), now).is_empty());
        assert!(read(&mut state, observation(26), now).is_empty());
        assert_eq!(read(&mut state, observation(20), now).len(), 1);
    }

    #[test]
    fn hidden_devices_warn_but_muted_visible_devices_do_not_color_brand() {
        let mut state = State::new(None);
        let now = Instant::now();
        let mut hidden = observation(8);
        hidden.preferences.show_in_menu = false;
        assert_eq!(read(&mut state, hidden, now).len(), 1);
        assert!(state.snapshot.devices.is_empty());
        assert_eq!(state.snapshot.severity, Severity::Critical);
        let mut muted = observation(8);
        muted.preferences.warn_low = false;
        assert!(read(&mut state, muted, now).is_empty());
        assert_eq!(state.snapshot.devices[0].severity, Severity::Critical);
        assert_eq!(state.snapshot.severity, Severity::Normal);
    }

    #[test]
    fn cache_replay_missing_readings_and_expired_inventory_never_warn() {
        let mut state = State::new(None);
        let now = Instant::now();
        let mut cached = observation(8);
        cached.battery.as_mut().unwrap().freshness = BatteryFreshness::Cached;
        assert!(read(&mut state, cached, now).is_empty());
        assert_eq!(state.snapshot.devices[0].percentage, None);
        let mut missing = observation(8);
        missing.battery = None;
        assert!(read(&mut state, missing, now).is_empty());
        assert_eq!(state.snapshot.devices[0].percentage, None);
        read(&mut state, observation(70), now);
        state.observations = vec![observation(8)];
        assert!(
            state
                .reconcile(now + MAX_READING_AGE + Duration::from_secs(1))
                .1
                .is_empty()
        );
        assert_eq!(state.snapshot.devices[0].percentage, None);
        assert_eq!(state.snapshot.severity, Severity::Normal);
        assert!(!state.history.warned.contains("unit:01020304"));
    }

    #[test]
    fn anonymous_devices_do_not_inherit_persisted_warning_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let mut state = State::new(Some(path.clone()));
        let mut anonymous = observation(8);
        anonymous.history_key = None;
        assert_eq!(read(&mut state, anonymous.clone(), Instant::now()).len(), 1);
        let mut restarted = State::new(Some(path));
        assert_eq!(read(&mut restarted, anonymous, Instant::now()).len(), 1);
        assert!(restarted.history.warned.is_empty());
    }
    #[test]
    fn removed_anonymous_pairing_does_not_suppress_its_replacement() {
        let mut state = State::new(None);
        let now = Instant::now();
        let mut anonymous = observation(8);
        anonymous.history_key = None;
        assert_eq!(read(&mut state, anonymous.clone(), now).len(), 1);
        assert!(read(&mut state, anonymous.clone(), now).is_empty());
        let mut sleeping = anonymous.clone();
        sleeping.online = false;
        assert!(read(&mut state, sleeping, now).is_empty());
        assert!(state.snapshot.devices.is_empty());
        assert_eq!(state.snapshot.severity, Severity::Normal);
        assert!(read(&mut state, anonymous.clone(), now).is_empty());
        state.observations.clear();
        state.reconcile(now);
        assert_eq!(read(&mut state, anonymous, now).len(), 1);
    }

    #[test]
    fn changed_anonymous_model_starts_a_new_warning_episode() {
        let mut state = State::new(None);
        let now = Instant::now();
        let mut anonymous = observation(8);
        anonymous.history_key = None;
        assert_eq!(read(&mut state, anonymous.clone(), now).len(), 1);
        anonymous.session_key = "receiver:receiver:slot:1:model:mouse".into();
        assert_eq!(read(&mut state, anonymous, now).len(), 1);
        assert_eq!(state.session_warned.len(), 1);
    }

    #[test]
    fn temporary_save_failure_is_retried_without_another_battery_transition() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("unavailable");
        std::fs::write(&parent, b"not a directory").unwrap();
        let path = parent.join("warnings.json");
        let mut state = State::new(Some(path.clone()));
        let now = Instant::now();
        assert_eq!(read(&mut state, observation(8), now).len(), 1);
        std::fs::remove_file(&parent).unwrap();
        assert!(read(&mut state, observation(8), now).is_empty());
        let mut restarted = State::new(Some(path));
        assert!(read(&mut restarted, observation(8), now).is_empty());
    }
}
