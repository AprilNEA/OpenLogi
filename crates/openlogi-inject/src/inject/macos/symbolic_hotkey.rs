use std::ffi::{c_int, c_uint, c_ushort, c_void};

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use super::super::space_switch::Direction;
use super::{HeldKey, HeldModifiers, KeyPhase, app_services_symbol};

/// "Save picture of screen as a file", ⌘⇧3 by default.
const SCREENSHOT_TO_FILE: u32 = 28;
/// "Copy picture of selected area to the clipboard", ⌃⌘⇧4 by default.
const SELECTED_AREA_TO_CLIPBOARD: u32 = 31;
/// "Move left a space", ⌃← by default.
const SPACE_LEFT: u32 = 79;
/// "Move right a space", ⌃→ by default.
const SPACE_RIGHT: u32 = 81;

/// The modifiers a physical keyboard presses as keys of their own, in press
/// order. Every other bit of a hotkey's mask (Fn, the numeric pad) is a flag
/// the keyboard sets on the key's own events.
const MODIFIER_KEYS: [(CGEventFlags, HeldKey); 4] = [
    (CGEventFlags::CGEventFlagCommand, HeldKey::Command),
    (CGEventFlags::CGEventFlagControl, HeldKey::Control),
    (CGEventFlags::CGEventFlagShift, HeldKey::Shift),
    (CGEventFlags::CGEventFlagAlternate, HeldKey::Alt),
];

/// `kVK_LeftArrow`…`kVK_UpArrow`.
const ARROW_KEYS: std::ops::RangeInclusive<u16> = 0x7b..=0x7e;

/// Save a picture of the screen to the screenshot folder.
pub(super) fn screenshot() {
    post_symbolic_hotkey(SCREENSHOT_TO_FILE);
}

/// Copy a picture of an area the user selects to the clipboard.
pub(super) fn capture_region() {
    post_symbolic_hotkey(SELECTED_AREA_TO_CLIPBOARD);
}

/// The user's own "Move left/right a space" shortcut.
pub(super) struct SpaceShortcut {
    virtual_key: u16,
    flags: CGEventFlags,
}

/// The user's own "Move left/right a space" shortcut, or `None` when it is
/// disabled or cannot be read.
///
/// Read-only: unlike the screenshot hotkeys, a disabled Space shortcut is
/// never enabled — the caller slides through DockSwipe events instead.
/// Typing the shortcut lets the Dock play the same slide it plays for the
/// keyboard, which synthesized swipe frames only approximate (#1722, #1972).
pub(super) fn space_shortcut(direction: Direction) -> Option<SpaceShortcut> {
    let hotkey = match direction {
        Direction::Previous => SPACE_LEFT,
        Direction::Next => SPACE_RIGHT,
    };
    let cgs = cgs_hotkey_api()?;
    // SAFETY: resolved AppServices symbol called with its expected signature.
    if !unsafe { (cgs.is_enabled)(hotkey) } {
        tracing::debug!(hotkey, "Space shortcut disabled");
        return None;
    }
    let (virtual_key, modifiers) = hotkey_value(cgs, hotkey)?;
    Some(SpaceShortcut {
        virtual_key,
        flags: CGEventFlags::from_bits_truncate(u64::from(modifiers)),
    })
}

impl SpaceShortcut {
    /// Type the shortcut around whatever modifiers held chords keep down.
    ///
    /// The events are built and posted under the held-chord lock, so a
    /// modifier another binding holds is lifted only for the keystroke and
    /// restored before the lock is released, and every event is built before
    /// the first is posted.
    ///
    /// Each edge goes through the virtual HID keyboard when it carries the
    /// edge, as a held chord's edges do, so the shortcut reaches WindowServer
    /// as keyboard input. An edge it does not carry goes out as its `CGEvent`.
    ///
    /// Returns `false`, posting nothing, when a held chord keeps the
    /// shortcut's own key down: its key-up would end that hold early.
    pub(super) fn post(&self) -> bool {
        super::super::with_held_modifiers(|held| {
            if super::held_key_down(self.virtual_key) {
                tracing::debug!("a held chord owns the Space shortcut's key");
                return false;
            }
            let Some(edges) = keystroke(self.virtual_key, self.flags, held) else {
                tracing::warn!("could not build the Space shortcut's key events");
                return false;
            };
            for edge in edges {
                if !super::virtual_keyboard::edge(edge.virtual_key, edge.phase) {
                    edge.event.post(CGEventTapLocation::HID);
                }
            }
            true
        })
    }
}

/// Type `virtual_key` under `flags` as `CGEvent`s, around whatever modifiers
/// held chords keep down, in the shape a physical keyboard sends (see
/// [`keystroke`]).
///
/// This is the fallback for a key press the virtual HID keyboard declined.
/// A key event that only carries the modifier flags can leave those
/// modifiers down: Command stays held, and an App Switcher that Command-Tab
/// opened never closes (#2178).
///
/// No edge is offered to the virtual keyboard again. It declines a press of
/// a key a held chord keeps down on it, but would accept that key's up edge
/// and release the key under the chord.
pub(super) fn post_keystroke(virtual_key: u16, flags: CGEventFlags) {
    super::super::with_held_modifiers(|held| {
        let Some(edges) = keystroke(virtual_key, flags, held) else {
            tracing::warn!(virtual_key, "could not build the shortcut's key events");
            return;
        };
        for edge in edges {
            edge.event.post(CGEventTapLocation::HID);
        }
    });
}

/// One key edge of a keystroke, with the `CGEvent` that posts it when the
/// virtual HID keyboard does not carry it.
struct KeyEdge {
    virtual_key: u16,
    phase: KeyPhase,
    event: CGEvent,
}

impl KeyEdge {
    fn new(virtual_key: u16, flags: CGEventFlags, phase: KeyPhase) -> Option<Self> {
        Some(Self {
            virtual_key,
            phase,
            event: super::key_event(virtual_key, flags, phase)?,
        })
    }
}

/// The edges a physical keyboard sends for `virtual_key` under `flags` while
/// `held` modifiers are already down: each held modifier the shortcut does
/// not use up, each missing one down, the key down and up, then both sets
/// restored in reverse. A held modifier the shortcut uses stays down
/// throughout; one it does not use would turn it into a different shortcut.
///
/// WindowServer matches a synthesized symbolic hotkey only in this shape: a
/// key event that merely carries the modifier flags is ignored, as is an
/// arrow key without the numeric-pad flag every Apple keyboard sets on it
/// (#1653). Apple keyboards also set the Fn flag on an arrow key, and the
/// default Mission Control and Spaces records include it, so an arrow key
/// gets both flags. A custom shortcut carries no Fn bit of its own, and
/// without it ⌃← does not move a Space (#2178).
fn keystroke(virtual_key: u16, flags: CGEventFlags, held: HeldModifiers) -> Option<Vec<KeyEdge>> {
    let pressed: Vec<HeldKey> = MODIFIER_KEYS
        .iter()
        .filter(|&&(flag, key)| flags.contains(flag) && !held.contains(key))
        .map(|&(_, key)| key)
        .collect();
    let released: Vec<HeldKey> = MODIFIER_KEYS
        .iter()
        .filter(|&&(flag, key)| !flags.contains(flag) && held.contains(key))
        .map(|&(_, key)| key)
        .collect();
    let mut key_flags = MODIFIER_KEYS
        .iter()
        .fold(flags, |rest, &(flag, _)| rest.difference(flag));
    if ARROW_KEYS.contains(&virtual_key) {
        key_flags |= CGEventFlags::CGEventFlagNumericPad | CGEventFlags::CGEventFlagSecondaryFn;
    }

    let mut modifiers = held;
    let mut edges = Vec::with_capacity((pressed.len() + released.len()) * 2 + 2);
    for &key in &released {
        let (vk, edge_flags) = super::held_key_event(key, KeyPhase::Up, &mut modifiers)?;
        edges.push(KeyEdge::new(vk, edge_flags, KeyPhase::Up)?);
    }
    for &key in &pressed {
        let (vk, edge_flags) = super::held_key_event(key, KeyPhase::Down, &mut modifiers)?;
        edges.push(KeyEdge::new(vk, edge_flags, KeyPhase::Down)?);
    }
    let chord = super::held_modifier_flags(modifiers) | key_flags;
    edges.push(KeyEdge::new(virtual_key, chord, KeyPhase::Down)?);
    edges.push(KeyEdge::new(virtual_key, chord, KeyPhase::Up)?);
    for &key in pressed.iter().rev() {
        let (vk, edge_flags) = super::held_key_event(key, KeyPhase::Up, &mut modifiers)?;
        edges.push(KeyEdge::new(vk, edge_flags, KeyPhase::Up)?);
    }
    for &key in released.iter().rev() {
        let (vk, edge_flags) = super::held_key_event(key, KeyPhase::Down, &mut modifiers)?;
        edges.push(KeyEdge::new(vk, edge_flags, KeyPhase::Down)?);
    }
    debug_assert_eq!(
        modifiers, held,
        "a keystroke leaves held modifiers as it found them"
    );
    Some(edges)
}

/// The physical key and modifier mask `hotkey` is bound to.
fn hotkey_value(cgs: CgsHotkeyApi, hotkey: u32) -> Option<(u16, u32)> {
    let mut key_equivalent = 0_u16;
    let mut virtual_key = 0_u16;
    let mut modifiers = 0_u32;

    // SAFETY: resolved AppServices symbols are called with their
    // expected signatures and valid out-parameters.
    let err = unsafe {
        (cgs.get_value)(
            hotkey,
            &raw mut key_equivalent,
            &raw mut virtual_key,
            &raw mut modifiers,
        )
    };
    if err != 0 {
        tracing::warn!(hotkey, err, "CGSGetSymbolicHotKeyValue failed");
        return None;
    }
    Some((virtual_key, modifiers))
}

fn post_symbolic_hotkey(hotkey: u32) {
    let Some(cgs) = cgs_hotkey_api() else {
        tracing::warn!(hotkey, "CGS symbolic hotkey API unavailable");
        return;
    };
    let Some((virtual_key, modifiers)) = hotkey_value(cgs, hotkey) else {
        return;
    };

    // SAFETY: resolved AppServices symbol called with its expected
    // signature.
    let was_enabled = unsafe { (cgs.is_enabled)(hotkey) };
    let _restore = if was_enabled {
        None
    } else {
        // SAFETY: resolved AppServices symbol called with its expected
        // signature.
        let err = unsafe { (cgs.set_enabled)(hotkey, true) };
        if err != 0 {
            tracing::warn!(hotkey, err, "CGSSetSymbolicHotKeyEnabled(true) failed");
        }
        // Restore even when the enable call reported an error: the SPI may
        // have changed state before returning it, and this preserves the
        // old unconditional best-effort disable behavior.
        Some(HotkeyRestore {
            hotkey,
            set_enabled: cgs.set_enabled,
        })
    };

    post_key(virtual_key, modifiers);
}

fn post_key(vk: u16, modifiers: u32) {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        tracing::warn!("CGEventSource::new failed for symbolic hotkey");
        return;
    };
    let Ok(down) = CGEvent::new_keyboard_event(src.clone(), vk, true) else {
        tracing::warn!(vk, "CGEvent::new_keyboard_event(down) failed");
        return;
    };
    let flags = CGEventFlags::from_bits_truncate(u64::from(modifiers));
    down.set_flags(flags);
    down.post(CGEventTapLocation::Session);

    let Ok(up) = CGEvent::new_keyboard_event(src, vk, false) else {
        tracing::warn!(vk, "CGEvent::new_keyboard_event(up) failed");
        return;
    };
    up.set_flags(flags);
    up.post(CGEventTapLocation::Session);
}

#[derive(Clone, Copy)]
struct CgsHotkeyApi {
    get_value: CgsGetSymbolicHotKeyValueFn,
    is_enabled: CgsIsSymbolicHotKeyEnabledFn,
    set_enabled: CgsSetSymbolicHotKeyEnabledFn,
}

type CgsGetSymbolicHotKeyValueFn =
    unsafe extern "C" fn(c_uint, *mut c_ushort, *mut c_ushort, *mut c_uint) -> c_int;
type CgsIsSymbolicHotKeyEnabledFn = unsafe extern "C" fn(c_uint) -> bool;
type CgsSetSymbolicHotKeyEnabledFn = unsafe extern "C" fn(c_uint, bool) -> c_int;

struct HotkeyRestore {
    hotkey: u32,
    set_enabled: CgsSetSymbolicHotKeyEnabledFn,
}

impl Drop for HotkeyRestore {
    fn drop(&mut self) {
        // SAFETY: this is the same resolved AppServices function and hotkey
        // id used to open the temporary enable window.
        let err = unsafe { (self.set_enabled)(self.hotkey, false) };
        if err != 0 {
            tracing::warn!(
                hotkey = self.hotkey,
                err,
                "CGSSetSymbolicHotKeyEnabled(false) failed"
            );
        }
    }
}

fn cgs_hotkey_api() -> Option<CgsHotkeyApi> {
    let get_value = app_services_symbol(c"CGSGetSymbolicHotKeyValue")?;
    let is_enabled = app_services_symbol(c"CGSIsSymbolicHotKeyEnabled")?;
    let set_enabled = app_services_symbol(c"CGSSetSymbolicHotKeyEnabled")?;

    // SAFETY: the symbols, when present, have the private SPI
    // signatures declared above.
    Some(unsafe {
        CgsHotkeyApi {
            get_value: std::mem::transmute::<*mut c_void, CgsGetSymbolicHotKeyValueFn>(get_value),
            is_enabled: std::mem::transmute::<*mut c_void, CgsIsSymbolicHotKeyEnabledFn>(
                is_enabled,
            ),
            set_enabled: std::mem::transmute::<*mut c_void, CgsSetSymbolicHotKeyEnabledFn>(
                set_enabled,
            ),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicU32, Ordering};

    use core_graphics::event::{CGEventFlags, CGEventType, EventField};

    use super::{HeldKey, HeldModifiers, HotkeyRestore, KeyEdge, KeyPhase, keystroke};

    const NONE: CGEventFlags = CGEventFlags::CGEventFlagNull;
    const COMMAND: CGEventFlags = CGEventFlags::CGEventFlagCommand;
    const CONTROL: CGEventFlags = CGEventFlags::CGEventFlagControl;
    const SHIFT: CGEventFlags = CGEventFlags::CGEventFlagShift;
    const OPTION: CGEventFlags = CGEventFlags::CGEventFlagAlternate;
    const FN: CGEventFlags = CGEventFlags::CGEventFlagSecondaryFn;
    const PAD: CGEventFlags = CGEventFlags::CGEventFlagNumericPad;

    /// How an event reads back: a modifier key's edges become
    /// `FlagsChanged`, as a physical keyboard's do, so only the flags tell
    /// its press from its release.
    #[derive(Debug, PartialEq, Eq)]
    enum Edge {
        Down,
        Up,
        Flags,
    }
    use Edge::{Down, Flags, Up};

    /// Each edge's event as (virtual key, edge, flags). Also checks that the
    /// key and phase the virtual keyboard is offered are the event's own, so
    /// both paths type the same keystroke.
    fn edges(edges: &[KeyEdge]) -> Vec<(i64, Edge, CGEventFlags)> {
        edges
            .iter()
            .map(|edge| {
                let event = &edge.event;
                let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                assert_eq!(keycode, i64::from(edge.virtual_key));
                let down = match event.get_type() {
                    CGEventType::KeyDown => true,
                    CGEventType::KeyUp => false,
                    // A modifier's own flag tells its press from its release.
                    CGEventType::FlagsChanged => event.get_flags().contains(match keycode {
                        0x37 => COMMAND,
                        0x38 => SHIFT,
                        0x3a => OPTION,
                        0x3b => CONTROL,
                        other => panic!("unexpected modifier key {other:#x}"),
                    }),
                    other => panic!("unexpected event type {other:?}"),
                };
                assert_eq!(edge.phase == KeyPhase::Down, down, "{keycode:#x}");
                let kind = match event.get_type() {
                    CGEventType::FlagsChanged => Flags,
                    _ if down => Down,
                    _ => Up,
                };
                (keycode, kind, event.get_flags())
            })
            .collect()
    }

    /// The default "Move right a space" record is ⌃→ with the Fn bit, which
    /// is how Apple keyboards report arrow keys.
    #[test]
    fn space_shortcut_presses_control_as_a_key_and_flags_the_arrow_like_a_keyboard() {
        let events =
            keystroke(0x7c, CONTROL | FN, HeldModifiers::default()).expect("keyboard events");
        assert_eq!(
            edges(&events),
            [
                (0x3b, Flags, CONTROL),
                (0x7c, Down, CONTROL | FN | PAD),
                (0x7c, Up, CONTROL | FN | PAD),
                (0x3b, Flags, NONE),
            ]
        );
    }

    /// A custom ⌃← shortcut carries no Fn bit, but its arrow must be flagged
    /// as the Spaces record expects, or the Space does not move (#2178).
    #[test]
    fn a_custom_arrow_shortcut_flags_the_arrow_like_a_keyboard() {
        let events = keystroke(0x7b, CONTROL, HeldModifiers::default()).expect("keyboard events");
        assert_eq!(
            edges(&events),
            [
                (0x3b, Flags, CONTROL),
                (0x7b, Down, CONTROL | FN | PAD),
                (0x7b, Up, CONTROL | FN | PAD),
                (0x3b, Flags, NONE),
            ]
        );
    }

    /// A customized shortcut on a non-arrow key gets no numeric-pad flag, and
    /// its modifiers are released in the reverse of their press order.
    #[test]
    fn keystroke_nests_several_modifiers_and_flags_only_arrows_as_numeric_pad() {
        let events =
            keystroke(0x14, COMMAND | SHIFT, HeldModifiers::default()).expect("keyboard events");
        assert_eq!(
            edges(&events),
            [
                (0x37, Flags, COMMAND),
                (0x38, Flags, COMMAND | SHIFT),
                (0x14, Down, COMMAND | SHIFT),
                (0x14, Up, COMMAND | SHIFT),
                (0x38, Flags, COMMAND),
                (0x37, Flags, NONE),
            ]
        );
    }

    /// The `CGEvent` fallback for a shortcut press releases each modifier it
    /// presses as a key of its own: a Tab that only carries Command's flag
    /// left Command down and the App Switcher open (#2178).
    #[test]
    fn a_shortcut_releases_every_modifier_it_presses() {
        let events = keystroke(0x30, COMMAND, HeldModifiers::default()).expect("keyboard events");
        assert_eq!(
            edges(&events),
            [
                (0x37, Flags, COMMAND),
                (0x30, Down, COMMAND),
                (0x30, Up, COMMAND),
                (0x37, Flags, NONE),
            ]
        );
        let events =
            keystroke(0x30, CONTROL | SHIFT, HeldModifiers::default()).expect("keyboard events");
        assert_eq!(
            edges(&events),
            [
                (0x3b, Flags, CONTROL),
                (0x38, Flags, CONTROL | SHIFT),
                (0x30, Down, CONTROL | SHIFT),
                (0x30, Up, CONTROL | SHIFT),
                (0x38, Flags, CONTROL),
                (0x3b, Flags, NONE),
            ]
        );
    }

    /// A modifier a held chord already owns that the shortcut uses is neither
    /// pressed again nor released.
    #[test]
    fn keystroke_leaves_shared_held_modifiers_down() {
        let mut held = HeldModifiers::default();
        held.set(HeldKey::Control, true);
        let events = keystroke(0x7c, CONTROL | FN, held).expect("keyboard events");
        assert_eq!(
            edges(&events),
            [
                (0x7c, Down, CONTROL | FN | PAD),
                (0x7c, Up, CONTROL | FN | PAD),
            ]
        );
    }

    /// A held modifier the shortcut does not use would make it a different
    /// shortcut, so it is lifted around the keystroke and pressed again after.
    #[test]
    fn keystroke_lifts_held_modifiers_the_shortcut_does_not_use() {
        let mut held = HeldModifiers::default();
        held.set(HeldKey::Control, true);
        held.set(HeldKey::Shift, true);
        let events = keystroke(0x7c, CONTROL | FN, held).expect("keyboard events");
        assert_eq!(
            edges(&events),
            [
                (0x38, Flags, CONTROL),
                (0x7c, Down, CONTROL | FN | PAD),
                (0x7c, Up, CONTROL | FN | PAD),
                (0x38, Flags, CONTROL | SHIFT),
            ]
        );

        let mut held = HeldModifiers::default();
        held.set(HeldKey::Shift, true);
        let events = keystroke(0x7c, CONTROL | FN, held).expect("keyboard events");
        assert_eq!(
            edges(&events),
            [
                (0x38, Flags, NONE),
                (0x3b, Flags, CONTROL),
                (0x7c, Down, CONTROL | FN | PAD),
                (0x7c, Up, CONTROL | FN | PAD),
                (0x3b, Flags, NONE),
                (0x38, Flags, SHIFT),
            ]
        );
    }

    /// The hook skips OpenLogi's own key edges only by their tag.
    #[test]
    fn every_keystroke_edge_carries_the_synthetic_tag() {
        for edge in
            keystroke(0x7b, CONTROL | FN, HeldModifiers::default()).expect("keyboard events")
        {
            assert_eq!(
                edge.event
                    .get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
                crate::SYNTHETIC_EVENT_USER_DATA
            );
        }
    }

    static RESTORED_HOTKEY: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn record_restore(hotkey: u32, enabled: bool) -> i32 {
        if !enabled {
            RESTORED_HOTKEY.store(hotkey, Ordering::Relaxed);
        }
        0
    }

    #[test]
    fn temporary_enable_is_restored_during_unwind() {
        RESTORED_HOTKEY.store(0, Ordering::Relaxed);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _restore = HotkeyRestore {
                hotkey: super::SCREENSHOT_TO_FILE,
                set_enabled: record_restore,
            };
            panic!("exercise unwind cleanup");
        }));

        assert!(result.is_err());
        assert_eq!(
            RESTORED_HOTKEY.load(Ordering::Relaxed),
            super::SCREENSHOT_TO_FILE
        );
    }
}
