//! Live keypad keys: an LCD key that shows a value — a command's output, the
//! volume, a battery, the clock — refreshed on its own, and that can be
//! pressed to act on it.
//!
//! A press makes the key active (it is drawn highlighted) and runs its
//! [`LiveKey::on_press`] action; while it is active, the two keys under the
//! keypad's grid run [`LiveKey::decrement`] and [`LiveKey::increment`]
//! instead of turning the page. A second press, or [`LiveKey::timeout_secs`]
//! without a touch, ends it.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::binding::Action;

/// What a live key shows.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LiveSource {
    /// A shell command's output: its first line, or a JSON object
    /// `{"label": "…", "progress": 0-100}`.
    Command(String),
    /// The output volume, in percent.
    Volume,
    /// The battery of the device with this config key.
    Battery(String),
    /// The local time, `HH:MM`.
    Clock,
}

/// One live key's configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveKey {
    /// What the key shows.
    pub source: LiveSource,
    /// How often the value is read, in seconds (1 to 3600).
    #[serde(default = "default_refresh_secs")]
    pub refresh_secs: u16,
    /// What a press that activates the key runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_press: Option<Action>,
    /// What the left key under the grid runs while the key is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decrement: Option<Action>,
    /// What the right key under the grid runs while the key is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub increment: Option<Action>,
    /// How long the key stays active without a touch, in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u16,
}

const fn default_refresh_secs() -> u16 {
    5
}

const fn default_timeout_secs() -> u16 {
    10
}

impl LiveKey {
    /// A live key showing `source` with every other setting at its default.
    #[must_use]
    pub fn new(source: LiveSource) -> Self {
        Self {
            source,
            refresh_secs: default_refresh_secs(),
            on_press: None,
            decrement: None,
            increment: None,
            timeout_secs: default_timeout_secs(),
        }
    }

    /// How often the value is read, kept to 1 s – 1 h.
    #[must_use]
    pub fn refresh(&self) -> Duration {
        Duration::from_secs(u64::from(self.refresh_secs.clamp(1, 3600)))
    }

    /// How long the key stays active without a touch, kept to 2 s – 5 min.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(u64::from(self.timeout_secs.clamp(2, 300)))
    }

    /// What the left key runs while active: the configured action, else the
    /// volume key for a volume source.
    #[must_use]
    pub fn effective_decrement(&self) -> Option<Action> {
        self.decrement
            .clone()
            .or_else(|| (self.source == LiveSource::Volume).then_some(Action::VolumeDown))
    }

    /// What the right key runs while active; see [`Self::effective_decrement`].
    #[must_use]
    pub fn effective_increment(&self) -> Option<Action> {
        self.increment
            .clone()
            .or_else(|| (self.source == LiveSource::Volume).then_some(Action::VolumeUp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_volume_key_steps_the_volume_unless_told_otherwise() {
        let mut key = LiveKey::new(LiveSource::Volume);
        assert_eq!(key.effective_increment(), Some(Action::VolumeUp));
        assert_eq!(key.effective_decrement(), Some(Action::VolumeDown));
        key.increment = Some(Action::NextTrack);
        assert_eq!(key.effective_increment(), Some(Action::NextTrack));
        let clock = LiveKey::new(LiveSource::Clock);
        assert_eq!(clock.effective_increment(), None);
    }

    #[test]
    fn intervals_stay_in_range_and_toml_round_trips() {
        let mut key = LiveKey::new(LiveSource::Command("date +%H".into()));
        key.refresh_secs = 0;
        key.timeout_secs = 9999;
        assert_eq!(key.refresh(), Duration::from_secs(1));
        assert_eq!(key.timeout(), Duration::from_secs(300));
        let text = toml::to_string(&key).expect("serializes");
        assert_eq!(toml::from_str::<LiveKey>(&text).expect("parses"), key);
        let short: LiveKey = toml::from_str("source = \"Clock\"").expect("defaults");
        assert_eq!(short, LiveKey::new(LiveSource::Clock));
    }
}
