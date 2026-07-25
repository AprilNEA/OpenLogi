//! Physical key-chord recorder for the shortcut editor.
//!
//! Reads the keyboard's **physical** state (`GetAsyncKeyState`) on a timer
//! rather than handling key events, for two reasons: the dialog must record
//! chords the OS or another app has already claimed as a global hotkey
//! (`Win+Ctrl+Space` is exactly such a case — the owner swallows the
//! message, but the physical state is still readable), and the same
//! technique already backs the ring overlay's Esc/outside-click watcher.
//!
//! The recorder emits the **canonical chord text**
//! ([`KeyCombo::parse`]'s input language, e.g. `"Win+Ctrl+Space"`) instead
//! of a [`KeyCombo`], so parsing and display keep exactly one source of
//! truth: whatever a user could type, the recorder produces, and vice versa.

/// What one [`ChordRecorder::poll`] observed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    /// Nothing pressed yet — keep listening.
    Idle,
    /// Keys are down; `0` is the chord so far (shown live in the dialog).
    Holding(String),
    /// Every key was released: `0` is the finished chord.
    Done(String),
    /// Escape alone — the user backed out of listening.
    Cancelled,
}

/// Accumulates the chord across polls: the modifiers held when a
/// non-modifier key goes down, finalized when everything is released.
#[derive(Default)]
pub struct ChordRecorder {
    /// The best (most complete) chord seen while keys were held.
    held: Option<String>,
}

impl ChordRecorder {
    /// Sample the keyboard once. Call on a ~30 ms timer while listening.
    pub fn poll(&mut self) -> Poll {
        self.advance(platform::sample())
    }

    /// [`Self::poll`] with the keyboard reading supplied, so the state
    /// machine is testable without depending on what is physically held
    /// while the suite runs.
    fn advance(&mut self, sample: (Vec<&'static str>, Option<&'static str>)) -> Poll {
        let (modifiers, key) = sample;
        // Escape with no modifiers is the cancel gesture, never a binding —
        // matching the ring overlay, where Esc means "back out".
        if modifiers.is_empty() && key == Some("Escape") {
            self.held = None;
            return Poll::Cancelled;
        }
        match key {
            Some(key) => {
                let mut chord = String::new();
                for name in &modifiers {
                    chord.push_str(name);
                    chord.push('+');
                }
                chord.push_str(key);
                self.held = Some(chord.clone());
                Poll::Holding(chord)
            }
            // A bare modifier is not a chord; wait for the real key.
            None if !modifiers.is_empty() => Poll::Idle,
            // Everything is up: a chord that was held is now complete.
            None => match self.held.take() {
                Some(chord) => Poll::Done(chord),
                None => Poll::Idle,
            },
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    #![expect(
        unsafe_code,
        reason = "GetAsyncKeyState is the Win32 physical-key-state API"
    )]

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    /// Modifier virtual keys, in the canonical order [`super::ChordRecorder`]
    /// emits them (matching `KeyCombo::parse`'s rendering).
    const MODIFIERS: &[(i32, &str)] = &[
        (0x5B, "Win"), // VK_LWIN
        (0x5C, "Win"), // VK_RWIN
        (0x11, "Ctrl"),
        (0x12, "Alt"),
        (0x10, "Shift"),
    ];

    /// Named non-modifier keys. Letters, digits and F-keys are generated in
    /// [`sample`] rather than listed.
    const NAMED: &[(i32, &str)] = &[
        (0x20, "Space"),
        (0x0D, "Enter"),
        (0x09, "Tab"),
        (0x1B, "Escape"),
        (0x08, "Backspace"),
        (0x2E, "Delete"),
        (0x24, "Home"),
        (0x23, "End"),
        (0x21, "PageUp"),
        (0x22, "PageDown"),
        (0x26, "Up"),
        (0x28, "Down"),
        (0x25, "Left"),
        (0x27, "Right"),
    ];

    /// Whether `vk` is physically down right now.
    fn down(vk: i32) -> bool {
        // SAFETY: GetAsyncKeyState takes a virtual-key code and only reads
        // process-wide keyboard state; no pointers are involved.
        let state = unsafe { GetAsyncKeyState(vk) };
        state.cast_unsigned() & 0x8000 != 0
    }

    /// The modifiers currently held (deduplicated, canonical order) and the
    /// first non-modifier key held, if any.
    pub fn sample() -> (Vec<&'static str>, Option<&'static str>) {
        let mut modifiers: Vec<&'static str> = Vec::new();
        for (vk, name) in MODIFIERS {
            if down(*vk) && !modifiers.contains(name) {
                modifiers.push(name);
            }
        }

        // Letters (VK_A..VK_Z) and digits (VK_0..VK_9) share their ASCII
        // codes, so their names come straight from the code.
        for vk in 0x41..=0x5A_i32 {
            if down(vk) {
                let name = LETTERS[usize::try_from(vk - 0x41).unwrap_or(0)];
                return (modifiers, Some(name));
            }
        }
        for vk in 0x30..=0x39_i32 {
            if down(vk) {
                let name = DIGITS[usize::try_from(vk - 0x30).unwrap_or(0)];
                return (modifiers, Some(name));
            }
        }
        for vk in 0x70..=0x7B_i32 {
            if down(vk) {
                let name = FUNCTION_KEYS[usize::try_from(vk - 0x70).unwrap_or(0)];
                return (modifiers, Some(name));
            }
        }
        for (vk, name) in NAMED {
            if down(*vk) {
                return (modifiers, Some(name));
            }
        }
        (modifiers, None)
    }

    /// `'static` names for VK_A..VK_Z, indexed by `vk - 0x41`.
    const LETTERS: [&str; 26] = [
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
        "S", "T", "U", "V", "W", "X", "Y", "Z",
    ];
    /// `'static` names for VK_0..VK_9, indexed by `vk - 0x30`.
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    /// `'static` names for VK_F1..VK_F12, indexed by `vk - 0x70`.
    const FUNCTION_KEYS: [&str; 12] = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    ];
}

#[cfg(not(target_os = "windows"))]
mod platform {
    /// Recording is Windows-first, like the ring overlay itself; elsewhere
    /// the dialog's typed-chord path is the way in.
    pub fn sample() -> (Vec<&'static str>, Option<&'static str>) {
        (Vec::new(), None)
    }
}

/// Whether this platform can record chords (gates the dialog's button).
#[must_use]
pub const fn is_supported() -> bool {
    cfg!(target_os = "windows")
}

#[cfg(test)]
mod tests {
    use super::{ChordRecorder, Poll};

    /// Nothing held: no chord is ever invented, however long we wait.
    #[test]
    fn idle_polls_never_finalize_a_chord() {
        let mut recorder = ChordRecorder::default();
        assert_eq!(recorder.advance((vec![], None)), Poll::Idle);
        assert_eq!(recorder.advance((vec![], None)), Poll::Idle);
    }

    /// The full gesture: modifiers first (not yet a chord), then the key,
    /// then release — emitting the chord exactly once.
    #[test]
    fn a_held_chord_is_emitted_once_on_release() {
        let mut recorder = ChordRecorder::default();
        assert_eq!(
            recorder.advance((vec!["Ctrl"], None)),
            Poll::Idle,
            "a bare modifier is not a chord"
        );
        assert_eq!(
            recorder.advance((vec!["Ctrl", "Shift"], Some("P"))),
            Poll::Holding("Ctrl+Shift+P".into())
        );
        assert_eq!(
            recorder.advance((vec!["Ctrl"], None)),
            Poll::Idle,
            "still coming up — the chord is not final until everything is up"
        );
        assert_eq!(
            recorder.advance((vec![], None)),
            Poll::Done("Ctrl+Shift+P".into())
        );
        assert_eq!(
            recorder.advance((vec![], None)),
            Poll::Idle,
            "not re-emitted"
        );
    }

    /// A chord that grows keeps its most complete form.
    #[test]
    fn the_last_held_chord_wins() {
        let mut recorder = ChordRecorder::default();
        let _ = recorder.advance((vec!["Ctrl"], Some("Space")));
        let _ = recorder.advance((vec!["Win", "Ctrl"], Some("Space")));
        assert_eq!(
            recorder.advance((vec![], None)),
            Poll::Done("Win+Ctrl+Space".into())
        );
    }

    /// Escape alone backs out and discards anything held.
    #[test]
    fn escape_cancels_without_emitting() {
        let mut recorder = ChordRecorder::default();
        let _ = recorder.advance((vec!["Ctrl"], Some("P")));
        assert_eq!(recorder.advance((vec![], Some("Escape"))), Poll::Cancelled);
        assert_eq!(
            recorder.advance((vec![], None)),
            Poll::Idle,
            "the cancelled chord is gone, not pending"
        );
        // Escape *with* a modifier is a real binding, not a cancel.
        assert_eq!(
            recorder.advance((vec!["Ctrl"], Some("Escape"))),
            Poll::Holding("Ctrl+Escape".into())
        );
    }
}
