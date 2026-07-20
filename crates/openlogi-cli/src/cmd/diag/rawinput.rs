//! `openlogi diag rawinput` — OS-level input tap via the Win32 RawInput API.
//!
//! Registers whole HID usage pages (digitizer `0x0D`, haptics `0x0E`, the
//! Logitech vendor pages `0xFF00`/`0xFF43`) with `RIDEV_INPUTSINK` and
//! hex-dumps every report Windows delivers — INCLUDING reports consumed by
//! exclusive OS owners like the Precision Touchpad stack. This is the only
//! user-mode way to observe the Bolt receiver's touch-pad collection
//! (`0x000D/0x05`), whose node `CreateFile` cannot open. Windows-only;
//! read-only (registration does not steal input from its owners).

use anyhow::Result;
use clap::Args;

#[derive(Debug, Args)]
pub struct RawInputArgs {
    /// How long to listen, in seconds.
    #[arg(long, default_value_t = 45)]
    pub seconds: u64,
}

#[cfg(not(target_os = "windows"))]
pub async fn run(_args: RawInputArgs) -> Result<()> {
    anyhow::bail!("`diag rawinput` is Windows-only")
}

#[cfg(target_os = "windows")]
pub async fn run(args: RawInputArgs) -> Result<()> {
    // RawInput delivery is synchronous window messaging; the pump owns a
    // dedicated blocking thread rather than the async runtime.
    let seconds = args.seconds;
    tokio::task::spawn_blocking(move || windows_impl::listen(seconds)).await?
}

#[cfg(target_os = "windows")]
#[expect(
    unsafe_code,
    reason = "raw Win32 RawInput FFI — a read-only diagnostic input tap with no safe wrapper available"
)]
mod windows_impl {
    use std::collections::HashMap;
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Input::{
        GetRawInputData, GetRawInputDeviceInfoW, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
        RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK, RIDEV_PAGEONLY, RIDI_DEVICENAME, RIM_TYPEHID,
        RegisterRawInputDevices,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostQuitMessage,
        RegisterClassW, SetTimer, TranslateMessage, WM_INPUT, WM_TIMER, WNDCLASSW,
    };

    /// Set once at listen start; the window proc reads it for timestamps.
    static START: OnceLock<Instant> = OnceLock::new();

    fn device_names() -> &'static Mutex<HashMap<usize, String>> {
        static NAMES: OnceLock<Mutex<HashMap<usize, String>>> = OnceLock::new();
        NAMES.get_or_init(Mutex::default)
    }

    pub fn listen(seconds: u64) -> Result<()> {
        let _ = START.set(Instant::now());

        let class_name: Vec<u16> = "openlogi_rawinput\0".encode_utf16().collect();
        let window_name: Vec<u16> = "openlogi rawinput tap\0".encode_utf16().collect();

        // SAFETY: plain Win32 window bootstrap; all pointers passed are either
        // valid locals or null where the API documents null as acceptable.
        let hwnd = unsafe {
            let hinstance = GetModuleHandleW(null());
            let mut wc: WNDCLASSW = zeroed();
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = hinstance;
            wc.lpszClassName = class_name.as_ptr();
            // Re-registration in one process returns 0 with
            // ERROR_CLASS_ALREADY_EXISTS — harmless for this tool's lifetime.
            RegisterClassW(&wc);

            CreateWindowExW(
                0,
                class_name.as_ptr(),
                window_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                hinstance,
                null(),
            )
        };
        if hwnd.is_null() {
            // SAFETY: trivial error-code read.
            bail!("CreateWindowExW failed: {}", unsafe { GetLastError() });
        }

        let flags = RIDEV_INPUTSINK | RIDEV_PAGEONLY;
        let registrations = [
            // Digitizer page — the receiver's touch-pad collection lives here.
            rid(0x000d, flags, hwnd),
            // Haptics page — the receiver's 0x000E collection.
            rid(0x000e, flags, hwnd),
            // Logitech vendor pages (HID++ short/long and friends).
            rid(0xff00, flags, hwnd),
            rid(0xff43, flags, hwnd),
        ];
        // SAFETY: registrations array outlives the call; cbSize matches.
        let ok = unsafe {
            RegisterRawInputDevices(
                registrations.as_ptr(),
                registrations.len() as u32,
                size_of::<RAWINPUTDEVICE>() as u32,
            )
        };
        if ok == 0 {
            // SAFETY: trivial error-code read.
            bail!("RegisterRawInputDevices failed: {}", unsafe {
                GetLastError()
            });
        }

        println!(
            "raw-input tap live for {seconds} s — pages 0x0D digitizer, 0x0E haptics, 0xFF00/0xFF43 vendor"
        );

        // SAFETY: standard message pump on the window created above; the
        // timer id is private to this window.
        unsafe {
            SetTimer(
                hwnd,
                1,
                u32::try_from(seconds * 1000).unwrap_or(45_000),
                None,
            );
            let mut msg: MSG = zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        println!("raw-input tap closed");
        Ok(())
    }

    fn rid(usage_page: u16, flags: u32, hwnd: HWND) -> RAWINPUTDEVICE {
        RAWINPUTDEVICE {
            usUsagePage: usage_page,
            usUsage: 0, // RIDEV_PAGEONLY requires usage 0
            dwFlags: flags,
            hwndTarget: hwnd,
        }
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_INPUT => {
                dump_input(lparam as HRAWINPUT);
                // WM_INPUT must still reach DefWindowProc for cleanup.
                // SAFETY: forwarding the untouched arguments.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_TIMER => {
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
                0
            }
            // SAFETY: default handling for everything else.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn dump_input(handle: HRAWINPUT) {
        let header_size = size_of::<RAWINPUTHEADER>() as u32;
        let mut size = 0u32;
        // SAFETY: sizing call per RawInput protocol (null buffer, then fetch).
        unsafe {
            GetRawInputData(handle, RID_INPUT, null_mut(), &mut size, header_size);
        }
        if size == 0 {
            return;
        }
        let mut buf = vec![0u8; size as usize];
        // SAFETY: buffer is exactly the size Windows requested.
        let got = unsafe {
            GetRawInputData(
                handle,
                RID_INPUT,
                buf.as_mut_ptr().cast(),
                &mut size,
                header_size,
            )
        };
        if got == 0 || got == u32::MAX {
            return;
        }
        // SAFETY: Windows filled `buf` with a RAWINPUT structure of at least
        // `got` bytes; the buffer is aligned by Vec<u8> only to 1, so read
        // fields via raw pointer without constructing a reference... a u8 Vec
        // is sufficiently aligned in practice for RAWINPUT on x86-64 heap
        // allocations (16-byte aligned), and this mirrors the canonical
        // RawInput usage pattern.
        let raw = buf.as_ptr().cast::<RAWINPUT>();
        // SAFETY: header fields are plain integers within the filled buffer.
        let (dw_type, h_device) = unsafe { ((*raw).header.dwType, (*raw).header.hDevice) };
        if dw_type != RIM_TYPEHID {
            return;
        }
        // SAFETY: dwType == RIM_TYPEHID guarantees the union holds RAWHID.
        let (each, count, data_ptr) = unsafe {
            let hid = &(*raw).data.hid;
            (
                hid.dwSizeHid as usize,
                hid.dwCount as usize,
                hid.bRawData.as_ptr(),
            )
        };
        let name = lookup_name(h_device as usize);
        let ms = START.get().map_or(0, |start| start.elapsed().as_millis());
        for i in 0..count {
            // SAFETY: bRawData holds dwCount packed reports of dwSizeHid bytes
            // inside the buffer Windows sized for us.
            let report = unsafe { std::slice::from_raw_parts(data_ptr.add(i * each), each) };
            let hex: String = report.iter().map(|b| format!("{b:02x} ")).collect();
            println!("[{ms:>7}ms] {name} len={each}: {hex}");
        }
    }

    fn lookup_name(handle: usize) -> String {
        if let Some(name) = device_names()
            .lock()
            .ok()
            .and_then(|map| map.get(&handle).cloned())
        {
            return name;
        }
        let mut len = 0u32;
        // SAFETY: sizing call, then bounded fetch into a matching buffer.
        let name = unsafe {
            GetRawInputDeviceInfoW(handle as _, RIDI_DEVICENAME, null_mut(), &mut len);
            let mut buf = vec![0u16; len as usize];
            let got = GetRawInputDeviceInfoW(
                handle as _,
                RIDI_DEVICENAME,
                buf.as_mut_ptr().cast(),
                &mut len,
            );
            if got == u32::MAX || got == 0 {
                format!("hdev={handle:x}")
            } else {
                let full = String::from_utf16_lossy(&buf[..got as usize]);
                let full = full.trim_end_matches('\0');
                // Announce the full path once; per-report lines carry the
                // discriminating VID/PID/interface segment.
                println!("device hdev={handle:x}: {full}");
                let core: String = full
                    .split('#')
                    .nth(1)
                    .unwrap_or(full)
                    .chars()
                    .take(40)
                    .collect();
                core
            }
        };
        if let Ok(mut map) = device_names().lock() {
            map.insert(handle, name.clone());
        }
        name
    }
}
