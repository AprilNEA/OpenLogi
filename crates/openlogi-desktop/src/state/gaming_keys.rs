//! A G-series keyboard's gaming cluster: whether OpenLogi runs G1–G5 instead
//! of the keyboard's onboard actions, and what M1–M3 do then.
//!
//! Both are per-device `config.toml` settings the agent reads when it builds
//! the keyboard's capture session; the bindings themselves are ordinary
//! per-device bindings ([`openlogi_core::binding::ButtonId::GKey`]).

use openlogi_core::binding::GKeyProfile;
use openlogi_core::config::{GKeyOwner, GamingKeyMode};

use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::{AppState, StateEvent};

/// The selected keyboard's gaming cluster as the Keys tab shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GamingKeys {
    /// Whether M1–M3 report presses (`0x8020`).
    pub(crate) m_keys: bool,
    /// Whether MR reports presses (`0x8030`).
    pub(crate) macro_record: bool,
    /// Whether OpenLogi runs G1–G5 instead of the onboard actions.
    pub(crate) software_control: bool,
    /// What M1–M3 do while OpenLogi runs G1–G5.
    pub(crate) mode: GamingKeyMode,
}

impl AppState {
    /// The selected device's gaming cluster, or `None` when it has no
    /// `0x8010` G-keys.
    #[must_use]
    pub(crate) fn current_gaming_keys(&self) -> Option<GamingKeys> {
        let record = self.current_record()?;
        let capabilities = record.capabilities.filter(|c| c.g_keys)?;
        let key = record.persistent_config_key();
        Some(GamingKeys {
            m_keys: capabilities.m_keys,
            macro_record: capabilities.macro_record,
            software_control: key
                .is_some_and(|key| self.config.g_key_owner(key) == GKeyOwner::OpenLogi),
            mode: key.map_or_else(GamingKeyMode::default, |key| {
                self.config.gaming_key_mode(key)
            }),
        })
    }

    /// Hand the selected keyboard's G1–G5 to `owner` and persist it. The
    /// agent takes or releases the row on reload.
    pub(crate) fn commit_g_key_owner(&mut self, owner: GKeyOwner) -> StateEvents {
        let events = self.for_current_device(StateEvent::BindingsChanged);
        let Some(key) = self.gaming_config_key() else {
            return events;
        };
        if self.config.g_key_owner(&key) != owner {
            self.config
                .edit(|config| config.set_g_key_owner(&key, owner));
            self.persist_and_reload("G-key owner");
        }
        events
    }

    /// Persist what M1–M3 do on the selected keyboard.
    pub(crate) fn commit_gaming_key_mode(&mut self, mode: GamingKeyMode) -> StateEvents {
        let events = self.for_current_device(StateEvent::BindingsChanged);
        let Some(key) = self.gaming_config_key() else {
            return events;
        };
        if self.config.gaming_key_mode(&key) != mode {
            self.config
                .edit(|config| config.set_gaming_key_mode(&key, mode));
            self.persist_and_reload("G-key mode");
        }
        events
    }

    /// The name the user gave the selected keyboard's G-key `profile`.
    #[must_use]
    pub(crate) fn current_g_key_profile_name(&self, profile: GKeyProfile) -> Option<&str> {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)?;
        self.config.g_key_profile_name(key, profile)
    }

    /// Name the selected keyboard's G-key `profile` and persist it; a blank
    /// name clears it. The name only labels the profile here, so the agent
    /// is not reloaded.
    pub(crate) fn commit_g_key_profile_name(
        &mut self,
        profile: GKeyProfile,
        name: &str,
    ) -> StateEvents {
        let events = self.for_current_device(StateEvent::BindingsChanged);
        let Some(key) = self.gaming_config_key() else {
            return events;
        };
        let name = name.trim();
        if self
            .config
            .g_key_profile_name(&key, profile)
            .unwrap_or_default()
            != name
        {
            self.config
                .edit(|config| config.set_g_key_profile_name(&key, profile, name));
            self.persist_config("G-key profile name");
        }
        events
    }

    /// The config key of the selected device when it has G-keys to set.
    fn gaming_config_key(&self) -> Option<String> {
        self.current_record()
            .filter(|record| record.capabilities.is_some_and(|c| c.g_keys))
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_owned)
    }
}
