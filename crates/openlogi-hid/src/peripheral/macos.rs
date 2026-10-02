use std::collections::BTreeMap;

use objc2_core_foundation::{CFNumber, CFString};
use objc2_io_kit::{IOHIDDevice, IOHIDManager, IORegistryEntryGetRegistryEntryID};
use openlogi_core::peripheral::{EndpointId, PeripheralError, Transport};

use super::Metadata;

thread_local! {
    static MANAGER: objc2_core_foundation::CFRetained<IOHIDManager> = IOHIDManager::new(None, 0);
}

#[expect(
    unsafe_code,
    reason = "IOHIDManagerCopyDevices returns retained IOHIDDevice objects; IOKit fills bounded output buffers"
)]
pub(super) fn metadata(
    _: &[async_hid::Device],
) -> Result<BTreeMap<String, Metadata>, PeripheralError> {
    MANAGER.with(|manager| {
        // SAFETY: None requests enumeration of all devices without a dictionary or an open.
        unsafe {
            manager.set_device_matching(None);
        }
        let Some(devices) = manager.devices() else {
            return Ok(BTreeMap::new());
        };
        let count = usize::try_from(devices.count())
            .map_err(|e| PeripheralError::DiscoveryUnavailable(e.to_string()))?;
        if count > 4096 {
            return Err(PeripheralError::ResourceLimit(
                "HID device enumeration exceeds 4096 nodes".into(),
            ));
        }
        let mut pointers = vec![std::ptr::null(); count];
        // SAFETY: The immutable copied set owns exactly count retained IOHIDDeviceRefs.
        unsafe {
            devices.values(pointers.as_mut_ptr());
        }
        let mut result = BTreeMap::new();
        for pointer in pointers {
            // SAFETY: CopyDevices documents IOHIDDeviceRef elements, retained by devices until this loop ends.
            let device = unsafe { &*pointer.cast::<IOHIDDevice>() };
            let id = registry_id(device.service())?;
            let transport = device
                .property(&CFString::from_str("Transport"))
                .and_then(|p| p.downcast::<CFString>().ok())
                .map(|s| s.to_string());
            let transport = match transport.as_deref() {
                Some("USB") => Some(Transport::UsbHid),
                Some("Bluetooth" | "Bluetooth Low Energy") => Some(Transport::BluetoothHid),
                _ => None,
            };
            let (parent, interface) = if transport == Some(Transport::UsbHid) {
                usb_topology(device.service())?
            } else {
                (None, None)
            };
            result.insert(
                format!("{:?}", async_hid::DeviceId::RegistryEntryId(id)),
                Metadata {
                    transport,
                    interface,
                    max_report_bytes: number(device, "MaxInputReportSize")
                        .and_then(|v| v.try_into().ok()),
                    max_output_report_bytes: number(device, "MaxOutputReportSize")
                        .and_then(|v| v.try_into().ok()),
                    max_feature_report_bytes: number(device, "MaxFeatureReportSize")
                        .and_then(|v| v.try_into().ok()),
                    parent,
                },
            );
        }
        Ok(result)
    })
}

fn number(device: &IOHIDDevice, key: &str) -> Option<i64> {
    device
        .property(&CFString::from_str(key))?
        .downcast_ref::<CFNumber>()?
        .as_i64()
}

// The async-hid macOS callback borrows the caller's feature buffer. A bounded
// broker may cancel its future, so keep the synchronous read and its buffer in
// one blocking task until IOKit returns. Cancellation cannot free that buffer.
pub(super) async fn feature_read(
    device: async_hid::DeviceId,
    id: u8,
    bytes: usize,
    check: impl Fn() -> Result<(), PeripheralError> + Send + 'static,
) -> Result<super::Report, PeripheralError> {
    tokio::task::spawn_blocking(move || {
        check()?;
        let async_hid::DeviceId::RegistryEntryId(registry) = device else {
            return Err(PeripheralError::Unsupported(
                "unknown macOS HID address".into(),
            ));
        };
        let device = open_device(registry)?;
        check()?;
        read_feature(&device.0, id, bytes)
    })
    .await
    .map_err(|e| PeripheralError::ReadFailed(format!("feature worker: {e}")))?
}

struct Service(u32);

impl Service {
    #[expect(
        unsafe_code,
        reason = "IOKit returns an owned parent service through a valid output"
    )]
    fn parent(service: u32) -> Result<Option<Self>, PeripheralError> {
        let mut parent = 0;
        let mut plane = io_name(objc2_io_kit::kIOServicePlane);
        // SAFETY: service is live, the plane is NUL-terminated, and parent is a writable output.
        let status = unsafe {
            objc2_io_kit::IORegistryEntryGetParentEntry(service, &raw mut plane, &raw mut parent)
        };
        if status.cast_unsigned() == objc2_io_kit::kIOReturnNoDevice {
            return Ok(None);
        }
        if status != 0 {
            return Err(PeripheralError::DiscoveryUnavailable(format!(
                "USB parent read failed: {status}"
            )));
        }
        Ok(Some(Self(parent)))
    }

    #[expect(
        unsafe_code,
        reason = "IOKit reads a live service and a NUL-terminated class name"
    )]
    fn conforms_to(&self, class: &std::ffi::CStr) -> bool {
        let mut class = io_name(class);
        // SAFETY: self owns the service; class is a NUL-terminated, full-sized io_name_t buffer.
        unsafe { objc2_io_kit::IOObjectConformsTo(self.0, &raw mut class) }
    }

    #[expect(
        unsafe_code,
        reason = "IOKit returns a retained property for a live service"
    )]
    fn interface(&self) -> Option<u8> {
        let key = CFString::from_str("bInterfaceNumber");
        // SAFETY: self owns the service; key is live and None selects the default CF allocator.
        let value =
            unsafe { objc2_io_kit::IORegistryEntryCreateCFProperty(self.0, Some(&key), None, 0) }?;
        value.downcast_ref::<CFNumber>()?.as_i64()?.try_into().ok()
    }
}

fn io_name(name: &std::ffi::CStr) -> [i8; 128] {
    let mut buffer = [0; 128];
    for (destination, byte) in buffer.iter_mut().zip(name.to_bytes_with_nul()) {
        *destination = byte.cast_signed();
    }
    buffer
}

impl Drop for Service {
    fn drop(&mut self) {
        objc2_io_kit::IOObjectRelease(self.0);
    }
}

#[expect(
    unsafe_code,
    reason = "IOKit fills a registry ID through a valid output"
)]
fn registry_id(service: u32) -> Result<u64, PeripheralError> {
    let mut id = 0;
    // SAFETY: The caller retains service; id is a writable u64.
    let status = unsafe { IORegistryEntryGetRegistryEntryID(service, &raw mut id) };
    if status != 0 {
        return Err(PeripheralError::DiscoveryUnavailable(format!(
            "registry ID read failed: {status}"
        )));
    }
    Ok(id)
}

fn usb_topology(service: u32) -> Result<(Option<EndpointId>, Option<u8>), PeripheralError> {
    let mut parent = Service::parent(service)?;
    let mut interface = None;
    for _ in 0..32 {
        let Some(current) = parent else {
            // Virtual HID devices such as Keyboard Backlight can label their transport USB.
            return Ok((None, interface));
        };
        interface = interface.or_else(|| current.interface());
        if current.conforms_to(c"IOUSBHostDevice") || current.conforms_to(c"IOUSBDevice") {
            return Ok((
                Some(EndpointId(format!("usb:{}", registry_id(current.0)?))),
                interface,
            ));
        }
        parent = Service::parent(current.0)?;
    }
    Err(PeripheralError::ResourceLimit(
        "USB ancestry exceeds 32 nodes".into(),
    ))
}

struct OpenDevice(objc2_core_foundation::CFRetained<IOHIDDevice>);
impl Drop for OpenDevice {
    fn drop(&mut self) {
        self.0.close(0);
    }
}

#[expect(
    unsafe_code,
    reason = "IOKit matching consumes its dictionary and returns a reference owned by Service"
)]
fn open_device(registry: u64) -> Result<OpenDevice, PeripheralError> {
    use objc2_io_kit::{IORegistryEntryIDMatching, IOServiceGetMatchingService};
    // SAFETY: The registry ID comes from the selected live endpoint.
    let matching =
        unsafe { IORegistryEntryIDMatching(registry) }.ok_or(PeripheralError::Offline)?;
    let matching = matching
        .downcast::<objc2_core_foundation::CFDictionary>()
        .map_err(|_| PeripheralError::ReadFailed("invalid registry selector".into()))?;
    // SAFETY: The dictionary has the documented type and is consumed by IOKit.
    let service = unsafe { IOServiceGetMatchingService(0, Some(matching)) };
    if service == 0 {
        return Err(PeripheralError::Offline);
    }
    let service = Service(service);
    let device = IOHIDDevice::new(None, service.0).ok_or(PeripheralError::Offline)?;
    let result = device.open(0);
    if result != 0 {
        return Err(PeripheralError::PermissionDenied(format!(
            "nonexclusive HID open failed: {result}"
        )));
    }
    Ok(OpenDevice(device))
}

#[expect(
    unsafe_code,
    reason = "IOHIDDeviceGetReport uses an owned buffer until the synchronous call returns"
)]
fn read_feature(
    device: &IOHIDDevice,
    id: u8,
    bytes: usize,
) -> Result<super::Report, PeripheralError> {
    let mut buffer = vec![0_u8; bytes];
    buffer[0] = id;
    let data = if id == 0 {
        &mut buffer[1..]
    } else {
        &mut buffer[..]
    };
    if data.is_empty() {
        return Err(PeripheralError::InvalidSettings(
            "feature read requires a payload buffer".into(),
        ));
    }
    let mut length =
        isize::try_from(data.len()).map_err(|e| PeripheralError::ReadFailed(e.to_string()))?;
    let pointer = std::ptr::NonNull::new(data.as_mut_ptr())
        .ok_or_else(|| PeripheralError::ReadFailed("empty feature buffer".into()))?;
    // SAFETY: Both writable buffers outlive this synchronous call; length is their exact capacity.
    let result = unsafe {
        device.report(
            objc2_io_kit::IOHIDReportType::Feature,
            isize::from(id),
            pointer,
            std::ptr::NonNull::from(&mut length),
        )
    };
    if result != 0 {
        return Err(PeripheralError::ReadFailed(format!(
            "feature read failed: {result}"
        )));
    }
    let length = usize::try_from(length).map_err(|e| PeripheralError::ReadFailed(e.to_string()))?;
    let data = data
        .get(..length)
        .ok_or_else(|| PeripheralError::ReadFailed("feature response exceeds the buffer".into()))?;
    super::decode(data, id != 0)
}
