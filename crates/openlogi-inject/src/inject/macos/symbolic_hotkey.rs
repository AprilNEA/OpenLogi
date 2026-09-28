use std::ffi::{c_int, c_uint, c_ushort, c_void};

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use super::app_services_symbol;

const SPACE_LEFT: u32 = 79;
const SPACE_RIGHT: u32 = 81;

/// Switch to the previous desktop / Space.
pub(super) fn previous_desktop() {
    post_symbolic_hotkey(SPACE_LEFT);
}

/// Switch to the next desktop / Space.
pub(super) fn next_desktop() {
    post_symbolic_hotkey(SPACE_RIGHT);
}

fn post_symbolic_hotkey(hotkey: u32) {
    let Some(cgs) = cgs_hotkey_api() else {
        tracing::warn!(hotkey, "CGS symbolic hotkey API unavailable");
        return;
    };

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
        return;
    }

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

    use super::HotkeyRestore;

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
                hotkey: super::SPACE_LEFT,
                set_enabled: record_restore,
            };
            panic!("exercise unwind cleanup");
        }));

        assert!(result.is_err());
        assert_eq!(RESTORED_HOTKEY.load(Ordering::Relaxed), super::SPACE_LEFT);
    }
}
