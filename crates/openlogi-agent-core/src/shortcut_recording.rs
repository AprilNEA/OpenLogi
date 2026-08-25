//! Agent-owned physical shortcut recording.
//!
//! The settings process is an IPC client and never installs an input hook. A
//! recording session therefore starts a temporary, observe-only [`inputs`]
//! keyboard listener in the agent, feeds its events through
//! [`controls::ShortcutRecorder`], and publishes only OpenLogi's portable
//! [`KeyCombo`] result. The existing mouse hook remains untouched.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use controls::{PhysicalInput, RecordedShortcut, RecorderState, ShortcutRecorder};
use inputs::{
    Event, EventDisposition, EventKinds, EventSink, GrabMode, KeyCode, Listener, Modifiers,
    PhysicalKey,
};
use openlogi_core::binding::{KeyCombo, KeyboardUsage};
use openlogi_ipc::{ShortcutRecording, ShortcutRecordingPhase};
use tracing::warn;

use crate::observable::ObservableState;

/// Bound on recorder events waiting off the OS callback thread. Overflow drops
/// an observation rather than delaying system input; the user can simply retry.
const EVENT_QUEUE_CAPACITY: usize = 128;
/// A recorder is an explicit, short-lived UI operation. Expiring abandoned
/// sessions ensures a crashed or closed GUI cannot leave the resident agent
/// reading keyboard event nodes indefinitely.
const RECORDING_TIMEOUT: Duration = Duration::from_secs(60);

enum Message {
    Start(u64),
    Cancel(u64),
    Event(Event),
    ListenerFailed(u64),
    #[cfg(test)]
    Barrier(std::sync::mpsc::Sender<()>),
}

/// Observe-only sink: clone into a bounded queue and always pass the event.
struct RecordingSink {
    tx: SyncSender<Message>,
}

impl EventSink for RecordingSink {
    fn on_event(&self, event: &Event) -> EventDisposition {
        let _ = self.tx.try_send(Message::Event(event.clone()));
        EventDisposition::Pass
    }
}

/// Owns the temporary listener and the off-callback recorder worker.
pub struct ShortcutRecordingManager {
    tx: SyncSender<Message>,
    listener: Mutex<Option<Listener>>,
    next_session: AtomicU64,
    current_session: AtomicU64,
    observable: Arc<ObservableState>,
}

impl ShortcutRecordingManager {
    /// Create the manager and its recorder worker.
    #[must_use]
    pub fn new(observable: Arc<ObservableState>) -> Arc<Self> {
        let (tx, rx) = sync_channel(EVENT_QUEUE_CAPACITY);
        let manager = Arc::new(Self {
            tx,
            listener: Mutex::new(None),
            next_session: AtomicU64::new(1),
            current_session: AtomicU64::new(0),
            observable: Arc::clone(&observable),
        });
        let weak = Arc::downgrade(&manager);
        if let Err(error) = thread::Builder::new()
            .name("openlogi-shortcut-recorder".into())
            .spawn(move || run_recorder(&rx, &observable, &weak, RECORDING_TIMEOUT))
        {
            warn!(%error, "could not start shortcut recorder worker");
        }
        manager
    }

    /// Start a fresh recording, replacing any previous session.
    ///
    /// Returns its monotonic session id. Listener setup failures are published
    /// as [`ShortcutRecordingPhase::Unavailable`] for this same id.
    pub fn start(self: &Arc<Self>) -> u64 {
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);
        self.current_session.store(session_id, Ordering::Release);
        // Publish before returning the RPC acknowledgement. The GUI can then
        // discard its pre-command long poll and know the first fresh snapshot
        // already contains this session rather than stale state from the one
        // it replaced.
        self.observable
            .set_shortcut_recording(Some(ShortcutRecording {
                session_id,
                phase: ShortcutRecordingPhase::Recording,
            }));
        if self.tx.send(Message::Start(session_id)).is_err() {
            self.current_session.store(0, Ordering::Release);
            self.publish_unavailable(session_id);
            return session_id;
        }

        let mut listener = match self.listener.lock() {
            Ok(listener) => listener,
            Err(poisoned) => poisoned.into_inner(),
        };
        if listener
            .as_ref()
            .is_some_and(|listener| !listener.is_healthy())
        {
            let stale = listener.take();
            drop(stale);
        }

        if listener.is_none() {
            if !keyboard_capture_available() {
                warn!("no readable physical keyboard is available for shortcut recording");
                if self.tx.send(Message::ListenerFailed(session_id)).is_err() {
                    self.current_session.store(0, Ordering::Release);
                    self.publish_unavailable(session_id);
                }
                return session_id;
            }
            let sink: Arc<dyn EventSink> = Arc::new(RecordingSink {
                tx: self.tx.clone(),
            });
            match Listener::builder()
                .event_kinds(EventKinds::KEY)
                .grab_mode(GrabMode::Never)
                .start_arc(sink)
            {
                Ok(started) => *listener = Some(started),
                Err(error) => {
                    warn!(%error, "could not start physical shortcut recording");
                    if self.tx.send(Message::ListenerFailed(session_id)).is_err() {
                        self.current_session.store(0, Ordering::Release);
                        self.publish_unavailable(session_id);
                    }
                }
            }
        }

        session_id
    }

    /// Cancel the current recording, if any, or dismiss its terminal result.
    pub fn cancel(&self) {
        let session_id = self.current_session.swap(0, Ordering::AcqRel);
        // As with start, make the command's observable result true before its
        // RPC returns. Worker cleanup follows on the bounded queue.
        self.observable.set_shortcut_recording(None);
        if session_id != 0 && self.tx.send(Message::Cancel(session_id)).is_err() {
            self.finish(session_id);
        }
    }

    fn publish_unavailable(&self, session_id: u64) {
        self.observable
            .set_shortcut_recording(Some(ShortcutRecording {
                session_id,
                phase: ShortcutRecordingPhase::Unavailable,
            }));
    }

    fn finish(&self, session_id: u64) {
        let _ = self.current_session.compare_exchange(
            session_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let listener = match self.listener.lock() {
            Ok(mut listener) => {
                if self.current_session.load(Ordering::Acquire) != 0 {
                    return;
                }
                listener.take()
            }
            Err(poisoned) => {
                let mut listener = poisoned.into_inner();
                if self.current_session.load(Ordering::Acquire) != 0 {
                    return;
                }
                listener.take()
            }
        };
        drop(listener);
    }
}

fn keyboard_capture_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        inputs::devices()
            .into_iter()
            .any(|device| device.has_keyboard_keys)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the bounded worker loop keeps every recorder transition in one auditable state machine"
)]
fn run_recorder(
    rx: &Receiver<Message>,
    observable: &ObservableState,
    manager: &Weak<ShortcutRecordingManager>,
    recording_timeout: Duration,
) {
    let mut recorder = ShortcutRecorder::new();
    let mut active = None;
    let mut published = None;
    let mut deadline: Option<Instant> = None;

    loop {
        let message = match deadline {
            Some(active_deadline) => {
                match rx.recv_timeout(active_deadline.saturating_duration_since(Instant::now())) {
                    Ok(message) => message,
                    Err(RecvTimeoutError::Timeout) => {
                        let Some(session_id) = active.take() else {
                            deadline = None;
                            continue;
                        };
                        recorder.cancel();
                        if let Some(manager) = manager.upgrade() {
                            if manager.current_session.load(Ordering::Acquire) == session_id {
                                publish(
                                    observable,
                                    &mut published,
                                    Some(ShortcutRecording {
                                        session_id,
                                        phase: ShortcutRecordingPhase::Interrupted,
                                    }),
                                );
                            }
                            manager.finish(session_id);
                        }
                        deadline = None;
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match rx.recv() {
                Ok(message) => message,
                Err(_) => break,
            },
        };
        match message {
            Message::Start(session_id) => {
                recorder.reset();
                active = Some(session_id);
                deadline = Some(Instant::now() + recording_timeout);
                publish(
                    observable,
                    &mut published,
                    Some(ShortcutRecording {
                        session_id,
                        phase: ShortcutRecordingPhase::Recording,
                    }),
                );
            }
            Message::Cancel(session_id) if active == Some(session_id) => {
                recorder.cancel();
                active = None;
                deadline = None;
                publish(observable, &mut published, None);
                if let Some(manager) = manager.upgrade() {
                    manager.finish(session_id);
                }
            }
            Message::ListenerFailed(session_id) if active == Some(session_id) => {
                active = None;
                deadline = None;
                publish(
                    observable,
                    &mut published,
                    Some(ShortcutRecording {
                        session_id,
                        phase: ShortcutRecordingPhase::Unavailable,
                    }),
                );
                if let Some(manager) = manager.upgrade() {
                    manager.finish(session_id);
                }
            }
            Message::Event(event) => {
                let Some(session_id) = active else {
                    continue;
                };
                // Cancel/start changes this atomic before enqueueing its worker
                // command. Ignore older callback events already ahead of that
                // command in the shared bounded queue, or a superseded chord
                // could complete against the replacement UI target.
                let Some(manager) = manager.upgrade() else {
                    continue;
                };
                if manager.current_session.load(Ordering::Acquire) != session_id {
                    continue;
                }
                let phase = match recorder.process(&event) {
                    RecorderState::Recording => ShortcutRecordingPhase::Recording,
                    RecorderState::WaitingForRelease => ShortcutRecordingPhase::WaitingForRelease,
                    RecorderState::Complete(shortcut) => {
                        if let Some(combo) = recorded_combo(shortcut) {
                            active = None;
                            ShortcutRecordingPhase::Complete(combo)
                        } else {
                            recorder.reset();
                            ShortcutRecordingPhase::UnsupportedKey
                        }
                    }
                    RecorderState::Cancelled => {
                        active = None;
                        ShortcutRecordingPhase::Interrupted
                    }
                };
                let terminal = matches!(
                    phase,
                    ShortcutRecordingPhase::Complete(_) | ShortcutRecordingPhase::Interrupted
                );
                publish(
                    observable,
                    &mut published,
                    Some(ShortcutRecording { session_id, phase }),
                );
                if terminal {
                    deadline = None;
                    manager.finish(session_id);
                }
            }
            Message::Cancel(_) | Message::ListenerFailed(_) => {}
            #[cfg(test)]
            Message::Barrier(done) => {
                let _ = done.send(());
            }
        }
    }
}

fn publish(
    observable: &ObservableState,
    published: &mut Option<ShortcutRecording>,
    recording: Option<ShortcutRecording>,
) {
    if *published == recording {
        return;
    }
    observable.set_shortcut_recording(recording.clone());
    *published = recording;
}

fn recorded_combo(shortcut: &RecordedShortcut) -> Option<KeyCombo> {
    let PhysicalInput::Key(key) = shortcut.chord().trigger() else {
        return None;
    };
    let usage = KeyboardUsage::try_from(keyboard_usage(key)?).ok()?;
    let modifiers = shortcut.modifiers();
    Some(combo_with_modifiers(usage, modifiers))
}

fn combo_with_modifiers(usage: KeyboardUsage, modifiers: Modifiers) -> KeyCombo {
    KeyCombo::new(usage)
        .with_command(modifiers.meta)
        .with_control(modifiers.control)
        .with_option(modifiers.alt)
        .with_shift(modifiers.shift)
}

#[expect(
    clippy::too_many_lines,
    reason = "the exhaustive physical-key to USB-HID table is clearer as one auditable match"
)]
fn keyboard_usage(key: PhysicalKey) -> Option<u8> {
    let PhysicalKey::Code(code) = key else {
        return None;
    };
    Some(match code {
        KeyCode::KeyA => 0x04,
        KeyCode::KeyB => 0x05,
        KeyCode::KeyC => 0x06,
        KeyCode::KeyD => 0x07,
        KeyCode::KeyE => 0x08,
        KeyCode::KeyF => 0x09,
        KeyCode::KeyG => 0x0a,
        KeyCode::KeyH => 0x0b,
        KeyCode::KeyI => 0x0c,
        KeyCode::KeyJ => 0x0d,
        KeyCode::KeyK => 0x0e,
        KeyCode::KeyL => 0x0f,
        KeyCode::KeyM => 0x10,
        KeyCode::KeyN => 0x11,
        KeyCode::KeyO => 0x12,
        KeyCode::KeyP => 0x13,
        KeyCode::KeyQ => 0x14,
        KeyCode::KeyR => 0x15,
        KeyCode::KeyS => 0x16,
        KeyCode::KeyT => 0x17,
        KeyCode::KeyU => 0x18,
        KeyCode::KeyV => 0x19,
        KeyCode::KeyW => 0x1a,
        KeyCode::KeyX => 0x1b,
        KeyCode::KeyY => 0x1c,
        KeyCode::KeyZ => 0x1d,
        KeyCode::Digit1 => 0x1e,
        KeyCode::Digit2 => 0x1f,
        KeyCode::Digit3 => 0x20,
        KeyCode::Digit4 => 0x21,
        KeyCode::Digit5 => 0x22,
        KeyCode::Digit6 => 0x23,
        KeyCode::Digit7 => 0x24,
        KeyCode::Digit8 => 0x25,
        KeyCode::Digit9 => 0x26,
        KeyCode::Digit0 => 0x27,
        KeyCode::Enter => 0x28,
        KeyCode::Escape => 0x29,
        KeyCode::Backspace => 0x2a,
        KeyCode::Tab => 0x2b,
        KeyCode::Space => 0x2c,
        KeyCode::Minus => 0x2d,
        KeyCode::Equal => 0x2e,
        KeyCode::BracketLeft => 0x2f,
        KeyCode::BracketRight => 0x30,
        KeyCode::Backslash => 0x31,
        KeyCode::Semicolon => 0x33,
        KeyCode::Quote => 0x34,
        KeyCode::Backquote => 0x35,
        KeyCode::Comma => 0x36,
        KeyCode::Period => 0x37,
        KeyCode::Slash => 0x38,
        KeyCode::F1 => 0x3a,
        KeyCode::F2 => 0x3b,
        KeyCode::F3 => 0x3c,
        KeyCode::F4 => 0x3d,
        KeyCode::F5 => 0x3e,
        KeyCode::F6 => 0x3f,
        KeyCode::F7 => 0x40,
        KeyCode::F8 => 0x41,
        KeyCode::F9 => 0x42,
        KeyCode::F10 => 0x43,
        KeyCode::F11 => 0x44,
        KeyCode::F12 => 0x45,
        KeyCode::Home => 0x4a,
        KeyCode::PageUp => 0x4b,
        KeyCode::Delete => 0x4c,
        KeyCode::End => 0x4d,
        KeyCode::PageDown => 0x4e,
        KeyCode::ArrowRight => 0x4f,
        KeyCode::ArrowLeft => 0x50,
        KeyCode::ArrowDown => 0x51,
        KeyCode::ArrowUp => 0x52,
        KeyCode::NumLock => 0x53,
        KeyCode::NumpadDivide => 0x54,
        KeyCode::NumpadMultiply => 0x55,
        KeyCode::NumpadSubtract => 0x56,
        KeyCode::NumpadAdd => 0x57,
        KeyCode::NumpadEnter => 0x58,
        KeyCode::Numpad1 => 0x59,
        KeyCode::Numpad2 => 0x5a,
        KeyCode::Numpad3 => 0x5b,
        KeyCode::Numpad4 => 0x5c,
        KeyCode::Numpad5 => 0x5d,
        KeyCode::Numpad6 => 0x5e,
        KeyCode::Numpad7 => 0x5f,
        KeyCode::Numpad8 => 0x60,
        KeyCode::Numpad9 => 0x61,
        KeyCode::Numpad0 => 0x62,
        KeyCode::NumpadDecimal => 0x63,
        KeyCode::NumpadEqual => 0x67,
        KeyCode::F13 => 0x68,
        KeyCode::F14 => 0x69,
        KeyCode::F15 => 0x6a,
        KeyCode::F16 => 0x6b,
        KeyCode::F17 => 0x6c,
        KeyCode::F18 => 0x6d,
        KeyCode::F19 => 0x6e,
        KeyCode::F20 => 0x6f,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use inputs::{EventKind, KeyEvent, PressState};

    fn key_event(key: KeyCode, modifiers: Modifiers) -> Event {
        Event::new(
            EventKind::Key(KeyEvent {
                physical: PhysicalKey::Code(key),
                logical: None,
                state: PressState::Pressed,
                repeat: false,
                modifiers,
            }),
            None,
            false,
        )
    }

    fn barrier(tx: &SyncSender<Message>) {
        let (done, wait) = std::sync::mpsc::channel();
        tx.send(Message::Barrier(done))
            .expect("recorder worker is live");
        wait.recv().expect("recorder worker reached barrier");
    }

    #[test]
    fn controls_recording_becomes_openlogis_portable_combo() {
        let modifiers = Modifiers {
            meta: true,
            shift: true,
            ..Modifiers::default()
        };
        let mut recorder = ShortcutRecorder::new();
        recorder.process(&key_event(KeyCode::MetaLeft, modifiers));
        recorder.process(&key_event(KeyCode::ShiftLeft, modifiers));
        recorder.process(&key_event(KeyCode::KeyP, modifiers));

        let combo = recorded_combo(
            recorder
                .shortcut()
                .expect("ordinary key should complete recording"),
        )
        .expect("P has a portable USB-HID usage");

        assert_eq!(combo.rendered_label(), "Cmd+Shift+P");
        assert_eq!(combo.key().code(), 0x13);
    }

    #[test]
    fn unsupported_physical_keys_are_rejected_instead_of_guessed() {
        let mut recorder = ShortcutRecorder::new();
        recorder.process(&key_event(KeyCode::NumpadComma, Modifiers::default()));

        assert_eq!(
            recorded_combo(recorder.shortcut().expect("key should complete recording")),
            None
        );
    }

    #[test]
    fn keypad_enter_keeps_a_distinct_injectable_usage() {
        let mut recorder = ShortcutRecorder::new();
        recorder.process(&key_event(KeyCode::NumpadEnter, Modifiers::default()));

        let combo = recorded_combo(
            recorder
                .shortcut()
                .expect("keypad Enter should complete recording"),
        )
        .expect("keypad Enter has a portable USB-HID usage");
        assert_eq!(combo.key().code(), 0x58);
        assert_eq!(combo.rendered_label(), "NumpadEnter");
    }

    #[test]
    fn queued_events_cannot_complete_a_superseded_session() {
        let observable = Arc::new(ObservableState::new("test".to_string()));
        let (tx, rx) = sync_channel(EVENT_QUEUE_CAPACITY);
        let manager = Arc::new(ShortcutRecordingManager {
            tx: tx.clone(),
            listener: Mutex::new(None),
            next_session: AtomicU64::new(3),
            current_session: AtomicU64::new(1),
            observable: Arc::clone(&observable),
        });
        let weak = Arc::downgrade(&manager);
        let worker_observable = Arc::clone(&observable);
        let worker = thread::spawn(move || {
            run_recorder(&rx, &worker_observable, &weak, RECORDING_TIMEOUT);
        });

        tx.send(Message::Start(1)).expect("recorder worker is live");
        barrier(&tx);
        assert_eq!(
            observable.snapshot().shortcut_recording,
            Some(ShortcutRecording {
                session_id: 1,
                phase: ShortcutRecordingPhase::Recording,
            })
        );

        // `start()` advances this identity before its Start command can get
        // behind callback events in the shared queue.
        manager.current_session.store(2, Ordering::Release);
        tx.send(Message::Event(key_event(
            KeyCode::KeyP,
            Modifiers::default(),
        )))
        .expect("recorder worker is live");
        barrier(&tx);
        assert_eq!(
            observable.snapshot().shortcut_recording,
            Some(ShortcutRecording {
                session_id: 1,
                phase: ShortcutRecordingPhase::Recording,
            }),
            "the queued old key must not publish a stale completion"
        );

        tx.send(Message::Start(2)).expect("recorder worker is live");
        barrier(&tx);
        assert_eq!(
            observable.snapshot().shortcut_recording,
            Some(ShortcutRecording {
                session_id: 2,
                phase: ShortcutRecordingPhase::Recording,
            })
        );

        manager.current_session.store(0, Ordering::Release);
        tx.send(Message::Cancel(2))
            .expect("recorder worker is live");
        barrier(&tx);
        assert_eq!(observable.snapshot().shortcut_recording, None);
        drop(manager);
        drop(tx);
        worker.join().expect("recorder worker exits cleanly");
    }

    #[test]
    fn abandoned_recording_expires_and_releases_ownership() {
        let observable = Arc::new(ObservableState::new("test".to_string()));
        let (tx, rx) = sync_channel(EVENT_QUEUE_CAPACITY);
        let manager = Arc::new(ShortcutRecordingManager {
            tx: tx.clone(),
            listener: Mutex::new(None),
            next_session: AtomicU64::new(2),
            current_session: AtomicU64::new(1),
            observable: Arc::clone(&observable),
        });
        let weak = Arc::downgrade(&manager);
        let worker_observable = Arc::clone(&observable);
        let worker = thread::spawn(move || {
            run_recorder(&rx, &worker_observable, &weak, Duration::from_millis(10));
        });

        tx.send(Message::Start(1)).expect("recorder worker is live");
        barrier(&tx);
        let wait_until = Instant::now() + Duration::from_secs(1);
        while !matches!(
            observable.snapshot().shortcut_recording,
            Some(ShortcutRecording {
                session_id: 1,
                phase: ShortcutRecordingPhase::Interrupted,
            })
        ) && Instant::now() < wait_until
        {
            thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(
            observable.snapshot().shortcut_recording,
            Some(ShortcutRecording {
                session_id: 1,
                phase: ShortcutRecordingPhase::Interrupted,
            })
        );
        assert_eq!(manager.current_session.load(Ordering::Acquire), 0);
        drop(manager);
        drop(tx);
        worker.join().expect("recorder worker exits cleanly");
    }

    #[test]
    fn portable_key_table_matches_openlogis_supported_usage_boundaries() {
        assert_eq!(keyboard_usage(PhysicalKey::Code(KeyCode::KeyA)), Some(0x04));
        assert_eq!(keyboard_usage(PhysicalKey::Code(KeyCode::F12)), Some(0x45));
        assert_eq!(
            keyboard_usage(PhysicalKey::Code(KeyCode::ArrowUp)),
            Some(0x52)
        );
        assert_eq!(
            keyboard_usage(PhysicalKey::Code(KeyCode::NumpadEnter)),
            Some(0x58)
        );
        assert_eq!(keyboard_usage(PhysicalKey::Code(KeyCode::F20)), Some(0x6f));
        assert_eq!(keyboard_usage(PhysicalKey::Code(KeyCode::F21)), None);
        assert_eq!(
            keyboard_usage(PhysicalKey::Native(inputs::NativeKeyCode::Evdev(500))),
            None
        );
    }
}
