//! The key that types a character under the user's keyboard layout.
//!
//! A macOS virtual keycode names a physical key, not a character: posting
//! `kVK_ANSI_Q` with ⌃⌘ on AZERTY presses ⌃⌘A, because that key types A
//! there (#1430, #343). Shortcut synthesis therefore looks up which key types
//! the shortcut's character, the way the user would press it.
//!
//! Only the bare layer counts. Adding Shift or Option would make a different
//! shortcut (⌘⇧3 takes a screenshot; ⌘3 does not), and apps localize the key
//! equivalent of a character their layout reaches only through a modifier, so
//! neither the character's key nor its US position reliably matches there
//! (#1479). Such a key, like a layout that cannot be read, keeps its US
//! position.
//!
//! Text Input Source Services must run on the main thread — reading the layout
//! elsewhere crashes inside HIToolbox — so the layout's `uchr` data is copied
//! out on the main thread and translated with `UCKeyTranslate`, which is a
//! pure function of that data, on the caller's thread.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{CFData, CFRetained, CFString, CFType};

use super::main_thread::on_main;

// HIToolbox and CarbonCore have no objc2 bindings: `objc2-carbon` skips
// HIToolbox and `objc2-core-services` skips CarbonCore.
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> Option<NonNull<CFType>>;
    fn TISGetInputSourceProperty(source: &CFType, key: &CFString) -> Option<NonNull<CFType>>;
    fn LMGetKbdType() -> u8;
    static kTISPropertyUnicodeKeyLayoutData: &'static CFString;
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn UCKeyTranslate(
        layout: *const c_void,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        key_translate_options: u32,
        dead_key_state: *mut u32,
        max_string_length: usize,
        actual_string_length: *mut usize,
        unicode_string: *mut u16,
    ) -> i32;
}

/// `kUCKeyActionDown`.
const KEY_ACTION_DOWN: u16 = 0;
/// `kUCKeyTranslateNoDeadKeysMask`: a dead key reports its own accent instead
/// of starting a composition.
const NO_DEAD_KEYS: u32 = 1;
/// `cmdKey` in the classic Event Manager modifier bits, shifted right by 8 as
/// `UCKeyTranslate` takes them.
const MOD_COMMAND: u32 = 0x01;

/// The key that types `target` with neither Shift nor Option under the active
/// keyboard layout, or `None` when no key does or the layout cannot be read.
///
/// `command` matters for layouts such as "Dvorak – QWERTY ⌘", which switch to
/// QWERTY while Command is held.
pub(super) fn layout_key(target: char, command: bool) -> Option<u16> {
    current_layout()?.key_for(target, command)
}

/// A keyboard layout's `uchr` data and the hardware keyboard type it is
/// translated for.
struct Layout {
    uchr: Vec<u8>,
    keyboard_type: u32,
}

impl Layout {
    fn key_for(&self, target: char, command: bool) -> Option<u16> {
        let modifiers = if command { MOD_COMMAND } else { 0 };
        typing_keys().find(|&vk| self.types(vk, modifiers) == Some(target))
    }

    /// The single character `vk` types with `modifiers` held.
    fn types(&self, vk: u16, modifiers: u32) -> Option<char> {
        let mut dead_key_state = 0;
        let mut len = 0;
        let mut text = [0_u16; 4];
        // SAFETY: `uchr` is a whole `UCKeyboardLayout` copied out of its input
        // source and outlives the call; the out-pointers are live locals and
        // `text` holds the length passed.
        let status = unsafe {
            UCKeyTranslate(
                self.uchr.as_ptr().cast(),
                vk,
                KEY_ACTION_DOWN,
                modifiers,
                self.keyboard_type,
                NO_DEAD_KEYS,
                &raw mut dead_key_state,
                text.len(),
                &raw mut len,
                text.as_mut_ptr(),
            )
        };
        (status == 0 && len == 1)
            .then(|| char::from_u32(u32::from(text[0])))
            .flatten()
    }
}

/// The keys a layout places characters on: the main typing block, including
/// ISO's extra key, and JIS's ¥ and _ keys. Never the keypad, whose digits
/// sit at fixed positions and are not what a shortcut's digit means.
fn typing_keys() -> impl Iterator<Item = u16> {
    (0x00..=0x32).chain([0x5d, 0x5e])
}

/// Read the active layout on the main thread.
fn current_layout() -> Option<Layout> {
    on_main("keyboard layout", |_| read_current_layout()).flatten()
}

/// Must run on the main thread.
fn read_current_layout() -> Option<Layout> {
    // SAFETY: a Copy-rule function; ownership of the +1 reference passes to
    // the `CFRetained`.
    let source = unsafe { CFRetained::from_raw(TISCopyCurrentKeyboardLayoutInputSource()?) };
    let uchr = layout_data(&source)?;
    // SAFETY: reads a process global and has no preconditions.
    let keyboard_type = u32::from(unsafe { LMGetKbdType() });
    Some(Layout {
        uchr,
        keyboard_type,
    })
}

/// Copy `source`'s `uchr` data. Input methods without one (some CJK input
/// sources) return `None`.
fn layout_data(source: &CFType) -> Option<Vec<u8>> {
    // SAFETY: `source` is a live input source and the key a Carbon-exported
    // constant; the result follows the Get rule, borrowed from `source`.
    let data = unsafe { TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData) }?;
    // SAFETY: the borrowed property stays alive as long as `source`, which
    // outlives this reference.
    let data = unsafe { data.as_ref() };
    Some(data.downcast_ref::<CFData>()?.to_vec())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, PoisonError};

    use objc2_core_foundation::CFArray;

    use super::*;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISCreateInputSourceList(
            properties: *const c_void,
            include_all_installed: bool,
        ) -> Option<NonNull<CFArray<CFType>>>;
        static kTISPropertyInputSourceID: &'static CFString;
    }

    /// The harness runs tests off the main thread. The main-thread rule
    /// guards an app whose main thread runs AppKit's event loop alongside the
    /// read; a test process runs none (enigo reads off-main in exactly that
    /// case), so serialising the reads is what remains: two tests must not
    /// race HIToolbox's lazy setup.
    static TIS: Mutex<()> = Mutex::new(());

    /// A layout that ships with every macOS, by input-source ID. The ID's
    /// case has varied across releases, so it is matched without it.
    fn installed(id: &str) -> Layout {
        let _tis = TIS.lock().unwrap_or_else(PoisonError::into_inner);
        // SAFETY: a Create-rule function; null properties list every source.
        let sources = unsafe { TISCreateInputSourceList(std::ptr::null(), true) }
            .expect("input sources are listed");
        // SAFETY: ownership of the +1 reference passes to the `CFRetained`.
        let sources = unsafe { CFRetained::from_raw(sources) };
        let source = sources
            .iter()
            .find(|source| source_id(source).is_some_and(|name| name.eq_ignore_ascii_case(id)))
            .unwrap_or_else(|| panic!("{id} ships with macOS"));
        Layout {
            uchr: layout_data(&source).expect("a keyboard layout has uchr data"),
            // SAFETY: reads a process global and has no preconditions.
            keyboard_type: u32::from(unsafe { LMGetKbdType() }),
        }
    }

    fn source_id(source: &CFType) -> Option<String> {
        // SAFETY: `source` is a live input source and the key a
        // Carbon-exported constant; the result follows the Get rule.
        let name = unsafe { TISGetInputSourceProperty(source, kTISPropertyInputSourceID) }?;
        // SAFETY: the borrowed property lives as long as `source`, which
        // outlives this reference.
        let name = unsafe { name.as_ref() };
        Some(name.downcast_ref::<CFString>()?.to_string())
    }

    #[test]
    fn azerty_types_q_and_a_where_qwerty_types_a_and_q() {
        let french = installed("com.apple.keylayout.French");
        assert_eq!(french.key_for('q', true), Some(0x00));
        assert_eq!(french.key_for('a', true), Some(0x0c));
    }

    #[test]
    fn a_character_behind_shift_has_no_key() {
        // AZERTY types 3 only with Shift, and a shortcut must not gain one.
        let french = installed("com.apple.keylayout.French");
        assert_eq!(french.key_for('3', true), None);
    }

    #[test]
    fn dvorak_moves_punctuation_too() {
        let dvorak = installed("com.apple.keylayout.Dvorak");
        assert_eq!(dvorak.key_for('[', true), Some(0x1b));
    }

    #[test]
    fn us_layout_keeps_ansi_positions() {
        let us = installed("com.apple.keylayout.US");
        assert_eq!(us.key_for('q', true), Some(0x0c));
        assert_eq!(us.key_for('[', true), Some(0x21));
    }

    #[test]
    fn dvorak_qwerty_command_uses_qwerty_only_while_command_is_held() {
        let dvorak = installed("com.apple.keylayout.DVORAK-QWERTYCMD");
        assert_eq!(dvorak.key_for('s', true), Some(0x01));
        assert_eq!(dvorak.key_for('s', false), Some(0x29));
    }
}
