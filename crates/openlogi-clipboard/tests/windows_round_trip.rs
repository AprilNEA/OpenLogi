//! The Windows backend against a real clipboard: the one of the logon
//! session's non-interactive window station, so the signed-in user's clipboard
//! (`WinSta0`'s) is never touched. Entering refuses `WinSta0` outright, and
//! the station's clipboard is emptied before and after the test.
//!
//! Ignored by default because it changes the process window station, which
//! some sessions refuse. Run it with
//! `cargo test -p openlogi-clipboard --test windows_round_trip -- --ignored`.
#![cfg(target_os = "windows")]
#![expect(
    clippy::tests_outside_test_module,
    reason = "Cargo integration tests are test-only crates"
)]
#![expect(
    unsafe_code,
    reason = "window stations and desktops are only reachable through FFI"
)]

use std::sync::{Mutex, PoisonError};
use std::{io, iter, ptr, thread};

use std::path::PathBuf;

use openlogi_clipboard::{
    ClipboardText, change_count, read_files, read_text, write_files, write_text,
};
use windows_sys::Win32::Foundation::GENERIC_ALL;
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard};
use windows_sys::Win32::System::StationsAndDesktops::{
    CreateDesktopW, CreateWindowStationW, GetProcessWindowStation, GetUserObjectInformationW,
    HWINSTA, SetProcessWindowStation, SetThreadDesktop, UOI_NAME,
};

/// The interactive window station, whose clipboard is the user's.
const INTERACTIVE_STATION: &str = "WinSta0";

/// The window station belongs to the whole process: one test in it at a time.
static STATION: Mutex<()> = Mutex::new(());

/// The process's window station while a non-interactive one replaces it.
struct PrivateStation {
    previous: HWINSTA,
}

impl PrivateStation {
    /// Moves the process to the logon session's non-interactive window station
    /// and the calling thread to a desktop in it, then empties that station's
    /// clipboard. The thread must not have created windows or hooks.
    fn enter() -> io::Result<Self> {
        let desktop_name = wide(&format!("OpenLogiClipboardTest{}", std::process::id()));
        // SAFETY: plain query of this process's window station.
        let previous = unsafe { GetProcessWindowStation() };
        // SAFETY: a null name asks for the logon session's own station, which a
        // standard user may create (a named one needs more rights); a null
        // security descriptor means the default one.
        let station = unsafe { CreateWindowStationW(ptr::null(), 0, GENERIC_ALL, ptr::null()) };
        if station.is_null() {
            return Err(failed("CreateWindowStationW"));
        }
        if station_name(station)?.eq_ignore_ascii_case(INTERACTIVE_STATION) {
            return Err(io::Error::other("refusing the interactive window station"));
        }
        // SAFETY: `station` is the live handle created above.
        if unsafe { SetProcessWindowStation(station) } == 0 {
            return Err(failed("SetProcessWindowStation"));
        }
        let restore = Self { previous };
        // SAFETY: `desktop_name` is NUL-terminated UTF-16 that outlives the
        // call; no device, no mode, default security.
        let desktop = unsafe {
            CreateDesktopW(
                desktop_name.as_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                GENERIC_ALL,
                ptr::null(),
            )
        };
        if desktop.is_null() {
            return Err(failed("CreateDesktopW"));
        }
        // SAFETY: `desktop` is live and belongs to the process window station;
        // the caller's thread owns no windows or hooks.
        if unsafe { SetThreadDesktop(desktop) } == 0 {
            return Err(failed("SetThreadDesktop"));
        }
        empty_clipboard()?;
        Ok(restore)
    }
}

impl Drop for PrivateStation {
    fn drop(&mut self) {
        let _ = empty_clipboard();
        // SAFETY: `previous` is the process's original, still-open station.
        // The other station and the desktop close with the process.
        unsafe { SetProcessWindowStation(self.previous) };
    }
}

fn station_name(station: HWINSTA) -> io::Result<String> {
    const NAME_UNITS: usize = 256;
    const NAME_BYTES: u32 = 512;
    let mut name = [0_u16; NAME_UNITS];
    let mut needed = 0_u32;
    // SAFETY: `name` is NAME_BYTES of writable memory that outlives the call,
    // and `needed` is a writable u32.
    let read = unsafe {
        GetUserObjectInformationW(
            station,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            NAME_BYTES,
            &raw mut needed,
        )
    };
    if read == 0 {
        return Err(failed("GetUserObjectInformationW"));
    }
    let len = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(NAME_UNITS);
    Ok(String::from_utf16_lossy(&name[..len]))
}

fn empty_clipboard() -> io::Result<()> {
    // SAFETY: a null owner is valid for an immediate open; nothing else is
    // passed.
    if unsafe { OpenClipboard(ptr::null_mut()) } == 0 {
        return Err(failed("OpenClipboard"));
    }
    // SAFETY: this thread holds the clipboard open.
    let emptied = unsafe { EmptyClipboard() } != 0;
    let error = (!emptied).then(|| failed("EmptyClipboard"));
    // SAFETY: closes the clipboard this thread opened above.
    unsafe { CloseClipboard() };
    error.map_or(Ok(()), Err)
}

fn failed(step: &str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{step}: {error}"))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(iter::once(0)).collect()
}

#[test]
#[ignore = "changes the process window station; run explicitly"]
fn text_round_trips_through_a_private_clipboard() {
    let _one_at_a_time = STATION.lock().unwrap_or_else(PoisonError::into_inner);
    // A fresh thread has no windows or hooks, as SetThreadDesktop requires.
    thread::spawn(|| {
        let _station = PrivateStation::enter().expect("enter a non-interactive window station");
        assert_eq!(read_text(64).expect("read empty"), ClipboardText::Absent);

        let text = "Flow \u{2713} clipboard \u{1f5b1}";
        write_text(text).expect("write text");
        assert_eq!(
            read_text(64).expect("read text"),
            ClipboardText::Text(text.to_owned())
        );
        assert_eq!(
            read_text(4).expect("read capped text"),
            ClipboardText::TooLarge
        );

        write_text("second").expect("replace text");
        assert_eq!(
            read_text(64).expect("read replaced text"),
            ClipboardText::Text("second".to_owned())
        );
    })
    .join()
    .expect("clipboard thread");
}

#[test]
#[ignore = "changes the process window station; run explicitly"]
fn files_round_trip_through_a_private_clipboard() {
    let _one_at_a_time = STATION.lock().unwrap_or_else(PoisonError::into_inner);
    // A fresh thread has no windows or hooks, as SetThreadDesktop requires.
    thread::spawn(|| {
        let _station = PrivateStation::enter().expect("enter a non-interactive window station");
        assert_eq!(read_files().expect("read empty"), None);
        let before = change_count();

        // A file drop names paths; none has to exist.
        let paths = vec![
            PathBuf::from(r"C:\Users\Public\Documents\report.pdf"),
            PathBuf::from(r"C:\Users\Public\Pictures\u{dc}n\u{ef}c\u{f8}d\u{e9}"),
        ];
        write_files(&paths).expect("write files");
        assert_eq!(read_files().expect("read files"), Some(paths));
        assert_ne!(change_count(), before, "a write changes the count");
        assert_eq!(
            read_text(64).expect("read text"),
            ClipboardText::Absent,
            "a file drop is not text"
        );

        write_text("replaced").expect("replace files with text");
        assert_eq!(read_files().expect("read replaced files"), None);
    })
    .join()
    .expect("clipboard thread");
}
