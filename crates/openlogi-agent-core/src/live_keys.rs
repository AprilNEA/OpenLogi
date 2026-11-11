//! Live keypad keys: the values they show, which one is active, and reading
//! their sources.
//!
//! [`LiveKeys`] is the state, decided without I/O: which keys are due for a
//! read, what they show, and what a press or a paging key does. The agent's
//! watcher reads the due sources off the async threads ([`read_source`]) and
//! hands the values back; the orchestrator draws them into the key faces.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::config::{LiveKey, LiveSource};
use openlogi_core::device::BatteryInfo;

/// How long a source command may run before its value is given up.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// The most output a source command's value is read from.
const MAX_OUTPUT: usize = 4096;
/// The most characters of a command's value a key shows, from a plain line
/// or a JSON `label` alike.
const MAX_VALUE_CHARS: usize = 24;

/// What a live key shows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LiveValue {
    /// The text on the key.
    pub label: String,
    /// A bar under the text, in percent.
    pub progress: Option<u8>,
}

impl LiveValue {
    fn text(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            progress: None,
        }
    }

    fn percent(percent: u8) -> Self {
        Self {
            label: format!("{percent}%"),
            progress: Some(percent.min(100)),
        }
    }
}

/// A key: its device's config key and its page-qualified grid key.
pub type LiveKeyId = (String, ButtonId);

/// One source to read now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveRead {
    /// The key it is for.
    pub id: LiveKeyId,
    /// What to read.
    pub source: LiveSource,
    /// The battery a [`LiveSource::Battery`] names, as the agent knows it.
    pub battery: Option<BatteryInfo>,
}

/// The active key and until when.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Active {
    id: LiveKeyId,
    until: Instant,
}

/// What the paging keys do for a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Paging {
    /// No live key of the device is active: turn the page.
    TurnPage,
    /// Run this action of the active key instead (`None`: it has none).
    Adjust(Option<Action>),
}

/// The live keys' state.
#[derive(Debug, Default)]
pub struct LiveKeys {
    values: HashMap<LiveKeyId, LiveValue>,
    read_at: HashMap<LiveKeyId, Instant>,
    active: Option<Active>,
}

impl LiveKeys {
    /// The sources due for a read at `now` among `keys`, and forget the
    /// values of keys no longer configured.
    pub fn due<'a>(
        &mut self,
        keys: impl IntoIterator<Item = (LiveKeyId, &'a LiveKey)>,
        now: Instant,
    ) -> Vec<(LiveKeyId, LiveSource)> {
        let keys: Vec<(LiveKeyId, &LiveKey)> = keys.into_iter().collect();
        self.values
            .retain(|id, _| keys.iter().any(|(configured, _)| configured == id));
        self.read_at
            .retain(|id, _| keys.iter().any(|(configured, _)| configured == id));
        let due: Vec<(LiveKeyId, LiveSource)> = keys
            .into_iter()
            .filter(|(id, key)| {
                self.read_at
                    .get(id)
                    .is_none_or(|at| now.saturating_duration_since(*at) >= key.refresh())
            })
            .map(|(id, key)| (id, key.source.clone()))
            .collect();
        for (id, _) in &due {
            self.read_at.insert(id.clone(), now);
        }
        due
    }

    /// Take in read values. Answers whether anything shown changed.
    pub fn set_values(&mut self, values: impl IntoIterator<Item = (LiveKeyId, LiveValue)>) -> bool {
        let mut changed = false;
        for (id, value) in values {
            changed |= self.values.insert(id, value.clone()).as_ref() != Some(&value);
        }
        changed
    }

    /// End an active key whose time is up. Answers whether it ended.
    pub fn expire(&mut self, now: Instant) -> bool {
        self.active.take_if(|active| active.until <= now).is_some()
    }

    /// A press of the live key `id` at `now`: activate it, or end it when it
    /// is the active one. Answers the action to run (its `on_press`, on
    /// activation only).
    pub fn press(&mut self, id: LiveKeyId, key: &LiveKey, now: Instant) -> Option<Action> {
        if self.active.as_ref().is_some_and(|active| active.id == id) {
            self.active = None;
            return None;
        }
        self.active = Some(Active {
            id,
            until: now + key.timeout(),
        });
        key.effective_on_press()
    }

    /// A paging key of `device` at `now`: while one of its live keys is
    /// active, the key adjusts it (and the timeout restarts); otherwise it
    /// turns the page.
    pub fn paging(
        &mut self,
        device: &str,
        increment: bool,
        key_of: impl Fn(&ButtonId) -> Option<LiveKey>,
        now: Instant,
    ) -> Paging {
        let Some(active) = self.active.as_mut().filter(|active| active.id.0 == device) else {
            return Paging::TurnPage;
        };
        let Some(key) = key_of(&active.id.1) else {
            self.active = None;
            return Paging::TurnPage;
        };
        active.until = now + key.timeout();
        // A step changes the value: read it again at once.
        let id = active.id.clone();
        self.read_at.remove(&id);
        Paging::Adjust(if increment {
            key.effective_increment()
        } else {
            key.effective_decrement()
        })
    }

    /// What the key `id` shows, when read.
    #[must_use]
    pub fn value(&self, id: &LiveKeyId) -> Option<&LiveValue> {
        self.values.get(id)
    }

    /// Whether `id` is the active key.
    #[must_use]
    pub fn is_active(&self, id: &LiveKeyId) -> bool {
        self.active.as_ref().is_some_and(|active| active.id == *id)
    }
}

/// A source command's output as a value: a JSON object
/// `{"label": …, "progress": …}`, else its first non-empty line.
#[must_use]
pub fn parse_output(output: &str) -> LiveValue {
    let trimmed = output.trim();
    if trimmed.starts_with('{')
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        let label = json
            .get("label")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(MAX_VALUE_CHARS)
            .collect();
        let progress = json
            .get("progress")
            .and_then(serde_json::Value::as_u64)
            .map(|progress| u8::try_from(progress.min(100)).unwrap_or(100));
        return LiveValue { label, progress };
    }
    LiveValue::text(
        trimmed
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .chars()
            .take(MAX_VALUE_CHARS)
            .collect::<String>(),
    )
}

/// Read one source. Blocking: run it off the async threads.
#[must_use]
pub fn read_source(read: &LiveRead) -> LiveValue {
    match &read.source {
        LiveSource::Command(command) => {
            run_shell(command).map_or_else(|| LiveValue::text("—"), |out| parse_output(&out))
        }
        LiveSource::Volume => volume().map_or_else(|| LiveValue::text("—"), LiveValue::percent),
        LiveSource::Battery(_) => read
            .battery
            .as_ref()
            .and_then(|battery| battery.percentage)
            .map_or_else(|| LiveValue::text("—"), LiveValue::percent),
        LiveSource::Clock => {
            let now = chrono::Local::now();
            LiveValue::text(now.format("%H:%M").to_string())
        }
    }
}

/// Run `command` through the platform shell, bounded by [`COMMAND_TIMEOUT`],
/// and answer its standard output. The shell is the one a bound
/// [`Action::RunShellCommand`] runs in ([`openlogi_inject::shell_output`]).
fn run_shell(command: &str) -> Option<String> {
    openlogi_inject::shell_output(command, COMMAND_TIMEOUT, MAX_OUTPUT)
}

/// The output volume in percent, from the platform's own tool.
fn volume() -> Option<u8> {
    #[cfg(target_os = "linux")]
    {
        // `Volume: 0.45` or `Volume: 0.45 [MUTED]`.
        let out = run_shell("wpctl get-volume @DEFAULT_AUDIO_SINK@")?;
        let level: f64 = out.split_whitespace().nth(1)?.parse().ok()?;
        if out.contains("MUTED") {
            return Some(0);
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a level clamped to 0–100 before the cast"
        )]
        let percent = (level * 100.0).round().clamp(0.0, 100.0) as u8;
        Some(percent)
    }
    #[cfg(target_os = "macos")]
    {
        run_shell("/usr/bin/osascript -e 'output volume of (get volume settings)'")?
            .trim()
            .parse()
            .ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::{GridKey, GridSlot, KeypadPage};

    use super::*;

    fn id(key: GridKey) -> LiveKeyId {
        (
            "pad".into(),
            ButtonId::Grid(GridSlot {
                page: KeypadPage::FIRST,
                key,
            }),
        )
    }

    #[test]
    fn output_is_a_first_line_or_a_json_object() {
        assert_eq!(parse_output("\n  42 °C \nmore"), LiveValue::text("42 °C"));
        assert_eq!(
            parse_output(r#"{"label":"CPU","progress":130}"#),
            LiveValue {
                label: "CPU".into(),
                progress: Some(100)
            }
        );
        assert_eq!(parse_output(""), LiveValue::text(""));
        let long = format!(r#"{{"label":"{}"}}"#, "x".repeat(4000));
        assert_eq!(parse_output(&long).label.chars().count(), MAX_VALUE_CHARS);
    }

    #[test]
    fn keys_are_read_when_due_and_forgotten_when_removed() {
        let mut keys = LiveKeys::default();
        let clock = LiveKey::new(LiveSource::Clock);
        let t = Instant::now();
        assert_eq!(keys.due([(id(GridKey::Middle), &clock)], t).len(), 1);
        assert!(
            keys.due([(id(GridKey::Middle), &clock)], t).is_empty(),
            "not yet"
        );
        assert_eq!(
            keys.due([(id(GridKey::Middle), &clock)], t + clock.refresh())
                .len(),
            1
        );
        assert!(keys.set_values([(id(GridKey::Middle), LiveValue::text("09:00"))]));
        assert!(!keys.set_values([(id(GridKey::Middle), LiveValue::text("09:00"))]));
        keys.due(std::iter::empty(), t);
        assert_eq!(keys.value(&id(GridKey::Middle)), None);
    }

    #[test]
    fn a_press_activates_paging_adjusts_and_time_or_a_second_press_ends() {
        let mut keys = LiveKeys::default();
        let mut volume = LiveKey::new(LiveSource::Volume);
        volume.on_press = Some(Action::MuteVolume);
        let t = Instant::now();
        let key = id(GridKey::Middle);
        let lookup = |_: &ButtonId| Some(LiveKey::new(LiveSource::Volume));

        assert_eq!(keys.paging("pad", true, lookup, t), Paging::TurnPage);
        assert_eq!(
            keys.press(key.clone(), &volume, t),
            Some(Action::MuteVolume)
        );
        assert!(keys.is_active(&key));
        assert_eq!(
            keys.paging("pad", true, lookup, t),
            Paging::Adjust(Some(Action::VolumeUp))
        );
        assert_eq!(
            keys.paging("other", false, lookup, t),
            Paging::TurnPage,
            "another device turns its page"
        );
        assert!(!keys.expire(t + Duration::from_secs(1)));
        assert!(keys.expire(t + volume.timeout() + Duration::from_secs(1)));
        assert_eq!(keys.paging("pad", true, lookup, t), Paging::TurnPage);

        keys.press(key.clone(), &volume, t);
        assert_eq!(
            keys.press(key.clone(), &volume, t),
            None,
            "second press ends it"
        );
        assert!(!keys.is_active(&key));
    }
}
