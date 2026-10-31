//! Executable path of a process that refuses to be opened.
//!
//! Some programs, such as Warcraft III, give their process a security
//! descriptor that denies even `PROCESS_QUERY_LIMITED_INFORMATION`, so
//! `OpenProcess` fails with `ERROR_ACCESS_DENIED`. The kernel still reports
//! their image path through `SystemProcessIdInformation` without a process
//! handle; Task Manager shows such processes the same way. That path is in NT
//! form (`\Device\HarddiskVolume3\Games\War3.exe`), so it is mapped back to the
//! Win32 form that `QueryFullProcessImageNameW` returns, and the profile ID
//! stays the same whichever way it was read.
#![cfg_attr(
    target_os = "windows",
    expect(unsafe_code, reason = "read-only NT and Win32 process-image queries")
)]

#[cfg(target_os = "windows")]
use windows_sys::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS,
};
#[cfg(target_os = "windows")]
use windows_sys::Win32::Foundation::{HANDLE, UNICODE_STRING};
#[cfg(target_os = "windows")]
use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;

/// `SystemProcessIdInformation`. The SDK headers leave it out, but it has been
/// stable since Windows Vista.
#[cfg(target_os = "windows")]
const SYSTEM_PROCESS_ID_INFORMATION: SYSTEM_INFORMATION_CLASS = 88;

/// The longest image path, in UTF-16 units, that a `UNICODE_STRING` can hold:
/// its lengths are `u16` byte counts.
#[cfg(target_os = "windows")]
const MAX_IMAGE_PATH: usize = 32_767;

/// Layout of `SYSTEM_PROCESS_ID_INFORMATION`.
#[cfg(target_os = "windows")]
#[repr(C)]
struct ProcessIdInformation {
    process_id: HANDLE,
    image_name: UNICODE_STRING,
}

/// The Win32 image path of `pid`, read without opening the process.
#[cfg(target_os = "windows")]
pub(super) fn image_path(pid: u32) -> Option<String> {
    let mut buffer = vec![0u16; MAX_IMAGE_PATH];
    let mut info = ProcessIdInformation {
        process_id: std::ptr::without_provenance_mut(usize::try_from(pid).ok()?),
        image_name: UNICODE_STRING {
            Length: 0,
            MaximumLength: u16::try_from(buffer.len() * 2).ok()?,
            Buffer: buffer.as_mut_ptr(),
        },
    };
    // SAFETY: `info` is a live, writable `SYSTEM_PROCESS_ID_INFORMATION` of
    // the size passed. Its `image_name` points at `buffer`, which outlives the
    // call and holds `MaximumLength` bytes.
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_PROCESS_ID_INFORMATION,
            (&raw mut info).cast(),
            u32::try_from(size_of::<ProcessIdInformation>()).ok()?,
            std::ptr::null_mut(),
        )
    };
    let len = usize::from(info.image_name.Length) / 2;
    if status < 0 || len == 0 {
        tracing::debug!(
            pid,
            status,
            "could not read the image path of an unopenable process"
        );
        return None;
    }
    let nt_path = String::from_utf16_lossy(buffer.get(..len)?);
    let path = win32_path(&nt_path, drive_devices());
    tracing::debug!(pid, %path, "read the image path of an unopenable process");
    Some(path)
}

/// Each drive letter with the NT device name it maps to, queried lazily so
/// the search stops at the first match.
#[cfg(target_os = "windows")]
fn drive_devices() -> impl Iterator<Item = (char, String)> {
    (b'A'..=b'Z').filter_map(|letter| {
        let drive = [u16::from(letter), u16::from(b':'), 0];
        let mut device = [0u16; 512];
        // SAFETY: `drive` is a NUL-terminated UTF-16 string and `device` is a
        // writable buffer of the length passed.
        let written = unsafe {
            QueryDosDeviceW(
                drive.as_ptr(),
                device.as_mut_ptr(),
                u32::try_from(device.len()).ok()?,
            )
        };
        if written == 0 {
            return None;
        }
        // The result is a list of NUL-terminated names; the first is current.
        let end = device.iter().position(|&unit| unit == 0)?;
        Some((
            char::from(letter),
            String::from_utf16_lossy(device.get(..end)?),
        ))
    })
}

/// Map an NT image path to its Win32 form, for example
/// `\Device\HarddiskVolume3\Games\War3.exe` to `D:\Games\War3.exe`.
///
/// `drives` yields each drive letter with its NT device name, as
/// `QueryDosDeviceW` reports them. A path on a network share becomes a UNC
/// path. A path on no known drive keeps its NT form: it still ends in the
/// executable name, so `exe:<filename>` profiles keep matching.
fn win32_path(nt_path: &str, drives: impl IntoIterator<Item = (char, String)>) -> String {
    if let Some(share) = strip_prefix_ignore_case(nt_path, r"\Device\Mup\") {
        return format!(r"\\{share}");
    }
    drives
        .into_iter()
        .find_map(|(letter, device)| {
            let rest = strip_prefix_ignore_case(nt_path, &device)?;
            rest.starts_with('\\').then(|| format!("{letter}:{rest}"))
        })
        .unwrap_or_else(|| nt_path.to_owned())
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.get(..prefix.len())?.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::win32_path;

    fn drives() -> Vec<(char, String)> {
        vec![
            ('C', r"\Device\HarddiskVolume1".to_owned()),
            ('D', r"\Device\HarddiskVolume10".to_owned()),
        ]
    }

    #[test]
    fn a_volume_path_maps_to_its_drive_letter() {
        assert_eq!(
            win32_path(
                r"\Device\HarddiskVolume10\Games\Warcraft III\War3.exe",
                drives()
            ),
            r"D:\Games\Warcraft III\War3.exe",
        );
    }

    #[test]
    fn a_volume_prefix_matches_only_a_whole_device_name() {
        // `\Device\HarddiskVolume1` is a text prefix of `...Volume10`.
        assert_eq!(
            win32_path(r"\Device\HarddiskVolume10\War3.exe", drives()),
            r"D:\War3.exe",
        );
        assert_eq!(
            win32_path(r"\Device\HarddiskVolume1\War3.exe", drives()),
            r"C:\War3.exe",
        );
    }

    #[test]
    fn the_device_name_matches_regardless_of_case() {
        assert_eq!(
            win32_path(r"\device\harddiskvolume1\Windows\notepad.exe", drives()),
            r"C:\Windows\notepad.exe",
        );
    }

    #[test]
    fn a_network_share_becomes_a_unc_path() {
        assert_eq!(
            win32_path(r"\Device\Mup\server\games\War3.exe", drives()),
            r"\\server\games\War3.exe",
        );
    }

    #[test]
    fn a_path_on_no_known_drive_keeps_its_nt_form() {
        assert_eq!(
            win32_path(r"\Device\VhdHardDisk{1234}\War3.exe", drives()),
            r"\Device\VhdHardDisk{1234}\War3.exe",
        );
    }
}
