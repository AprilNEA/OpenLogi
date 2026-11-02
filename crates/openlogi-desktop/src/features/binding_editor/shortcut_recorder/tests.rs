use gpui::{
    Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent,
    Keystroke, Modifiers, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, actions, div, prelude::FluentBuilder as _,
};

use super::*;

actions!(shortcut_recorder_test, [CloseProbe]);

fn record(keystroke: &str) -> Recording {
    Recording::from(&Keystroke::parse(keystroke).expect("keystroke parses"))
}

fn chord(text: &str) -> Recording {
    Recording::Chord(text.parse().expect("chord parses"))
}

const CMD_SHIFT: Modifiers = Modifiers {
    control: false,
    alt: false,
    shift: true,
    platform: true,
    function: false,
};

/// A keystroke as GPUI's platforms report a shifted digit or symbol: the
/// shifted character, without Shift.
fn reported(key: &str) -> Keystroke {
    Keystroke {
        modifiers: Modifiers {
            platform: true,
            ..Modifiers::default()
        },
        key: key.into(),
        key_char: None,
    }
}

fn logo() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Super"
    }
}

#[test]
fn a_key_press_records_its_modifiers_and_key() {
    assert_eq!(record("ctrl-shift-pageup"), chord("Ctrl+Shift+PageUp"));
    assert_eq!(record("alt-f4"), chord("Alt+F4"));
    assert_eq!(record("backspace"), chord("Backspace"));
    assert_eq!(record("a"), chord("A"));
    let logo = if cfg!(target_os = "macos") {
        "Cmd+W"
    } else {
        "Super+W"
    };
    assert_eq!(record("cmd-w"), chord(logo));
}

#[test]
fn escape_alone_cancels_and_a_key_a_shortcut_cannot_send_is_unsupported() {
    assert_eq!(record("escape"), Recording::Cancelled);
    assert_eq!(record("shift-escape"), chord("Shift+Escape"));
    assert_eq!(record("f21"), Recording::Unsupported);
}

/// #2121: Cmd+Shift+] arrives as Cmd+`}` on US layouts, and Cmd+Shift+7 as
/// Cmd+`/` on German. Recording either as a chord would send another key.
#[test]
fn shift_with_a_digit_or_symbol_key_is_not_recorded() {
    for key in ["}", "!", "/"] {
        assert_eq!(
            Recording::from_press(&reported(key), CMD_SHIFT),
            Recording::ShiftedSymbol,
            "{key}"
        );
    }
}

#[test]
fn shift_that_gpui_keeps_still_records() {
    let press = |keystroke: &str| {
        let keystroke = Keystroke::parse(keystroke).expect("keystroke parses");
        Recording::from_press(&keystroke, keystroke.modifiers)
    };
    let logo = logo();
    assert_eq!(press("cmd-shift-a"), chord(&format!("{logo}+Shift+A")));
    assert_eq!(press("shift-tab"), chord("Shift+Tab"));
    assert_eq!(press("ctrl-shift-pageup"), chord("Ctrl+Shift+PageUp"));
    assert_eq!(
        Recording::from_press(&reported("/"), reported("/").modifiers),
        chord(&format!("{logo}+/")),
        "without Shift held, a symbol records as pressed"
    );
}

/// A window whose root handles `CloseProbe` (bound to Cmd+W) and renders a
/// recording area while `show_record_area` is set.
struct Probe {
    focus: FocusHandle,
    record_area: FocusHandle,
    show_record_area: bool,
    recorder: Option<ShortcutRecorder>,
    recordings: Vec<Recording>,
    closed: u32,
}

impl RecordsShortcut for Probe {
    fn shortcut_recorded(
        &mut self,
        recording: Recording,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.recorder = None;
        self.recordings.push(recording);
    }
}

impl Render for Probe {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_action(cx.listener(|probe, _: &CloseProbe, _, _| probe.closed += 1))
            .when(self.show_record_area, |root| {
                root.child(div().size_10().track_focus(&self.record_area))
            })
    }
}

fn probe_window(cx: &mut TestAppContext) -> (Entity<Probe>, &mut VisualTestContext) {
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-w", CloseProbe, None)]));
    let (probe, cx) = cx.add_window_view(|_, cx| Probe {
        focus: cx.focus_handle(),
        record_area: cx.focus_handle(),
        show_record_area: true,
        recorder: None,
        recordings: Vec::new(),
        closed: 0,
    });
    // Focus events only fire in an active window.
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let focus = probe.read(cx).focus.clone();
        focus.focus(window, cx);
        window.draw(cx).clear(cx);
    });
    probe.update_in(cx, |probe, window, cx| {
        let area = probe.record_area.clone();
        probe.recorder = Some(ShortcutRecorder::start(&area, window, cx));
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    (probe, cx)
}

#[gpui::test]
fn a_recording_takes_one_key_press_ahead_of_key_bindings(cx: &mut TestAppContext) {
    let (probe, cx) = probe_window(cx);

    cx.simulate_keystrokes("cmd-w");
    probe.read_with(cx, |probe, _| {
        assert_eq!(probe.recordings, [record("cmd-w")]);
        assert_eq!(probe.closed, 0, "the binding does not run while recording");
    });

    cx.simulate_keystrokes("cmd-w");
    probe.read_with(cx, |probe, _| {
        assert_eq!(probe.recordings.len(), 1, "a recording takes one key press");
        assert_eq!(probe.closed, 1, "the next press reaches the binding again");
    });
}

/// The interceptor reads Shift from the window's modifier state, which GPUI
/// updates from modifier changes and not from key presses (#2121).
#[gpui::test]
fn a_recording_sees_shift_that_the_keystroke_dropped(cx: &mut TestAppContext) {
    let (probe, cx) = probe_window(cx);

    cx.simulate_modifiers_change(CMD_SHIFT);
    cx.simulate_event(KeyDownEvent {
        keystroke: reported("}"),
        is_held: false,
        prefer_character_input: false,
    });
    probe.read_with(cx, |probe, _| {
        assert_eq!(probe.recordings, [Recording::ShiftedSymbol]);
    });
}

/// #2138 review: leaving the editor, for example by switching tabs, must not
/// leave a recording that swallows the next key press.
#[gpui::test]
fn a_recording_ends_when_its_area_is_no_longer_rendered(cx: &mut TestAppContext) {
    let (probe, cx) = probe_window(cx);

    probe.update(cx, |probe, cx| {
        probe.show_record_area = false;
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
    probe.read_with(cx, |probe, _| {
        assert_eq!(probe.recordings, [Recording::Cancelled]);
        assert!(probe.recorder.is_none());
    });

    cx.update(|window, cx| {
        let focus = probe.read(cx).focus.clone();
        focus.focus(window, cx);
        window.draw(cx).clear(cx);
    });
    cx.simulate_keystrokes("cmd-w");
    probe.read_with(cx, |probe, _| {
        assert_eq!(probe.recordings, [Recording::Cancelled]);
        assert_eq!(probe.closed, 1, "the key press reaches its binding");
    });
}
