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

impl LiveSource {
    /// The command offered when a key's source is first set to
    /// [`Self::Command`]: one that prints the time as `HH:MM` in the shell a
    /// live key's command runs in (`openlogi_inject::shell_output`) — `sh` on
    /// macOS and Linux, where `date` formats it, and `cmd.exe` on Windows,
    /// whose `date` would instead try to set the date.
    #[must_use]
    pub const fn example_command() -> &'static str {
        if cfg!(windows) { "time /t" } else { "date +%H:%M" }
    }

    /// Whether this platform can read the source. The volume is read on macOS
    /// and Linux only (the agent's live-key reader has no Windows backend), so
    /// a Windows volume key would only ever show its placeholder; every other
    /// source reads everywhere.
    #[must_use]
    pub const fn is_readable_here(&self) -> bool {
        match self {
            Self::Volume => cfg!(any(target_os = "linux", target_os = "macos")),
            Self::Command(_) | Self::Battery(_) | Self::Clock => true,
        }
    }
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

    /// What a press that activates the key runs: [`Self::on_press`], unless
    /// it turns the keypad's page or activates a live key, which would come
    /// straight back to a live key.
    #[must_use]
    pub fn effective_on_press(&self) -> Option<Action> {
        self.on_press.clone().and_then(runnable)
    }

    /// What the left key runs while active: the configured action, else the
    /// volume key for a volume source. An action that turns the page or
    /// activates a live key runs nothing (see [`Self::effective_on_press`]).
    #[must_use]
    pub fn effective_decrement(&self) -> Option<Action> {
        match &self.decrement {
            Some(action) => runnable(action.clone()),
            None => (self.source == LiveSource::Volume).then_some(Action::VolumeDown),
        }
    }

    /// What the right key runs while active; see [`Self::effective_decrement`].
    #[must_use]
    pub fn effective_increment(&self) -> Option<Action> {
        match &self.increment {
            Some(action) => runnable(action.clone()),
            None => (self.source == LiveSource::Volume).then_some(Action::VolumeUp),
        }
    }
}

/// `action`, unless a live key may not run it. A live key's actions run while
/// it holds the page keys, so one that turns the page or activates a live key
/// would come straight back to a live key: a hand-edited
/// `increment = "NextKeypadPage"` would adjust itself forever, and two keys
/// that activate each other would never stop.
fn runnable(action: Action) -> Option<Action> {
    match action {
        Action::ActivateLiveKey(_) | Action::PreviousKeypadPage | Action::NextKeypadPage => None,
        action => Some(action),
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

    /// A live key never runs an action that leads back to a live key, so a
    /// hand-edited config cannot make the agent loop.
    #[test]
    fn a_live_key_never_runs_an_action_that_comes_back_to_a_live_key() {
        use crate::binding::{ButtonId, GridKey};
        let mut key = LiveKey::new(LiveSource::Volume);
        key.on_press = Some(Action::ActivateLiveKey(ButtonId::grid(GridKey::Middle)));
        key.increment = Some(Action::NextKeypadPage);
        key.decrement = Some(Action::PreviousKeypadPage);
        assert_eq!(key.effective_on_press(), None);
        assert_eq!(key.effective_increment(), None);
        assert_eq!(key.effective_decrement(), None);
        key.on_press = Some(Action::MuteVolume);
        assert_eq!(key.effective_on_press(), Some(Action::MuteVolume));
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
