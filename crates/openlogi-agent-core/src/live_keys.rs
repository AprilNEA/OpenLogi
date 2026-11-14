//! Live keypad keys: the values they show, which one is active, and reading
//! their sources.
//!
//! [`LiveKeys`] is the state, decided without I/O: which keys are due for a
//! read, what they show, and what a press or a paging key does. The agent's
//! watcher reads the due sources through a [`LiveReader`] ([`read_source`]
//! off the async threads) and hands each value back as it arrives; the
//! orchestrator draws them into the key faces.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::config::{LiveKey, LiveSource};
use openlogi_core::device::BatteryInfo;
use tokio::sync::{Semaphore, mpsc};
use tracing::debug;

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

    /// What a key shows when its source could not be read.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::text("—")
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

/// Which read of a key a value answers. Issued by [`LiveKeys::due`]; a value
/// is taken in only while its ticket is the key's read in flight, so a read
/// of a source the key no longer shows can never overwrite the new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LiveReadTicket(u64);

/// One source to read now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveRead {
    /// The key it is for.
    pub id: LiveKeyId,
    /// Which read this is: hand it back with the value.
    pub ticket: LiveReadTicket,
    /// What to read.
    pub source: LiveSource,
    /// The battery a [`LiveSource::Battery`] names, as the agent knows it.
    pub battery: Option<BatteryInfo>,
}

impl LiveRead {
    /// Whether reading the source is instant: no command runs, so it never
    /// waits behind one.
    #[must_use]
    pub fn is_instant(&self) -> bool {
        matches!(self.source, LiveSource::Clock | LiveSource::Battery(_))
    }
}

/// A finished read: the value `ticket`'s read of `id` produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveReadDone {
    /// The key it is for.
    pub id: LiveKeyId,
    /// The read it answers.
    pub ticket: LiveReadTicket,
    /// What the source read.
    pub value: LiveValue,
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

/// One configured key's source and what is known of it.
#[derive(Debug)]
struct Tracked {
    /// The source the value and the read in flight belong to. A different
    /// configured source starts the key over.
    source: LiveSource,
    /// The last value read from `source`.
    value: Option<LiveValue>,
    /// When the last read of `source` was issued; `None`: due now.
    read_at: Option<Instant>,
    /// The read of `source` not answered yet. A key has at most one, so its
    /// values arrive in the order they were read.
    in_flight: Option<LiveReadTicket>,
}

impl Tracked {
    fn new(source: LiveSource) -> Self {
        Self {
            source,
            value: None,
            read_at: None,
            in_flight: None,
        }
    }
}

/// The live keys' state.
#[derive(Debug, Default)]
pub struct LiveKeys {
    keys: HashMap<LiveKeyId, Tracked>,
    next_ticket: u64,
    active: Option<Active>,
}

impl LiveKeys {
    /// The sources due for a read at `now` among `keys`, each with the
    /// ticket its value must come back with. Forgets the keys no longer
    /// configured, and starts over a key whose source changed: its old value
    /// and any read of the old source in flight no longer count, and the new
    /// source is read at once.
    pub fn due<'a>(
        &mut self,
        keys: impl IntoIterator<Item = (LiveKeyId, &'a LiveKey)>,
        now: Instant,
    ) -> Vec<(LiveKeyId, LiveSource, LiveReadTicket)> {
        let keys: Vec<(LiveKeyId, &LiveKey)> = keys.into_iter().collect();
        self.keys
            .retain(|id, _| keys.iter().any(|(configured, _)| configured == id));
        let mut due = Vec::new();
        for (id, key) in keys {
            let tracked = self
                .keys
                .entry(id.clone())
                .or_insert_with(|| Tracked::new(key.source.clone()));
            if tracked.source != key.source {
                *tracked = Tracked::new(key.source.clone());
            }
            let is_due = tracked.in_flight.is_none()
                && tracked
                    .read_at
                    .is_none_or(|at| now.saturating_duration_since(at) >= key.refresh());
            if !is_due {
                continue;
            }
            let ticket = LiveReadTicket(self.next_ticket);
            self.next_ticket += 1;
            tracked.read_at = Some(now);
            tracked.in_flight = Some(ticket);
            due.push((id, key.source.clone(), ticket));
        }
        due
    }

    /// Take in a finished read. A value whose ticket is not its key's read
    /// in flight — the key was removed, or its source changed since — is
    /// dropped. Answers whether anything shown changed.
    pub fn set_value(&mut self, done: LiveReadDone) -> bool {
        let Some(tracked) = self
            .keys
            .get_mut(&done.id)
            .filter(|tracked| tracked.in_flight == Some(done.ticket))
        else {
            return false;
        };
        tracked.in_flight = None;
        let changed = tracked.value.as_ref() != Some(&done.value);
        tracked.value = Some(done.value);
        changed
    }

    /// End an active key whose time is up. Answers whether it ended.
    pub fn expire(&mut self, now: Instant) -> bool {
        self.active.take_if(|active| active.until <= now).is_some()
    }

    /// A press of the live key `id` at `now`: activate it, or end it when it
    /// is the active one. Answers the action to run (its `on_press`, on
    /// activation only). A key whose time is up is no longer active, so its
    /// press activates it again.
    pub fn press(&mut self, id: LiveKeyId, key: &LiveKey, now: Instant) -> Option<Action> {
        self.expire(now);
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
    /// active, the key adjusts it (and the timeout restarts); otherwise —
    /// also once the active key's time is up — it turns the page.
    pub fn paging(
        &mut self,
        device: &str,
        increment: bool,
        key_of: impl Fn(&ButtonId) -> Option<LiveKey>,
        now: Instant,
    ) -> Paging {
        self.expire(now);
        let Some(active) = self.active.as_mut().filter(|active| active.id.0 == device) else {
            return Paging::TurnPage;
        };
        let Some(key) = key_of(&active.id.1) else {
            self.active = None;
            return Paging::TurnPage;
        };
        active.until = now + key.timeout();
        // A step changes the value: read it again as soon as the key has no
        // read in flight.
        if let Some(tracked) = self.keys.get_mut(&active.id) {
            tracked.read_at = None;
        }
        Paging::Adjust(if increment {
            key.effective_increment()
        } else {
            key.effective_decrement()
        })
    }

    /// What the key `id` shows, when read.
    #[must_use]
    pub fn value(&self, id: &LiveKeyId) -> Option<&LiveValue> {
        self.keys.get(id)?.value.as_ref()
    }

    /// Whether `id` is the active key.
    #[must_use]
    pub fn is_active(&self, id: &LiveKeyId) -> bool {
        self.active.as_ref().is_some_and(|active| active.id == *id)
    }
}

/// How many source reads that run a command may run at once. Instant reads
/// ([`LiveRead::is_instant`]) never wait for one of these.
const MAX_COMMAND_READS: usize = 4;

/// Reads due sources off the async threads and hands back each value as soon
/// as it is read, without waiting for unrelated sources: a command at its
/// time limit delays no other key.
pub struct LiveReader<R> {
    read: Arc<R>,
    commands: Arc<Semaphore>,
    done_tx: mpsc::UnboundedSender<LiveReadDone>,
    done_rx: mpsc::UnboundedReceiver<LiveReadDone>,
}

impl<R> LiveReader<R>
where
    R: Fn(&LiveRead) -> LiveValue + Send + Sync + 'static,
{
    /// A reader that reads each source with `read` (blocking).
    pub fn new(read: R) -> Self {
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        Self {
            read: Arc::new(read),
            commands: Arc::new(Semaphore::new(MAX_COMMAND_READS)),
            done_tx,
            done_rx,
        }
    }

    /// Start every read in `reads`. Each one's value arrives through
    /// [`Self::next`]; a read that panics answers as unavailable, so its key
    /// is never left waiting.
    pub fn start(&self, reads: Vec<LiveRead>) {
        for read in reads {
            let commands = (!read.is_instant()).then(|| Arc::clone(&self.commands));
            let reader = Arc::clone(&self.read);
            let done = self.done_tx.clone();
            tokio::spawn(async move {
                let _permit = match commands {
                    Some(commands) => commands.acquire_owned().await.ok(),
                    None => None,
                };
                let id = read.id.clone();
                let ticket = read.ticket;
                let value = tokio::task::spawn_blocking(move || reader(&read))
                    .await
                    .unwrap_or_else(|_| LiveValue::unavailable());
                // The receiver lives in the reader: a failed send means the
                // reader was dropped, and the watcher with it.
                if done.send(LiveReadDone { id, ticket, value }).is_err() {
                    debug!("live-key reader gone — read value dropped");
                }
            });
        }
    }

    /// The next finished read. Never ends: the reader holds a sender itself.
    pub async fn next(&mut self) -> Option<LiveReadDone> {
        self.done_rx.recv().await
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
            run_shell(command).map_or_else(LiveValue::unavailable, |out| parse_output(&out))
        }
        LiveSource::Volume => volume().map_or_else(LiveValue::unavailable, LiveValue::percent),
        LiveSource::Battery(_) => read
            .battery
            .as_ref()
            .and_then(|battery| battery.percentage)
            .map_or_else(LiveValue::unavailable, LiveValue::percent),
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

    /// The one read `due` issued for `key`, answered with `label`.
    fn answer(due: &[(LiveKeyId, LiveSource, LiveReadTicket)], label: &str) -> LiveReadDone {
        let [(id, _, ticket)] = due else {
            panic!("expected exactly one due read, got {due:?}");
        };
        LiveReadDone {
            id: id.clone(),
            ticket: *ticket,
            value: LiveValue::text(label),
        }
    }

    #[test]
    fn keys_are_read_when_due_and_forgotten_when_removed() {
        let mut keys = LiveKeys::default();
        let clock = LiveKey::new(LiveSource::Clock);
        let t = Instant::now();
        let first = keys.due([(id(GridKey::Middle), &clock)], t);
        assert_eq!(first.len(), 1);
        assert!(keys.set_value(answer(&first, "09:00")));
        assert!(
            keys.due([(id(GridKey::Middle), &clock)], t).is_empty(),
            "not yet"
        );
        let second = keys.due([(id(GridKey::Middle), &clock)], t + clock.refresh());
        assert!(
            !keys.set_value(answer(&second, "09:00")),
            "the same value changes nothing shown"
        );
        keys.due(std::iter::empty(), t);
        assert_eq!(keys.value(&id(GridKey::Middle)), None);
    }

    /// A key is never read twice at once: a slow read that outlasts the
    /// refresh interval is not overtaken by a newer one.
    #[test]
    fn a_key_with_a_read_in_flight_is_not_read_again() {
        let mut keys = LiveKeys::default();
        let clock = LiveKey::new(LiveSource::Clock);
        let t = Instant::now();
        let first = keys.due([(id(GridKey::Middle), &clock)], t);
        assert!(
            keys.due([(id(GridKey::Middle), &clock)], t + clock.refresh() * 3)
                .is_empty(),
            "the first read has not answered"
        );
        keys.set_value(answer(&first, "09:00"));
        assert_eq!(
            keys.due([(id(GridKey::Middle), &clock)], t + clock.refresh() * 3)
                .len(),
            1
        );
    }

    /// A read of the source a key showed before its configuration changed
    /// must not land on the key, and the new source is read at once rather
    /// than after the old source's last read plus the new interval.
    #[test]
    fn live_value_rejects_previous_source() {
        let mut keys = LiveKeys::default();
        let key = id(GridKey::Middle);
        let old = LiveKey::new(LiveSource::Command("echo old".into()));
        let mut new = LiveKey::new(LiveSource::Command("echo new".into()));
        new.refresh_secs = 3600;
        let t = Instant::now();

        let old_read = keys.due([(key.clone(), &old)], t);
        // The command changes while the old read is still running.
        let new_read = keys.due([(key.clone(), &new)], t + Duration::from_secs(1));
        assert_eq!(new_read.len(), 1, "the new source is read at once");
        assert!(
            !keys.set_value(answer(&old_read, "old")),
            "the previous source's value is dropped"
        );
        assert_eq!(keys.value(&key), None);
        assert!(keys.set_value(answer(&new_read, "new")));
        assert_eq!(keys.value(&key), Some(&LiveValue::text("new")));
        // A value read before the change does not come back afterwards
        // either.
        assert!(!keys.set_value(answer(&old_read, "old")));
        assert_eq!(keys.value(&key), Some(&LiveValue::text("new")));
    }

    /// Once its time is up, an active key no longer takes the paging keys —
    /// however late the watcher's own expiry runs — and they turn the page
    /// instead of renewing it.
    #[test]
    fn expired_live_key_does_not_capture_paging() {
        let mut keys = LiveKeys::default();
        let volume = LiveKey::new(LiveSource::Volume);
        let lookup = |_: &ButtonId| Some(LiveKey::new(LiveSource::Volume));
        let t = Instant::now();
        let key = id(GridKey::Middle);
        keys.press(key.clone(), &volume, t);

        let late = t + volume.timeout() + Duration::from_secs(1);
        assert_eq!(keys.paging("pad", true, lookup, late), Paging::TurnPage);
        assert!(!keys.is_active(&key), "the paging key did not renew it");
        assert_eq!(
            keys.press(key.clone(), &volume, late + volume.timeout() * 2),
            None,
            "a press after its time is up activates it again"
        );
        assert!(keys.is_active(&key));
    }

    /// A command at its time limit delays no other key: the clock's value
    /// arrives while every command slot is still busy. The commands only
    /// sleep, so a broken reader fails the deadline instead of hanging.
    #[tokio::test]
    async fn slow_live_source_does_not_delay_clock() {
        const SLOW: Duration = Duration::from_millis(1500);
        let mut reader = LiveReader::new(|read: &LiveRead| match read.source {
            LiveSource::Command(_) => {
                std::thread::sleep(SLOW);
                LiveValue::text("slow")
            }
            _ => LiveValue::text("09:00"),
        });
        let read = |key, source, ticket| LiveRead {
            id: id(key),
            ticket: LiveReadTicket(ticket),
            source,
            battery: None,
        };
        let mut reads: Vec<LiveRead> = GridKey::ALL
            .into_iter()
            .take(MAX_COMMAND_READS + 1)
            .zip(0..)
            .map(|(key, ticket)| read(key, LiveSource::Command("sleep".into()), ticket))
            .collect();
        reads.push(read(
            GridKey::ALL[MAX_COMMAND_READS + 1],
            LiveSource::Clock,
            99,
        ));
        reader.start(reads);

        let first = tokio::time::timeout(SLOW / 3, reader.next())
            .await
            .expect("the clock waited for the commands")
            .expect("the reader never ends");
        assert_eq!(first.ticket, LiveReadTicket(99));
        assert_eq!(first.value, LiveValue::text("09:00"));
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
