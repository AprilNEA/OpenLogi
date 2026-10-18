//! Windows clipboard text through `CF_UNICODETEXT`, and files through
//! `CF_HDROP`.
//!
//! The clipboard is opened without an owner window: only delayed rendering
//! needs one, and this renders immediately. Windows synthesizes `CF_TEXT` and
//! `CF_OEMTEXT` from the Unicode text for readers that want them.
#![expect(
    unsafe_code,
    reason = "the Win32 clipboard is only reachable through FFI"
)]

use std::ffi::{OsString, c_void};
use std::io;
use std::iter;
use std::marker::PhantomData;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::PathBuf;
use std::ptr;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY};
use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW};

use crate::{ClipboardError, ClipboardText};

pub(crate) const AVAILABLE: bool = true;

/// The registered format through which a file copy tells a paste to copy or
/// to move.
const PREFERRED_DROP_EFFECT: &str = "Preferred DropEffect";

/// The `DROPFILES` header in front of a `CF_HDROP` path list.
const DROPFILES_LEN: usize = size_of::<DROPFILES>();

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

pub(crate) fn read_files() -> Result<Option<Vec<PathBuf>>, ClipboardError> {
    let format = u32::from(CF_HDROP);
    // SAFETY: takes a format id by value and needs no open clipboard.
    if unsafe { IsClipboardFormatAvailable(format) } == 0 {
        return Ok(None);
    }
    let _open = OpenClipboardGuard::open()?;
    // SAFETY: this thread holds the clipboard open; the handle stays the
    // clipboard's, and is only used before `_open` closes it.
    let drop = unsafe { GetClipboardData(format) };
    if drop.is_null() {
        return Ok(None);
    }
    // SAFETY: `drop` is the open clipboard's live CF_HDROP; index u32::MAX
    // with no buffer asks for the number of files.
    let count = unsafe { DragQueryFileW(drop, u32::MAX, ptr::null_mut(), 0) };
    let mut paths = Vec::new();
    for index in 0..count {
        // SAFETY: as above; no buffer asks for the path's length in UTF-16
        // units, without its NUL.
        let len = unsafe { DragQueryFileW(drop, index, ptr::null_mut(), 0) };
        if len == 0 {
            continue;
        }
        let mut units = vec![0_u16; len as usize + 1];
        // SAFETY: `units` has room for `len + 1` units: the path and its NUL.
        let copied = unsafe { DragQueryFileW(drop, index, units.as_mut_ptr(), len + 1) };
        units.truncate(copied as usize);
        paths.push(PathBuf::from(OsString::from_wide(&units)));
    }
    Ok((!paths.is_empty()).then_some(paths))
}

pub(crate) fn write_files(paths: &[PathBuf]) -> Result<(), ClipboardError> {
    // Filled before the clipboard is taken, so it is held open only for the
    // swap itself.
    let drop_files = encode_drop_files(paths);
    let files = GlobalMemory::alloc(drop_files.len())?;
    Locked::new(files.0)?.bytes_mut()[..drop_files.len()].copy_from_slice(&drop_files);
    let effect_bytes = DROPEFFECT_COPY.to_le_bytes();
    let effect = GlobalMemory::alloc(effect_bytes.len())?;
    Locked::new(effect.0)?.bytes_mut()[..effect_bytes.len()].copy_from_slice(&effect_bytes);
    let effect_name = encode_units(PREFERRED_DROP_EFFECT);
    // SAFETY: `effect_name` is NUL-terminated UTF-16 that outlives the call.
    let effect_format = unsafe { RegisterClipboardFormatW(effect_name.as_ptr()) };

    let _open = OpenClipboardGuard::open()?;
    // SAFETY: this thread holds the clipboard open.
    if unsafe { EmptyClipboard() } == 0 {
        return Err(os_error("empty"));
    }
    // SAFETY: this thread holds the clipboard open and just emptied it, and
    // `files` is an unlocked moveable object; on success the system owns it.
    if unsafe { SetClipboardData(u32::from(CF_HDROP), files.0) }.is_null() {
        return Err(os_error("write"));
    }
    files.hand_over();
    // Without it a paste copies anyway; with it no target can take the files
    // for a move.
    // SAFETY: as above, for the unlocked `effect` object.
    if effect_format != 0 && !unsafe { SetClipboardData(effect_format, effect.0) }.is_null() {
        effect.hand_over();
    }
    Ok(())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the platforms without a backend have no counter to return"
)]
pub(crate) fn change_count() -> Option<u64> {
    // SAFETY: a plain query with no preconditions.
    Some(u64::from(unsafe { GetClipboardSequenceNumber() }))
}

/// `paths` as a `CF_HDROP` object: a `DROPFILES` header marking the list
/// wide, then every path NUL-terminated, then one more NUL.
fn encode_drop_files(paths: &[PathBuf]) -> Vec<u8> {
    let mut units: Vec<u16> = paths
        .iter()
        .flat_map(|path| path.as_os_str().encode_wide().chain(iter::once(0)))
        .collect();
    units.push(0);
    if paths.is_empty() {
        units.push(0);
    }
    let mut bytes = Vec::with_capacity(DROPFILES_LEN + size_of_val(units.as_slice()));
    // DROPFILES { pFiles, pt: POINT { x, y }, fNC, fWide }, little-endian.
    let list_offset = u32::try_from(DROPFILES_LEN)
        .unwrap_or_else(|_| unreachable!("DROPFILES is a 20-byte header"));
    bytes.extend_from_slice(&list_offset.to_le_bytes());
    bytes.extend_from_slice(&[0; 12]);
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    bytes.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
    bytes
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
    data: *mut c_void,
    bytes: usize,
}

impl Locked {
    fn new(handle: HGLOBAL) -> Result<Self, ClipboardError> {
        // SAFETY: `handle` is a live global memory object, from
        // GetClipboardData under an open clipboard or from GlobalAlloc.
        let data = unsafe { GlobalLock(handle) };
        if data.is_null() {
            return Err(os_error("lock"));
        }
        // SAFETY: as above; a locked object's size does not change.
        let bytes = unsafe { GlobalSize(handle) };
        Ok(Self {
            handle,
            data,
            bytes,
        })
    }

    fn units(&self) -> &[u16] {
        // SAFETY: GlobalLock returned the start of the object's GlobalSize
        // bytes, which stay valid and in place while it is locked, and global
        // memory is 8-byte aligned.
        unsafe { std::slice::from_raw_parts(self.data.cast::<u16>(), self.bytes / 2) }
    }

    fn units_mut(&mut self) -> &mut [u16] {
        // SAFETY: as in `units`; `&mut self` makes this the only view.
        unsafe { std::slice::from_raw_parts_mut(self.data.cast::<u16>(), self.bytes / 2) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `units`.
        unsafe { std::slice::from_raw_parts_mut(self.data.cast::<u8>(), self.bytes) }
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

    /// Reads a wide `CF_HDROP` object back, as `DragQueryFileW` would.
    fn decode_drop_files(bytes: &[u8]) -> Vec<PathBuf> {
        let offset = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(
            i32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            1,
            "fWide"
        );
        let units: Vec<u16> = bytes[offset..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        assert_eq!(units.last(), Some(&0), "the list ends in an extra NUL");
        units[..units.len() - 1]
            .split(|&unit| unit == 0)
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(OsString::from_wide(path)))
            .collect()
    }

    #[test]
    fn the_drop_files_header_matches_the_win32_layout() {
        assert_eq!(DROPFILES_LEN, 20);
        assert_eq!(std::mem::offset_of!(DROPFILES, pFiles), 0);
        assert_eq!(std::mem::offset_of!(DROPFILES, fWide), 16);
    }

    #[test]
    fn copied_files_are_encoded_as_a_wide_double_nul_list() {
        let paths = vec![
            PathBuf::from(r"C:\Users\me\Downloads\report.pdf"),
            PathBuf::from(r"D:\Fotos\Ünïcødé folder"),
        ];
        let bytes = encode_drop_files(&paths);
        assert_eq!(decode_drop_files(&bytes), paths);
        assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0], "double NUL");
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
