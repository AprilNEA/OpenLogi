//! Windows clipboard text through `CF_UNICODETEXT`.
//!
//! The clipboard is opened without an owner window: only delayed rendering
//! needs one, and this renders immediately. Windows synthesizes `CF_TEXT` and
//! `CF_OEMTEXT` from the Unicode text for readers that want them.
#![expect(
    unsafe_code,
    reason = "the Win32 clipboard is only reachable through FFI"
)]

use std::io;
use std::marker::PhantomData;
use std::ptr;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

use crate::{ClipboardError, ClipboardText};

pub(crate) const AVAILABLE: bool = true;

/// How often opening a clipboard that another program holds is tried, and
/// the pause between tries: holders let go within milliseconds.
const OPEN_ATTEMPTS: u32 = 10;
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(10);

pub(crate) fn read_text(max_bytes: usize) -> Result<ClipboardText, ClipboardError> {
    let format = u32::from(CF_UNICODETEXT);
    // SAFETY: takes a format id by value and needs no open clipboard.
    if unsafe { IsClipboardFormatAvailable(format) } == 0 {
        return Ok(ClipboardText::Absent);
    }
    let _open = OpenClipboardGuard::open()?;
    // SAFETY: this thread holds the clipboard open; the handle stays the
    // clipboard's, and is only used before `_open` closes it.
    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        // Emptied between the availability check and the open.
        return Ok(ClipboardText::Absent);
    }
    let locked = Locked::new(handle)?;
    Ok(decode_units(locked.units(), max_bytes))
}

pub(crate) fn write_text(text: &str) -> Result<(), ClipboardError> {
    let units = encode_units(text);
    // Filled before the clipboard is taken, so it is held open only for the
    // swap itself.
    let memory = GlobalMemory::alloc(size_of_val(units.as_slice()))?;
    Locked::new(memory.0)?.units_mut()[..units.len()].copy_from_slice(&units);

    let format = u32::from(CF_UNICODETEXT);
    let _open = OpenClipboardGuard::open()?;
    // SAFETY: this thread holds the clipboard open.
    if unsafe { EmptyClipboard() } == 0 {
        return Err(os_error("empty"));
    }
    // SAFETY: this thread holds the clipboard open and just emptied it, and
    // `memory` is an unlocked moveable object; on success the system owns it.
    if unsafe { SetClipboardData(format, memory.0) }.is_null() {
        return Err(os_error("write"));
    }
    memory.hand_over();
    Ok(())
}

/// The text of a `CF_UNICODETEXT` buffer: UTF-16 up to the first NUL, which a
/// buffer can lack or follow with slack.
fn decode_units(units: &[u16], max_bytes: usize) -> ClipboardText {
    let len = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    // Every UTF-16 unit takes at least one byte of UTF-8, so this bounds the
    // copy before any is made.
    if len > max_bytes {
        return ClipboardText::TooLarge;
    }
    let text = String::from_utf16_lossy(&units[..len]);
    if text.is_empty() {
        ClipboardText::Absent
    } else if text.len() > max_bytes {
        ClipboardText::TooLarge
    } else {
        ClipboardText::Text(text)
    }
}

/// `text` as a NUL-terminated `CF_UNICODETEXT` buffer.
fn encode_units(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn os_error(operation: &'static str) -> ClipboardError {
    ClipboardError::Os {
        operation,
        source: io::Error::last_os_error(),
    }
}

/// The clipboard, held open by this thread until dropped. `!Send`: only the
/// thread that opened it may close it.
struct OpenClipboardGuard(PhantomData<*const ()>);

impl OpenClipboardGuard {
    fn open() -> Result<Self, ClipboardError> {
        for attempt in 1..=OPEN_ATTEMPTS {
            // SAFETY: a null owner ties the open clipboard to this task, which
            // immediate rendering allows; the call takes nothing else.
            if unsafe { OpenClipboard(ptr::null_mut()) } != 0 {
                return Ok(Self(PhantomData));
            }
            if attempt < OPEN_ATTEMPTS {
                thread::sleep(OPEN_RETRY_DELAY);
            }
        }
        Err(ClipboardError::Busy)
    }
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: the guard exists only while this thread holds the clipboard
        // open.
        unsafe { CloseClipboard() };
    }
}

/// A global memory object, locked for access until dropped.
struct Locked {
    handle: HGLOBAL,
    data: *mut u16,
    units: usize,
}

impl Locked {
    fn new(handle: HGLOBAL) -> Result<Self, ClipboardError> {
        // SAFETY: `handle` is a live global memory object, from
        // GetClipboardData under an open clipboard or from GlobalAlloc.
        let data = unsafe { GlobalLock(handle) }.cast::<u16>();
        if data.is_null() {
            return Err(os_error("lock"));
        }
        // SAFETY: as above; a locked object's size does not change.
        let units = unsafe { GlobalSize(handle) } / size_of::<u16>();
        Ok(Self {
            handle,
            data,
            units,
        })
    }

    fn units(&self) -> &[u16] {
        // SAFETY: GlobalLock returned the start of the object's GlobalSize
        // bytes, which stay valid and in place while it is locked, and global
        // memory is 8-byte aligned.
        unsafe { std::slice::from_raw_parts(self.data, self.units) }
    }

    fn units_mut(&mut self) -> &mut [u16] {
        // SAFETY: as in `units`; `&mut self` makes this the only view.
        unsafe { std::slice::from_raw_parts_mut(self.data, self.units) }
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        // SAFETY: balances the GlobalLock in `new` on the same live object.
        unsafe { GlobalUnlock(self.handle) };
    }
}

/// A moveable global memory object this process still owns; freed on drop.
struct GlobalMemory(HGLOBAL);

impl GlobalMemory {
    fn alloc(bytes: usize) -> Result<Self, ClipboardError> {
        // SAFETY: allocates from plain values.
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if handle.is_null() {
            Err(os_error("allocation"))
        } else {
            Ok(Self(handle))
        }
    }

    /// The clipboard took the object and frees it from now on.
    fn hand_over(self) {
        std::mem::forget(self);
    }
}

impl Drop for GlobalMemory {
    fn drop(&mut self) {
        // SAFETY: the object is live, unlocked, and still this process's to
        // free.
        unsafe { GlobalFree(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn text_ends_at_the_first_nul_and_ignores_slack() {
        let mut buffer = units("copied");
        buffer.extend([0, u16::from(b'x'), 0]);
        assert_eq!(
            decode_units(&buffer, 64),
            ClipboardText::Text("copied".to_owned())
        );
        assert_eq!(
            decode_units(&units("no terminator"), 64),
            ClipboardText::Text("no terminator".to_owned())
        );
    }

    #[test]
    fn empty_text_is_absent() {
        assert_eq!(decode_units(&[], 64), ClipboardText::Absent);
        assert_eq!(decode_units(&[0, 0], 64), ClipboardText::Absent);
    }

    #[test]
    fn the_byte_limit_counts_utf8() {
        assert_eq!(
            decode_units(&units("abc"), 3),
            ClipboardText::Text("abc".to_owned())
        );
        assert_eq!(decode_units(&units("abcd"), 3), ClipboardText::TooLarge);
        // Two UTF-16 units, four UTF-8 bytes.
        assert_eq!(
            decode_units(&units("\u{e9}\u{e9}"), 3),
            ClipboardText::TooLarge
        );
    }

    #[test]
    fn an_unpaired_surrogate_reads_as_a_replacement_character() {
        assert_eq!(
            decode_units(&[u16::from(b'a'), 0xd800, u16::from(b'b')], 64),
            ClipboardText::Text("a\u{fffd}b".to_owned())
        );
    }

    #[test]
    fn written_text_is_nul_terminated_and_reads_back() {
        let text = "Flow \u{2713} \u{1f5b1}";
        let buffer = encode_units(text);
        assert_eq!(buffer.last(), Some(&0));
        assert_eq!(
            decode_units(&buffer, 64),
            ClipboardText::Text(text.to_owned())
        );
    }
}
