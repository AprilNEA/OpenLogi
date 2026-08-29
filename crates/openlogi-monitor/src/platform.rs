#![allow(unsafe_code, reason = "Windows DDC/CI APIs are C FFI")]

use std::ffi::{OsString, c_void};
use std::hash::{Hash, Hasher};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

use windows_sys::Win32::Devices::Display::{
    CapabilitiesRequestAndCapabilitiesReply, DestroyPhysicalMonitors, GetCapabilitiesStringLength,
    GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR,
    GetVCPFeatureAndVCPFeatureReply, PHYSICAL_MONITOR, SetVCPFeature,
};
use windows_sys::Win32::Foundation::{LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Gdi::{
    DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITORINFOEXW,
};
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, KEY_READ, REG_BINARY, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};

use crate::{MonitorError, MonitorInfo, MonitorInput, input_label};

const VCP_INPUT_SELECT: u8 = 0x60;
const EDD_GET_DEVICE_INTERFACE_NAME: u32 = 0x0000_0001;

pub fn list_monitors() -> Result<Vec<MonitorInfo>, MonitorError> {
    let mut logical = Vec::<HMONITOR>::new();
    // SAFETY: The callback only pushes the monitor handle into the Vec passed
    // via lparam for the duration of EnumDisplayMonitors.
    let ok = unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(enum_monitor),
            (&raw mut logical).cast::<()>() as LPARAM,
        )
    };
    if ok == 0 {
        return Err(MonitorError::WindowsApi {
            operation: "enumerating logical monitors",
        });
    }

    let mut monitors = Vec::new();
    for handle in logical {
        let display_name = logical_display_name(handle)?;
        for (physical_index, physical) in physical_monitors(handle)?.into_iter().enumerate() {
            let description = wide_description(&physical);
            let friendly_name = friendly_monitor_name(&display_name, physical_index)
                .unwrap_or_else(|| {
                    fallback_monitor_name(&display_name, physical_index, &description)
                });
            let capabilities = capabilities(&physical).unwrap_or_default();
            let current_input = current_input(&physical).ok();
            let mut inputs = parse_vcp_60_inputs(&capabilities);
            if inputs.is_empty()
                && let Some(input) = current_input
            {
                inputs.push(MonitorInput {
                    value: input,
                    label: input_label(input),
                });
            }
            monitors.push(MonitorInfo {
                id: monitor_id(&display_name, physical_index, &description),
                friendly_name,
                display_name: display_name.clone(),
                description,
                current_input,
                inputs,
            });
        }
    }
    Ok(monitors)
}

pub fn set_monitor_input(target_id: &str, input: u32) -> Result<(), MonitorError> {
    for monitor in physical_entries()? {
        let id = monitor_id(
            &monitor.display_name,
            monitor.physical_index,
            &monitor.description,
        );
        if id == target_id {
            // SAFETY: The physical monitor handle is valid until destroyed by
            // PhysicalMonitor::drop; VCP 0x60 is the MCCS input-select code.
            let ok = unsafe { SetVCPFeature(monitor.physical.handle(), VCP_INPUT_SELECT, input) };
            return if ok == 0 {
                Err(MonitorError::WindowsApi {
                    operation: "setting monitor input",
                })
            } else {
                Ok(())
            };
        }
    }
    Err(MonitorError::MonitorNotFound(target_id.to_string()))
}

struct PhysicalEntry {
    display_name: String,
    physical_index: usize,
    description: String,
    physical: PhysicalMonitor,
}

fn physical_entries() -> Result<Vec<PhysicalEntry>, MonitorError> {
    let mut entries = Vec::new();
    let monitors = list_logical_monitors()?;
    for handle in monitors {
        let display_name = logical_display_name(handle)?;
        for (physical_index, physical) in physical_monitors(handle)?.into_iter().enumerate() {
            let description = wide_description(&physical);
            entries.push(PhysicalEntry {
                display_name: display_name.clone(),
                physical_index,
                description,
                physical,
            });
        }
    }
    Ok(entries)
}

fn list_logical_monitors() -> Result<Vec<HMONITOR>, MonitorError> {
    let mut logical = Vec::<HMONITOR>::new();
    // SAFETY: Same as in list_monitors; the Vec lives for the whole call.
    let ok = unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(enum_monitor),
            (&raw mut logical).cast::<()>() as LPARAM,
        )
    };
    if ok == 0 {
        return Err(MonitorError::WindowsApi {
            operation: "enumerating logical monitors",
        });
    }
    Ok(logical)
}

unsafe extern "system" fn enum_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows_sys::core::BOOL {
    // SAFETY: data is the Vec<HMONITOR> pointer supplied to EnumDisplayMonitors.
    let monitors = unsafe { &mut *(data as *mut Vec<HMONITOR>) };
    monitors.push(monitor);
    TRUE
}

fn logical_display_name(handle: HMONITOR) -> Result<String, MonitorError> {
    // SAFETY: MONITORINFOEXW is zero-initialized then cbSize is set as required
    // by GetMonitorInfoW.
    let mut info: MONITORINFOEXW = unsafe { zeroed() };
    info.monitorInfo.cbSize =
        u32::try_from(size_of::<MONITORINFOEXW>()).map_err(|_| MonitorError::WindowsApi {
            operation: "sizing monitor info",
        })?;
    // SAFETY: handle is from EnumDisplayMonitors and info points to writable memory.
    let ok = unsafe { GetMonitorInfoW(handle, (&raw mut info).cast()) };
    if ok == 0 {
        return Err(MonitorError::WindowsApi {
            operation: "reading monitor info",
        });
    }
    Ok(wide_z_to_string(&info.szDevice))
}

fn friendly_monitor_name(display_name: &str, physical_index: usize) -> Option<String> {
    let device = display_device(display_name, physical_index)?;
    let device_id = wide_z_to_string(&device.DeviceID);
    let edid_name = edid_name_for_device_id(&device_id);
    edid_name.or_else(|| {
        let device_string = wide_z_to_string(&device.DeviceString);
        meaningful_name(&device_string)
    })
}

fn display_device(display_name: &str, physical_index: usize) -> Option<DISPLAY_DEVICEW> {
    // SAFETY: DISPLAY_DEVICEW is zero-initialized then cb is set as required by
    // EnumDisplayDevicesW. The display name is a null-terminated UTF-16 buffer
    // valid for the duration of the call.
    let mut device: DISPLAY_DEVICEW = unsafe { zeroed() };
    device.cb = u32::try_from(size_of::<DISPLAY_DEVICEW>()).ok()?;
    let display = wide_null(display_name);
    let physical_index = u32::try_from(physical_index).ok()?;
    // SAFETY: device is initialized with its byte size, display is
    // null-terminated, and EnumDisplayDevicesW writes only to device.
    let ok = unsafe {
        EnumDisplayDevicesW(
            display.as_ptr(),
            physical_index,
            &raw mut device,
            EDD_GET_DEVICE_INTERFACE_NAME,
        )
    };
    (ok != 0).then_some(device)
}

fn edid_name_for_device_id(device_id: &str) -> Option<String> {
    let path = edid_registry_path(device_id)?;
    read_registry_binary(&path, "EDID")
        .and_then(|edid| parse_edid_display_name(&edid))
        .and_then(|name| meaningful_name(&name))
}

fn edid_registry_path(device_id: &str) -> Option<String> {
    let trimmed = device_id
        .strip_prefix(r"\\?\DISPLAY#")
        .or_else(|| device_id.strip_prefix(r"DISPLAY#"))?;
    let (display_id, rest) = trimmed.split_once('#')?;
    let (instance, _) = rest.split_once('#')?;
    Some(format!(
        r"SYSTEM\CurrentControlSet\Enum\DISPLAY\{display_id}\{instance}\Device Parameters"
    ))
}

fn read_registry_binary(path: &str, value: &str) -> Option<Vec<u8>> {
    let mut key = std::ptr::null_mut();
    let path = wide_null(path);
    // SAFETY: path is null-terminated and key points to writable storage.
    let status =
        unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.as_ptr(), 0, KEY_READ, &raw mut key) };
    if status != 0 {
        return None;
    }
    let value = wide_null(value);
    let mut value_type = 0_u32;
    let mut len = 0_u32;
    // SAFETY: key is valid, value is null-terminated, and outputs are writable.
    let status = unsafe {
        RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null_mut(),
            &raw mut value_type,
            std::ptr::null_mut(),
            &raw mut len,
        )
    };
    if status != 0 || value_type != REG_BINARY || len == 0 {
        // SAFETY: key was opened successfully above.
        unsafe {
            let _ = RegCloseKey(key);
        }
        return None;
    }
    let mut data = vec![0_u8; len as usize];
    // SAFETY: data has len bytes and all other pointers are valid for the call.
    let status = unsafe {
        RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null_mut(),
            &raw mut value_type,
            data.as_mut_ptr(),
            &raw mut len,
        )
    };
    // SAFETY: key was opened successfully above.
    unsafe {
        let _ = RegCloseKey(key);
    }
    if status == 0 && value_type == REG_BINARY {
        data.truncate(len as usize);
        Some(data)
    } else {
        None
    }
}

fn parse_edid_display_name(edid: &[u8]) -> Option<String> {
    if edid.len() < 128 {
        return None;
    }
    for offset in [54_usize, 72, 90, 108] {
        if edid.get(offset..offset + 5)?[..4] == [0, 0, 0, 0xfc] {
            let name = edid[offset + 5..offset + 18]
                .iter()
                .copied()
                .take_while(|b| !matches!(*b, 0 | b'\n' | b'\r'))
                .collect::<Vec<_>>();
            return String::from_utf8(name).ok();
        }
    }
    None
}

fn meaningful_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Generic PnP Monitor") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn fallback_monitor_name(display_name: &str, physical_index: usize, description: &str) -> String {
    meaningful_name(description).unwrap_or_else(|| {
        let number = display_name
            .trim_start_matches(r"\\.\DISPLAY")
            .parse::<usize>()
            .ok()
            .unwrap_or(physical_index + 1);
        format!("显示器 {number}")
    })
}

struct PhysicalMonitor {
    inner: PHYSICAL_MONITOR,
}

impl PhysicalMonitor {
    fn handle(&self) -> *mut c_void {
        self.inner.hPhysicalMonitor
    }
}

impl Drop for PhysicalMonitor {
    fn drop(&mut self) {
        // SAFETY: The handle was returned by GetPhysicalMonitorsFromHMONITOR
        // and is destroyed exactly once by this Drop implementation.
        unsafe {
            let _ = DestroyPhysicalMonitors(1, &raw mut self.inner);
        }
    }
}

fn physical_monitors(handle: HMONITOR) -> Result<Vec<PhysicalMonitor>, MonitorError> {
    let mut count = 0_u32;
    // SAFETY: count points to writable memory and handle is valid.
    let ok = unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(handle, &raw mut count) };
    if ok == 0 {
        return Err(MonitorError::WindowsApi {
            operation: "counting physical monitors",
        });
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: PHYSICAL_MONITOR is a plain FFI struct; zeroed values are only
    // placeholders filled by the following API before being observed.
    let mut raw = (0..count)
        .map(|_| unsafe { zeroed::<PHYSICAL_MONITOR>() })
        .collect::<Vec<_>>();
    // SAFETY: raw has count entries and handle is valid.
    let ok = unsafe { GetPhysicalMonitorsFromHMONITOR(handle, count, raw.as_mut_ptr()) };
    if ok == 0 {
        return Err(MonitorError::WindowsApi {
            operation: "reading physical monitors",
        });
    }
    Ok(raw
        .into_iter()
        .map(|inner| PhysicalMonitor { inner })
        .collect())
}

fn current_input(monitor: &PhysicalMonitor) -> Result<u32, MonitorError> {
    let mut code_type = 0_i32;
    let mut current = 0_u32;
    let mut maximum = 0_u32;
    // SAFETY: The physical handle is valid and output pointers are writable.
    let ok = unsafe {
        GetVCPFeatureAndVCPFeatureReply(
            monitor.handle(),
            VCP_INPUT_SELECT,
            &raw mut code_type,
            &raw mut current,
            &raw mut maximum,
        )
    };
    if ok == 0 {
        Err(MonitorError::WindowsApi {
            operation: "reading current monitor input",
        })
    } else {
        Ok(current)
    }
}

fn capabilities(monitor: &PhysicalMonitor) -> Result<String, MonitorError> {
    let mut len = 0_u32;
    // SAFETY: len points to writable memory and the physical handle is valid.
    let ok = unsafe { GetCapabilitiesStringLength(monitor.handle(), &raw mut len) };
    if ok == 0 || len == 0 {
        return Err(MonitorError::WindowsApi {
            operation: "reading monitor capabilities length",
        });
    }
    let mut buf = vec![0_u8; len as usize];
    // SAFETY: buf is at least len bytes and handle is valid.
    let ok = unsafe {
        CapabilitiesRequestAndCapabilitiesReply(monitor.handle(), buf.as_mut_ptr().cast(), len)
    };
    if ok == 0 {
        return Err(MonitorError::WindowsApi {
            operation: "reading monitor capabilities",
        });
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
}

fn parse_vcp_60_inputs(capabilities: &str) -> Vec<MonitorInput> {
    let lower = capabilities.to_ascii_lowercase();
    let Some(start) = lower.find("60(") else {
        return Vec::new();
    };
    let rest = &lower[start + 3..];
    let Some(end) = rest.find(')') else {
        return Vec::new();
    };
    let mut values = rest[..end]
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|token| !token.is_empty())
        .filter_map(|token| u32::from_str_radix(token, 16).ok())
        .map(|value| MonitorInput {
            value,
            label: input_label(value),
        })
        .collect::<Vec<_>>();
    values.sort_by_key(|input| input.value);
    values.dedup_by_key(|input| input.value);
    values
}

fn monitor_id(display_name: &str, physical_index: usize, description: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    display_name.hash(&mut hasher);
    physical_index.hash(&mut hasher);
    description.hash(&mut hasher);
    format!(
        "{}:{}:{:016x}",
        display_name.trim_start_matches(r"\\.\\"),
        physical_index,
        hasher.finish()
    )
}

fn wide_description(monitor: &PhysicalMonitor) -> String {
    // PHYSICAL_MONITOR is packed in windows-sys; copy the field out before
    // taking a reference to avoid forming an unaligned reference.
    let description = monitor.inner.szPhysicalMonitorDescription;
    wide_z_to_string(&description)
}

fn wide_z_to_string(chars: &[u16]) -> String {
    let end = chars.iter().position(|c| *c == 0).unwrap_or(chars.len());
    OsString::from_wide(&chars[..end])
        .to_string_lossy()
        .into_owned()
}

fn wide_null(value: &str) -> Vec<u16> {
    OsString::from(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
