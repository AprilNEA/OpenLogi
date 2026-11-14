//! Display-key brightness: a release writes the keypad through the agent,
//! the newest write's read-back is shown and saved when it is the level
//! asked for, and a write the keypad did not take is shown as refused.

use super::fn_lock::drain;
use super::pointer_speed::install;
use super::*;
use crate::services::ipc::{Command, ReadKeyBrightness, SetKeyBrightness};
use crate::state::KeyBrightnessLoad;
use openlogi_core::config::KeyDisplayBrightness;

fn level(percent: u8) -> KeyDisplayBrightness {
    KeyDisplayBrightness::try_new(percent).expect("a valid brightness")
}

/// Answer the brightness read the state has queued, returning the route it
/// was asked on. Every other command is dropped.
fn answer_read(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    reading: KeyDisplayBrightness,
) -> DeviceRoute {
    let mut answered = None;
    for command in drain(receiver) {
        if let Command::ReadKeyBrightness(ReadKeyBrightness { route, reply }) = command {
            assert!(answered.is_none(), "one brightness read at a time");
            let _ = reply.send(Ok(reading));
            answered = Some(route);
        }
    }
    answered.expect("the state asked the keypad for its brightness")
}

/// The brightness writes among `commands`, as `(write, level)`.
fn writes(commands: &[Command]) -> Vec<(u64, KeyDisplayBrightness)> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::SetKeyBrightness(SetKeyBrightness {
                write, brightness, ..
            }) => Some((*write, *brightness)),
            _ => None,
        })
        .collect()
}

fn reloads(commands: &[Command]) -> usize {
    commands
        .iter()
        .filter(|command| matches!(command, Command::ReloadConfig(_)))
        .count()
}

/// Release the slider at `brightness` and return the number of the write.
fn release(
    cx: &mut gpui::TestAppContext,
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    brightness: KeyDisplayBrightness,
) -> u64 {
    let _ = with_state(cx, |state| state.commit_key_brightness(brightness));
    let [(write, written)] = writes(&drain(receiver))[..] else {
        panic!("expected one brightness write");
    };
    assert_eq!(written, brightness);
    write
}

/// Install the keypad, whose brightness read answers `reading`.
fn installed(
    reading: KeyDisplayBrightness,
    cx: &mut gpui::TestAppContext,
) -> (
    DeviceKey,
    String,
    tokio::sync::mpsc::UnboundedReceiver<Command>,
) {
    let (state, mut receiver) = super::keypad_pages::keypad_state();
    let record = state.current_record().expect("the keypad");
    let (key, config_key) = (record.device_key(), record.config_key.clone());
    install(state, cx);
    answer_read(&mut receiver, reading);
    cx.run_until_parked();
    (key, config_key, receiver)
}

fn with_state<R>(cx: &mut gpui::TestAppContext, f: impl FnOnce(&mut AppState) -> R) -> R {
    cx.update(|cx| AppState::update(cx, |state, _| f(state)))
}

fn saved(cx: &mut gpui::TestAppContext, config_key: &str) -> Option<KeyDisplayBrightness> {
    with_state(cx, |state| state.device_key_display_brightness(config_key))
}

#[gpui::test]
fn a_release_writes_the_keypad_and_saves_the_level_it_reads_back(cx: &mut gpui::TestAppContext) {
    let (key, config_key, mut receiver) = installed(level(100), cx);
    assert_eq!(
        with_state(cx, |state| state.current_key_brightness()),
        Some(level(100)),
        "the slider starts on the keypad's own level"
    );

    let events = with_state(cx, |state| state.commit_key_brightness(level(30)));
    assert_eq!(events, [StateEvent::KeyBrightnessChanged(key.clone())]);
    let commands = drain(&mut receiver);
    let [(write, written)] = writes(&commands)[..] else {
        panic!("expected one brightness write");
    };
    assert_eq!(written, level(30));
    assert_eq!(
        reloads(&commands),
        0,
        "nothing is saved before the keypad answers"
    );
    assert_eq!(
        with_state(cx, |state| state.current_key_brightness()),
        Some(level(30)),
        "the slider stays where it was released"
    );

    let _ = with_state(cx, |state| {
        state.apply_key_brightness_written(&key, write, level(30), Ok(level(30)))
    });
    assert_eq!(saved(cx, &config_key), Some(level(30)));
    assert_eq!(
        reloads(&drain(&mut receiver)),
        1,
        "the agent reloads the save"
    );
}

/// A write the keypad did not take — refused, timed out, or the agent gone
/// — reaches the settings: the keypad is marked refused, read again so the
/// slider returns to its own level, and nothing is saved. The next write
/// that lands clears the mark.
#[gpui::test]
fn key_brightness_failure_reaches_settings(cx: &mut gpui::TestAppContext) {
    let (key, config_key, mut receiver) = installed(level(100), cx);
    let write = release(cx, &mut receiver, level(30));

    let events = with_state(cx, |state| {
        state.apply_key_brightness_written(
            &key,
            write,
            level(30),
            Err(WriteError::RequestTimedOut {
                operation: openlogi_core::hid::HidppOperation::WriteKeyBrightness,
            }),
        )
    });
    cx.run_until_parked();

    assert_eq!(events, [StateEvent::KeyBrightnessChanged(key.clone())]);
    assert!(with_state(cx, |state| state.current_key_brightness_refused()));
    assert_eq!(saved(cx, &config_key), None);
    answer_read(&mut receiver, level(100));
    cx.run_until_parked();
    assert_eq!(
        with_state(cx, |state| state.current_key_brightness_load()),
        KeyBrightnessLoad::Ready(Arc::new(level(100))),
        "the slider returns to the keypad's own level"
    );

    let write = release(cx, &mut receiver, level(40));
    let _ = with_state(cx, |state| {
        state.apply_key_brightness_written(&key, write, level(40), Ok(level(40)))
    });
    assert!(!with_state(cx, |state| state.current_key_brightness_refused()));
    assert_eq!(saved(cx, &config_key), Some(level(40)));
}

/// Only the newest write's answer counts, and a write the agent answered
/// with a plain read — a newer write superseded it — saves nothing.
#[gpui::test]
fn only_the_newest_write_is_shown_or_saved(cx: &mut gpui::TestAppContext) {
    let (key, config_key, mut receiver) = installed(level(100), cx);
    let older = release(cx, &mut receiver, level(30));
    let newer = release(cx, &mut receiver, level(60));

    let events = with_state(cx, |state| {
        state.apply_key_brightness_written(&key, older, level(30), Ok(level(30)))
    });
    assert!(events.is_empty(), "the older answer changes nothing");
    assert_eq!(
        with_state(cx, |state| state.current_key_brightness()),
        Some(level(60))
    );
    assert_eq!(saved(cx, &config_key), None);

    let _ = with_state(cx, |state| {
        state.apply_key_brightness_written(&key, newer, level(60), Ok(level(55)))
    });
    assert_eq!(
        with_state(cx, |state| state.current_key_brightness()),
        Some(level(55)),
        "the keypad's own read-back is shown"
    );
    assert_eq!(
        saved(cx, &config_key),
        None,
        "a level the keypad did not take is not saved"
    );
}

/// A keypad that has not reported `0x8040` is neither read nor written.
#[test]
fn an_unmeasured_keypad_is_neither_read_nor_written() {
    let (mut state, mut receiver) = super::keypad_pages::unmeasured_keypad_state();
    drain(&mut receiver);
    assert!(!state.current_key_brightness_supported());

    let _ = state.commit_key_brightness(level(30));

    assert!(drain(&mut receiver).is_empty());
}

/// A write the agent superseded before its turn wrote nothing: even when
/// the keypad then reads exactly the level asked for, nothing is saved from
/// this answer, it is not shown as a refusal, and the keypad is read again.
#[gpui::test]
fn superseded_brightness_does_not_save_matching_readback(cx: &mut gpui::TestAppContext) {
    let (key, config_key, mut receiver) = installed(level(100), cx);
    let write = release(cx, &mut receiver, level(30));

    let events = with_state(cx, |state| {
        state.apply_key_brightness_written(
            &key,
            write,
            level(30),
            Err(WriteError::WriteSuperseded {
                operation: openlogi_core::hid::HidppOperation::WriteKeyBrightness,
            }),
        )
    });
    cx.run_until_parked();

    assert_eq!(events, [StateEvent::KeyBrightnessChanged(key)]);
    assert!(!with_state(cx, |state| state.current_key_brightness_refused()));
    assert_eq!(saved(cx, &config_key), None);
    answer_read(&mut receiver, level(30));
    cx.run_until_parked();
    assert_eq!(
        saved(cx, &config_key),
        None,
        "a matching reading after a superseded write saves nothing"
    );
}

/// An asleep keypad still lists its route, but a release could reach it
/// neither as a write nor as a save: it is not writable, and a release
/// sends nothing.
#[test]
fn offline_keypad_brightness_cannot_accept_release() {
    let (mut state, mut receiver) = super::keypad_pages::asleep_keypad_state();
    drain(&mut receiver);
    assert!(
        state.current_key_brightness_supported(),
        "the measurement stays"
    );
    assert!(!state.current_key_brightness_writable());

    let _ = state.commit_key_brightness(level(30));

    assert!(writes(&drain(&mut receiver)).is_empty());
}
