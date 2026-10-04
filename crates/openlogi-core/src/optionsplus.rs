//! Conservative, host-independent conversion of macOS Options+ schema 26.
//! Unknown assignments remain untouched in the target and appear in the report.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::{
    binding::{Binding, ButtonId},
    config::Config,
};

mod actions;
#[cfg(all(feature = "fs", target_os = "macos"))]
mod database;
#[cfg(all(feature = "fs", target_os = "macos"))]
pub use database::read_database;

/// Whether saved capabilities permit assigning mouse buttons. Missing identity
/// is allowed for older configs; this does not establish a physical-device match.
#[must_use]
pub fn can_import_into(device: &crate::config::DeviceConfig) -> bool {
    device.identity.as_ref().is_none_or(|identity| {
        identity.kind == crate::device::DeviceKind::Mouse && identity.capabilities.buttons
    })
}

/// Upper bound for the settings document, before allocating a SQLite blob.
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

/// A database or document that cannot be safely interpreted.
#[derive(Debug, Error)]
pub enum ImportError {
    /// Only the observed schema is supported.
    #[error("expected Options+ schema 26")]
    UnsupportedSchema,
    /// Invalid or ambiguous source relationships, including duplicate identities.
    #[error("invalid Options+ document: {0}")]
    Invalid(String),
    /// JSON syntax or a required field is invalid.
    #[error("could not parse Options+ settings: {0}")]
    Json(#[from] serde_json::Error),
    /// A bounded read failed; a missing database must never become empty settings.
    #[cfg(all(feature = "fs", target_os = "macos"))]
    #[error("could not read Options+ database: {0}")]
    Database(#[from] rusqlite::Error),
}

/// Why an assignment needs the user's attention before importing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    /// No equivalent action or an invalid shortcut payload.
    UnsupportedAction,
    /// Analog wheel direction and timing have not been verified.
    Wheel,
    /// Pointer speed, scroll direction, or hardware settings need manual setup.
    DeviceSetting,
    /// Options+ virtual ring slots are not physical controls.
    VirtualDevice,
    /// The application bundle could not be resolved on this host.
    Application,
    /// The target can only store single-action application overrides.
    AppGesture,
    /// Gesture actions transfer, but Options+ recognition/timing settings do not.
    GestureTiming,
    /// macOS navigation gestures become application keyboard shortcuts.
    Navigation,
    /// The source profile is disabled and is not imported.
    InactiveProfile,
}

/// One skipped assignment or explicit behavior difference in a preview.
#[derive(Clone, Debug)]
pub struct ImportNotice {
    profile: String,
    slot: String,
    kind: NoticeKind,
}

impl ImportNotice {
    /// Source profile name, for local display only.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }
    /// Source slot name, for local display only.
    #[must_use]
    pub fn slot(&self) -> &str {
        &self.slot
    }
    /// Stable reason translated by the presentation layer.
    #[must_use]
    pub fn kind(&self) -> NoticeKind {
        self.kind
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    id: String,
    #[serde(default)]
    name: String,
    application_id: String,
    #[serde(default = "active_by_default")]
    active_for_application: bool,
    base_profile_id: Option<String>,
    assignments: Vec<Assignment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Assignment {
    slot_id: String,
    card: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Application {
    #[serde(rename = "applicationId")]
    id: String,
    #[serde(rename = "applicationPath")]
    path: Option<String>,
    name: Option<String>,
}

/// Validated source settings. Device IDs are never guessed from model names.
#[derive(Debug)]
pub struct OptionsSettings {
    profiles: Vec<Profile>,
    applications: BTreeMap<String, String>,
    devices: BTreeSet<String>,
}

/// A merged candidate and a complete report for the selected source device.
#[derive(Debug)]
pub struct ImportResult {
    config: Config,
    notices: Vec<ImportNotice>,
    imported: usize,
}

impl ImportResult {
    /// Candidate containing only supported assignments over the existing target.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }
    /// Skipped assignments and behavior differences to display before saving.
    #[must_use]
    pub fn notices(&self) -> &[ImportNotice] {
        &self.notices
    }
    /// Number of supported source assignments, including existing equal values.
    #[must_use]
    pub fn imported(&self) -> usize {
        self.imported
    }
}

impl OptionsSettings {
    /// Parse only the profile and application catalog; account and analytics data
    /// are never part of the import model.
    pub fn parse(bytes: &[u8]) -> Result<Self, ImportError> {
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(ImportError::Invalid(
                "settings document is too large".into(),
            ));
        }
        let root: Value = serde_json::from_slice(bytes)?;
        if root["schema_version"].as_u64() != Some(26) {
            return Err(ImportError::UnsupportedSchema);
        }
        let keys = root["profile_keys"]
            .as_array()
            .ok_or_else(|| ImportError::Invalid("missing profile_keys".into()))?;
        if keys.is_empty() || keys.len() > 128 {
            return Err(ImportError::Invalid("invalid profile count".into()));
        }
        let mut profiles = Vec::new();
        let mut ids = BTreeSet::new();
        let mut devices = BTreeSet::new();
        let mut scopes = BTreeSet::new();
        for key in keys {
            let key = key
                .as_str()
                .ok_or_else(|| ImportError::Invalid("invalid profile key".into()))?;
            let profile: Profile = serde_json::from_value(root[key].clone())?;
            if profile.id.is_empty()
                || !ids.insert(profile.id.clone())
                || !scopes.insert(profile.application_id.clone())
                || profile.assignments.len() > 4096
            {
                return Err(ImportError::Invalid(
                    "duplicate profile or too many assignments".into(),
                ));
            }
            let mut slots = BTreeSet::new();
            for assignment in &profile.assignments {
                if !slots.insert(&assignment.slot_id) {
                    return Err(ImportError::Invalid("duplicate assignment slot".into()));
                }
                if let Some((device, _)) = split_slot(&assignment.slot_id) {
                    devices.insert(device.to_owned());
                }
            }
            profiles.push(profile);
        }
        let bases: Vec<_> = profiles
            .iter()
            .filter(|p| p.base_profile_id.is_none())
            .collect();
        if bases.len() != 1 || !bases[0].active_for_application {
            return Err(ImportError::Invalid(
                "expected one active base profile".into(),
            ));
        }
        let base_id = &bases[0].id;
        if profiles
            .iter()
            .any(|p| p.base_profile_id.as_ref().is_some_and(|id| id != base_id))
        {
            return Err(ImportError::Invalid("unresolved baseProfileId".into()));
        }
        let apps = root
            .pointer("/applications/applications")
            .and_then(Value::as_array)
            .ok_or_else(|| ImportError::Invalid("missing application catalog".into()))?;
        let mut applications = BTreeMap::new();
        let mut names = BTreeMap::new();
        let mut app_ids = BTreeSet::new();
        for app in apps {
            let app: Application = serde_json::from_value(app.clone())?;
            if !app_ids.insert(app.id.clone()) {
                return Err(ImportError::Invalid(
                    "duplicate application identity".into(),
                ));
            }
            if let Some(name) = app.name {
                names.insert(app.id.clone(), name);
            }
            if let Some(path) = app.path {
                applications.insert(app.id, path);
            }
        }
        for profile in &mut profiles {
            if profile.name.is_empty() {
                profile.name = names
                    .get(&profile.application_id)
                    .unwrap_or(&profile.application_id)
                    .clone();
            }
        }
        Ok(Self {
            profiles,
            applications,
            devices,
        })
    }

    /// Physical source prefixes available for explicit device selection.
    pub fn devices(&self) -> impl Iterator<Item = &str> {
        self.devices.iter().map(String::as_str)
    }

    /// Merge one explicitly selected source device into an existing target.
    /// Resolve macOS bundle IDs from installed bundles, never display names.
    /// Equal application overrides are retained: Options+ keeps them independent
    /// when a later global edit changes the same button.
    pub fn merge(
        &self,
        current: &Config,
        source_device: &str,
        target_device: &str,
        resolve_application: impl Fn(&str) -> Option<String>,
    ) -> Result<ImportResult, ImportError> {
        if !self.devices.contains(source_device) || !current.devices.contains_key(target_device) {
            return Err(ImportError::Invalid(
                "select a source and an existing OpenLogi device".into(),
            ));
        }
        if !current
            .devices
            .get(target_device)
            .is_some_and(can_import_into)
        {
            return Err(ImportError::Invalid(
                "the selected OpenLogi device does not support mouse button assignments".into(),
            ));
        }
        let mut result = ImportResult {
            config: current.clone(),
            notices: Vec::new(),
            imported: 0,
        };
        let mut resolved = BTreeSet::new();
        for profile in &self.profiles {
            if !profile.active_for_application {
                result.notice(profile, "", NoticeKind::InactiveProfile);
                continue;
            }
            let application = if profile.base_profile_id.is_some() {
                let app = self
                    .applications
                    .get(&profile.application_id)
                    .and_then(|path| resolve_application(path));
                let Some(app) = app.filter(|id| !id.is_empty()) else {
                    result.notice(profile, "", NoticeKind::Application);
                    continue;
                };
                if !resolved.insert(app.clone()) {
                    return Err(ImportError::Invalid(
                        "two profiles resolve to the same application".into(),
                    ));
                }
                Some(app)
            } else {
                None
            };
            for assignment in &profile.assignments {
                let Some((device, slot)) = split_slot(&assignment.slot_id) else {
                    result.notice(profile, &assignment.slot_id, NoticeKind::VirtualDevice);
                    continue;
                };
                if device != source_device {
                    continue;
                }
                let button = match slot {
                    "c82" => ButtonId::MiddleClick,
                    "c83" => ButtonId::Back,
                    "c86" => ButtonId::Forward,
                    "c195" => ButtonId::GestureButton,
                    "c196" => ButtonId::DpiToggle,
                    "thumb_wheel_adapter" => {
                        result.notice(profile, slot, NoticeKind::Wheel);
                        continue;
                    }
                    _ => {
                        result.notice(profile, slot, NoticeKind::DeviceSetting);
                        continue;
                    }
                };
                let Some(binding) = actions::binding(&assignment.card, button) else {
                    result.notice(profile, slot, NoticeKind::UnsupportedAction);
                    continue;
                };
                if application.is_some() && !matches!(binding, Binding::Single(_)) {
                    result.notice(profile, slot, NoticeKind::AppGesture);
                    continue;
                }
                if matches!(binding, Binding::Gesture(_)) {
                    result.notice(profile, slot, NoticeKind::GestureTiming);
                }
                if actions::uses_navigation(&binding) {
                    result.notice(profile, slot, NoticeKind::Navigation);
                }
                // Target membership was validated before any conversion.
                let Some(target) = result.config.devices.get_mut(target_device) else {
                    unreachable!()
                };
                if let (Some(app), Binding::Single(action)) = (&application, &binding) {
                    target
                        .per_app_bindings
                        .entry(app.clone())
                        .or_default()
                        .insert(button, action.clone());
                } else {
                    target.bindings.insert(button, binding);
                    target.disabled_gestures.remove(&button);
                }
                result.imported += 1;
            }
        }
        Ok(result)
    }
}

impl ImportResult {
    fn notice(&mut self, profile: &Profile, slot: &str, kind: NoticeKind) {
        self.notices.push(ImportNotice {
            profile: profile.name.clone(),
            slot: slot.into(),
            kind,
        });
    }
}

fn active_by_default() -> bool {
    true
}

fn split_slot(slot: &str) -> Option<(&str, &str)> {
    let (device, slot) = slot.split_once('_')?;
    // Only the MX Master 3S layout has been checked against real macOS data.
    device
        .starts_with("mx-master-3s-")
        .then_some((device, slot))
}

#[cfg(test)]
mod tests;
