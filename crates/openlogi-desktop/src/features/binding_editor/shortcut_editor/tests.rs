use gpui::{Render, TestAppContext, VisualTestContext};

use super::*;

/// A view that owns a draft, as the mouse inspector and the key panel do.
struct DraftView {
    draft: ShortcutDraft,
}

impl RecordsShortcut for DraftView {
    fn shortcut_recorded(
        &mut self,
        recording: Recording,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft.recorded(recording, window, cx);
    }
}

impl EditsShortcut for DraftView {
    fn shortcut_draft(&mut self) -> Option<&mut ShortcutDraft> {
        Some(&mut self.draft)
    }
}

impl Render for DraftView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn draft_view(cx: &mut TestAppContext) -> (Entity<DraftView>, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    cx.add_window_view(|window, cx| DraftView {
        draft: ShortcutDraft::new(window, cx),
    })
}

fn chord(text: &str) -> KeyCombo {
    text.parse().expect("test chord parses")
}

fn text(view: &Entity<DraftView>, cx: &mut VisualTestContext) -> String {
    view.read_with(cx, |view, cx| {
        view.draft.input().read(cx).value().to_string()
    })
}

#[test]
fn a_bound_shortcut_names_its_chord_and_mode() {
    let combo = chord("Ctrl+Alt+C");
    assert_eq!(
        ShortcutMode::of(&Action::CustomShortcut(combo.clone())),
        Some((&combo, ShortcutMode::Tap))
    );
    assert_eq!(
        ShortcutMode::of(&Action::HoldShortcut(combo.clone())),
        Some((&combo, ShortcutMode::Hold))
    );
    assert_eq!(ShortcutMode::of(&Action::Copy), None);
    // The modifiers-only hold is a different editor's action.
    assert_eq!(
        ShortcutMode::of(&Action::TapKeyHoldingModifiers(combo)),
        None
    );
}

/// Reopening the editor on a bound shortcut shows its chord in the mode it
/// was saved in; any other binding starts an empty Tap draft.
#[gpui::test]
fn reset_seeds_the_bound_chord_and_mode(cx: &mut TestAppContext) {
    let (view, cx) = draft_view(cx);
    let hold = Action::HoldShortcut(chord("Ctrl+Shift+PageUp"));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.draft.reset(Some(&hold), window, cx));
    });
    assert_eq!(text(&view, cx), "Ctrl+Shift+PageUp");
    view.read_with(cx, |view, _| {
        assert_eq!(view.draft.mode(), ShortcutMode::Hold);
    });

    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.draft.recorded(Recording::Unsupported, window, cx);
            view.draft.reset(Some(&Action::Copy), window, cx);
        });
    });
    assert_eq!(text(&view, cx), "");
    view.read_with(cx, |view, _| {
        assert_eq!(view.draft.mode(), ShortcutMode::Tap);
        assert_eq!(view.draft.error(), None);
    });
}

/// Add commits the field in the chosen mode; text that is no chord commits
/// nothing and shows the error. (Typing in the field takes the error back:
/// the mouse view's picker test types into it.)
#[gpui::test]
fn submit_commits_the_field_in_the_chosen_mode(cx: &mut TestAppContext) {
    let (view, cx) = draft_view(cx);
    let set = |cx: &mut VisualTestContext, value: &'static str| {
        cx.update(|window, cx| {
            let input = view.read(cx).draft.input().clone();
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        });
        cx.run_until_parked();
    };

    set(cx, "ctrl+alt");
    let submitted = view.update(cx, |view, cx| view.draft.submit(cx));
    assert_eq!(submitted, None);
    view.read_with(cx, |view, _| {
        assert_eq!(view.draft.error(), Some(ShortcutError::Unrecognized));
    });

    set(cx, "Ctrl+Alt+C");
    view.update(cx, |view, _| view.draft.set_mode(ShortcutMode::Hold));
    let submitted = view.update(cx, |view, cx| view.draft.submit(cx));
    assert_eq!(submitted, Some(Action::HoldShortcut(chord("Ctrl+Alt+C"))));
}

/// A recorded chord replaces the field; a key the recorder cannot send keeps
/// the field and shows which error applies.
#[gpui::test]
fn a_recording_fills_the_field_or_explains_why_not(cx: &mut TestAppContext) {
    let (view, cx) = draft_view(cx);
    let record = |cx: &mut VisualTestContext, recording: Recording| {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.draft.recorded(recording, window, cx));
        });
    };

    record(cx, Recording::Chord(chord("Alt+Space")));
    assert_eq!(text(&view, cx), "Alt+Space");

    record(cx, Recording::ShiftedSymbol);
    assert_eq!(text(&view, cx), "Alt+Space");
    view.read_with(cx, |view, _| {
        assert_eq!(view.draft.error(), Some(ShortcutError::ShiftedSymbol));
    });

    record(cx, Recording::Cancelled);
    view.read_with(cx, |view, _| {
        assert_eq!(view.draft.error(), Some(ShortcutError::ShiftedSymbol));
        assert!(!view.draft.is_recording());
    });
}
