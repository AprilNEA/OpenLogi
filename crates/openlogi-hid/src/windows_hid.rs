#![expect(unsafe_code, reason = "Win32 HID report writes require FFI")]

use std::{
    fmt, io,
    ptr::{null, null_mut},
};

use async_hid::{DeviceId, DeviceInfo};
use tracing::debug;
use windows_sys::Win32::{
    Devices::HumanInterfaceDevice::{
        HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData,
        HidD_GetAttributes, HidD_GetPreparsedData, HidD_SetFeature, HidD_SetOutputReport,
        HidP_GetCaps, PHIDP_PREPARSED_DATA,
    },
    Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        WriteFile,
    },
};

#[derive(Clone, Debug)]
pub(crate) struct NativeHidWriter {
    path: Vec<u16>,
}

impl NativeHidWriter {
    pub(crate) fn new(info: &DeviceInfo) -> Option<Self> {
        let DeviceId::UncPath(path) = &info.id else {
            return None;
        };
        // `HSTRING` derefs to its UTF-16 code units (`[u16]`, sans terminator);
        // copy them directly instead of round-tripping through a lossy UTF-8
        // `String`, which would corrupt a path with unpaired surrogates.
        // CreateFileW needs the path NUL-terminated.
        let mut path: Vec<u16> = path.to_vec();
        path.push(0);
        Some(Self { path })
    }

    pub(crate) fn write_report(&self, report: &[u8]) -> Result<(), NativeWriteError> {
        let mut errors = Vec::new();
        for desired_access in [GENERIC_READ | GENERIC_WRITE, GENERIC_WRITE, 0] {
            let handle = match HidHandle::open(&self.path, desired_access) {
                Ok(handle) => handle,
                Err(e) => {
                    errors.push(format!("open({desired_access:#x}): {e}"));
                    continue;
                }
            };

            let caps = match handle.caps() {
                Ok(caps) => caps,
                Err(e) => {
                    errors.push(format!("caps({desired_access:#x}): {e}"));
                    continue;
                }
            };

            debug!(
                report_id = format_args!("{:#04x}", report.first().copied().unwrap_or_default()),
                report_len = report.len(),
                output_len = caps.OutputReportByteLength,
                feature_len = caps.FeatureReportByteLength,
                desired_access = format_args!("{desired_access:#x}"),
                "trying native Windows HID report write"
            );

            if let Err(e) = try_write_methods(handle.raw(), &caps, report) {
                errors.push(format!("write({desired_access:#x}): {e}"));
                continue;
            }

            return Ok(());
        }

        Err(NativeWriteError::AllMethodsFailed(errors))
    }
}

fn try_write_methods(handle: HANDLE, caps: &HIDP_CAPS, report: &[u8]) -> Result<(), String> {
    let mut errors = Vec::new();

    for len in report_lengths(report.len(), usize::from(caps.OutputReportByteLength)) {
        let buffer = padded_report(report, len);
        match write_file(handle, &buffer) {
            Ok(()) => return Ok(()),
            Err(e) => errors.push(format!("WriteFile(len={len}): {e}")),
        }

        match set_output_report(handle, &buffer) {
            Ok(()) => return Ok(()),
            Err(e) => errors.push(format!("HidD_SetOutputReport(len={len}): {e}")),
        }
    }

    for len in report_lengths(report.len(), usize::from(caps.FeatureReportByteLength)) {
        let buffer = padded_report(report, len);
        match set_feature(handle, &buffer) {
            Ok(()) => return Ok(()),
            Err(e) => errors.push(format!("HidD_SetFeature(len={len}): {e}")),
        }
    }

    Err(errors.join("; "))
}

fn report_lengths(report_len: usize, caps_len: usize) -> impl Iterator<Item = usize> {
    [caps_len, report_len]
        .into_iter()
        .filter(move |len| *len >= report_len && *len > 0)
        .fold(Vec::new(), |mut acc, len| {
            if !acc.contains(&len) {
                acc.push(len);
            }
            acc
        })
        .into_iter()
}

fn padded_report(report: &[u8], len: usize) -> Vec<u8> {
    let mut buffer = vec![0; len];
    buffer[..report.len()].copy_from_slice(report);
    buffer
}

fn write_file(handle: HANDLE, report: &[u8]) -> Result<(), io::Error> {
    let mut written = 0;
    let ok = unsafe {
        WriteFile(
            handle,
            report.as_ptr(),
            report.len().try_into().unwrap_or(u32::MAX),
            &raw mut written,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if usize::try_from(written).ok() != Some(report.len()) {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!("wrote {written} of {} bytes", report.len()),
        ));
    }
    Ok(())
}

fn set_output_report(handle: HANDLE, report: &[u8]) -> Result<(), io::Error> {
    bool_result(unsafe {
        HidD_SetOutputReport(
            handle,
            report.as_ptr().cast(),
            report.len().try_into().unwrap_or(u32::MAX),
        )
    })
}

fn set_feature(handle: HANDLE, report: &[u8]) -> Result<(), io::Error> {
    bool_result(unsafe {
        HidD_SetFeature(
            handle,
            report.as_ptr().cast(),
            report.len().try_into().unwrap_or(u32::MAX),
        )
    })
}

fn bool_result(ok: bool) -> Result<(), io::Error> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

struct HidHandle(HANDLE);

impl HidHandle {
    fn open(path: &[u16], desired_access: u32) -> Result<Self, io::Error> {
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                desired_access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    fn raw(&self) -> HANDLE {
        self.0
    }

    fn caps(&self) -> Result<HIDP_CAPS, NativeWriteError> {
        let mut attributes = HIDD_ATTRIBUTES {
            Size: size_of::<HIDD_ATTRIBUTES>().try_into().unwrap_or(u32::MAX),
            VendorID: 0,
            ProductID: 0,
            VersionNumber: 0,
        };
        if !unsafe { HidD_GetAttributes(self.0, &raw mut attributes) } {
            return Err(NativeWriteError::LastOsError(
                "HidD_GetAttributes",
                io::Error::last_os_error(),
            ));
        }

        let mut preparsed: PHIDP_PREPARSED_DATA = 0;
        if !unsafe { HidD_GetPreparsedData(self.0, &raw mut preparsed) } {
            return Err(NativeWriteError::LastOsError(
                "HidD_GetPreparsedData",
                io::Error::last_os_error(),
            ));
        }

        let _preparsed = PreparsedData(preparsed);
        let mut caps = HIDP_CAPS::default();
        let status = unsafe { HidP_GetCaps(preparsed, &raw mut caps) };
        if status != HIDP_STATUS_SUCCESS {
            return Err(NativeWriteError::HidpStatus("HidP_GetCaps", status));
        }

        Ok(caps)
    }
}

impl Drop for HidHandle {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

struct PreparsedData(PHIDP_PREPARSED_DATA);

impl Drop for PreparsedData {
    fn drop(&mut self) {
        if self.0 != 0 {
            let _ = unsafe { HidD_FreePreparsedData(self.0) };
        }
    }
}

#[derive(Debug)]
pub(crate) enum NativeWriteError {
    AllMethodsFailed(Vec<String>),
    HidpStatus(&'static str, i32),
    LastOsError(&'static str, io::Error),
}

impl fmt::Display for NativeWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllMethodsFailed(errors) => {
                write!(f, "all native Windows HID write methods failed")?;
                for error in errors {
                    write!(f, "; {error}")?;
                }
                Ok(())
            }
            Self::HidpStatus(operation, status) => {
                write!(f, "{operation} returned NTSTATUS {status:#010x}")
            }
            Self::LastOsError(operation, error) => write!(f, "{operation}: {error}"),
        }
    }
}

impl std::error::Error for NativeWriteError {}
