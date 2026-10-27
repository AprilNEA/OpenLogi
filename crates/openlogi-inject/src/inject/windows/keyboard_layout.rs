//! The keyboard layout that translates the keys `SendInput` posts.
//!
//! A physical key press carries a hardware scan code next to its virtual key.
//! Some apps match a shortcut by that scan code once the active layout types a
//! non-Latin character with the key: under a Russian or Ukrainian layout,
//! Illustrator answers the physical `V` key but ignores a `V` posted with scan
//! code 0, although both reach it as the same virtual key and type the same
//! `м` (#2026). Synthesis therefore gives such a key the scan code of the key
//! the layout produces its virtual key with, as the keyboard does.
//!
//! The layout is the foreground window's, not the calling thread's: the
//! agent's threads own no focused window, and the window that receives the
//! keys translates them with its own input language.
use std::ptr::null_mut;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, HKL, MAPVK_VK_TO_VSC, MapVirtualKeyExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// The input language of the thread that owns the foreground window.
#[derive(Clone, Copy, Debug)]
pub(super) struct ForegroundLayout(HKL);

impl ForegroundLayout {
    /// The layout as it is now. With no foreground window, this thread's
    /// layout stands in for it.
    pub(super) fn current() -> Self {
        // SAFETY: GetForegroundWindow takes no arguments; it may return null.
        let window = unsafe { GetForegroundWindow() };
        // SAFETY: a null process-id pointer asks for the thread id alone, and
        // a null or stale window makes the call return 0.
        let thread = unsafe { GetWindowThreadProcessId(window, null_mut()) };
        // SAFETY: GetKeyboardLayout accepts any thread id; 0 names this thread.
        Self(unsafe { GetKeyboardLayout(thread) })
    }

    /// The scan code of the key that produces `vk` under this layout, or 0
    /// when no key does.
    pub(super) fn scan_code(self, vk: u16) -> u16 {
        // SAFETY: MapVirtualKeyExW reads no caller memory; the layout handle is
        // an identifier the system returned, not a pointer it dereferences.
        let scan = unsafe { MapVirtualKeyExW(u32::from(vk), MAPVK_VK_TO_VSC, self.0) };
        u16::try_from(scan).unwrap_or(0)
    }

    /// The virtual key the key with `scan` produces under this layout, or 0
    /// when it produces none: the inverse of [`Self::scan_code`].
    #[cfg(test)]
    pub(super) fn virtual_key(self, scan: u16) -> u16 {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::MAPVK_VSC_TO_VK;
        // SAFETY: as in `scan_code`.
        let vk = unsafe { MapVirtualKeyExW(u32::from(scan), MAPVK_VSC_TO_VK, self.0) };
        u16::try_from(vk).unwrap_or(0)
    }
}
