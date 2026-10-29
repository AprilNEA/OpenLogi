use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    Context, FocusHandle, InteractiveElement as _, IntoElement, KeyBinding, Keystroke, Render,
    Styled as _, TestAppContext, Window, actions, div,
};

use super::*;

actions!(shortcut_recorder_test, [CloseProbe]);

fn record(keystroke: &str) -> Recording {
    Recording::from(&Keystroke::parse(keystroke).expect("keystroke parses"))
}

fn chord(text: &str) -> Recording {
    Recording::Chord(text.parse().expect("chord parses"))
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

struct Probe {
    focus: FocusHandle,
    closed: Rc<Cell<u32>>,
}

impl Render for Probe {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let closed = self.closed.clone();
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_action(move |_: &CloseProbe, _, _| closed.set(closed.get() + 1))
    }
}

#[gpui::test]
fn a_recording_takes_one_key_press_ahead_of_key_bindings(cx: &mut TestAppContext) {
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-w", CloseProbe, None)]));
    let closed = Rc::new(Cell::new(0));
    let (view, cx) = cx.add_window_view({
        let closed = closed.clone();
        move |_, cx| Probe {
            focus: cx.focus_handle(),
            closed,
        }
    });
    cx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        focus.focus(window, cx);
        window.draw(cx).clear(cx);
    });

    let chords = Rc::new(RefCell::new(Vec::new()));
    let recorder = cx.update(|window, cx| {
        let chords = chords.clone();
        ShortcutRecorder::start(window, cx, move |recording, _, _| {
            chords.borrow_mut().push(recording);
        })
    });
    cx.simulate_keystrokes("cmd-w");
    assert_eq!(*chords.borrow(), [record("cmd-w")]);
    assert_eq!(closed.get(), 0, "the binding does not run while recording");

    cx.simulate_keystrokes("cmd-w");
    assert_eq!(chords.borrow().len(), 1, "a recording takes one key press");
    assert_eq!(closed.get(), 1, "the next press reaches the binding again");
    drop(recorder);
}
