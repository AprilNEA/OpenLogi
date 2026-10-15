//! The IOKit half of the macOS UVC backend: USB device discovery, registry
//! reads, and the `IOUSBDeviceInterface` plug-in.
//!
//! Every handle IOKit hands out is `+1` and has to be released exactly once, on
//! every path out — which is what each wrapper here exists to make automatic.
//! [`IoObject`] releases an `io_object_t`, [`UsbInterface`] releases a plug-in
//! interface, [`SeizedDevice`] closes the seize it opened, and CoreFoundation
//! values arrive as `CFRetained`, whose drop *is* the release. So there is no
//! hand-balanced teardown left to get wrong, and every `unsafe` block in the
//! macOS backend lives in this file.
//!
//! The configuration descriptor leaves here as a plain `&[u8]` borrowed from
//! the open device, so the descriptor parser above is ordinary safe code.

#![expect(
    unsafe_code,
    reason = "IOKit USB plug-in (IOUSBDeviceInterface182) FFI for UVC control transfers"
)]

use std::ffi::c_void;
use std::num::NonZeroU32;
use std::ptr;
use std::ptr::NonNull;

use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType, CFUUID};
use objc2_io_kit::{
    IOCFPlugInInterface, IOCreatePlugInInterfaceForService, IODestroyPlugInInterface,
    IOIteratorNext, IOObjectRelease, IORegistryEntryCreateCFProperty, IOServiceGetMatchingServices,
    IOServiceMatching, IOUSBConfigurationDescriptor, IOUSBConfigurationDescriptorPtr,
    IOUSBDevRequest, IOUSBDeviceInterface182, io_iterator_t, io_object_t, kIOMainPortDefault,
};

/// `kIOReturnSuccess`, which shares its value with `KERN_SUCCESS` and `S_OK`.
const SUCCESS: i32 = 0;

/// A reference to an IOKit object this process owns, released on drop.
///
/// IOKit spells "no object" as `0`, so [`NonZeroU32`] turns that sentinel into
/// `None` rather than a handle that would later be released as if it were real.
pub(super) struct IoObject(NonZeroU32);

impl IoObject {
    /// Adopt a reference IOKit created for us, or `None` for its null object.
    fn adopt(raw: io_object_t) -> Option<Self> {
        NonZeroU32::new(raw).map(Self)
    }

    fn raw(&self) -> io_object_t {
        self.0.get()
    }
}

impl Drop for IoObject {
    fn drop(&mut self) {
        let _ = IOObjectRelease(self.0.get());
    }
}

/// The services an IOKit iterator yields. Each one arrives as its own
/// [`IoObject`], and the iterator handle is released when this is dropped.
///
/// A matching lookup that succeeds with nothing to report hands back IOKit's
/// null iterator, which is held here as `None` and simply yields no services.
pub(super) struct IoServices(Option<IoObject>);

impl Iterator for IoServices {
    type Item = IoObject;

    fn next(&mut self) -> Option<IoObject> {
        IoObject::adopt(IOIteratorNext(self.0.as_ref()?.raw()))
    }
}

/// Every `IOUSBDevice` currently attached.
///
/// # Errors
/// The name of the IOKit call that failed, for a caller that reports it — an
/// empty iterator means "no such device", which is not the same thing.
pub(super) fn usb_devices() -> Result<IoServices, &'static str> {
    // SAFETY: a NUL-terminated literal names the class, and the `+1` dictionary
    // the call creates is adopted by `CFRetained`.
    let matching =
        unsafe { IOServiceMatching(c"IOUSBDevice".as_ptr()) }.ok_or("IOServiceMatching")?;
    // The lookup consumes one reference to the dictionary — which this binding
    // spells by taking it *by value* — so handing it over is its last use.
    let matching = CFRetained::<CFDictionary>::from(&*matching);
    let mut iterator: io_iterator_t = 0;
    // SAFETY: `kIOMainPortDefault` is IOKit's own static, and `iterator` is a
    // live local the call fills in on success.
    let rc = unsafe {
        IOServiceGetMatchingServices(kIOMainPortDefault, Some(matching), &raw mut iterator)
    };
    if rc != SUCCESS {
        return Err("IOServiceGetMatchingServices");
    }
    Ok(IoServices(IoObject::adopt(iterator)))
}

/// One property of a registry entry, as the CoreFoundation type IOKit stored
/// it under. Callers narrow it with `CFRetained::downcast`.
pub(super) fn registry_property(entry: &IoObject, key: &CFString) -> Option<CFRetained<CFType>> {
    // SAFETY: `entry` holds a live reference for the whole call, and `key` is a
    // live CFString — the key IOKit dereferences with no null check of its own.
    // The `+1` container it creates is adopted by `CFRetained`.
    unsafe { IORegistryEntryCreateCFProperty(entry.raw(), Some(key), None, 0) }
}

// IOUSBLib's plug-in UUIDs, as the `CFUUIDGetConstantUUIDWithBytes` macros in
// IOUSBLib.h and IOCFPlugIn.h spell them.
/// `kIOUSBDeviceUserClientTypeID`.
const USB_DEVICE_USER_CLIENT_TYPE_ID: [u8; 16] = [
    0x9d, 0xc7, 0xb7, 0x80, 0x9e, 0xc0, 0x11, 0xd4, 0xa5, 0x4f, 0x00, 0x0a, 0x27, 0x05, 0x28, 0x61,
];
/// `kIOCFPlugInInterfaceID`.
const CF_PLUGIN_INTERFACE_ID: [u8; 16] = [
    0xc2, 0x44, 0xe8, 0x58, 0x10, 0x9c, 0x11, 0xd4, 0x91, 0xd4, 0x00, 0x50, 0xe4, 0xc6, 0x42, 0x6f,
];
/// `kIOUSBDeviceInterfaceID182` — the first revision exposing
/// `USBDeviceOpenSeize`, and the vtable [`IOUSBDeviceInterface182`] describes.
const USB_DEVICE_INTERFACE_ID_182: [u8; 16] = [
    0x15, 0x2f, 0xc4, 0x96, 0x48, 0x91, 0x11, 0xd5, 0x9d, 0x52, 0x00, 0x0a, 0x27, 0x80, 0x1e, 0x86,
];

/// The constant CFUUID for `b`, which CoreFoundation spells one byte per
/// argument.
fn uuid(b: [u8; 16]) -> Option<CFRetained<CFUUID>> {
    CFUUID::constant_uuid_with_bytes(
        None, b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12],
        b[13], b[14], b[15],
    )
}

/// A retained `IOUSBDeviceInterface182`, released on drop.
///
/// The device is not open yet: this is the interface IOKit's USB plug-in hands
/// back, and it answers the read-only queries (vendor id, location id) on its
/// own. Opening it for control transfers is [`UsbInterface::seize`].
pub(super) struct UsbInterface(NonNull<*mut IOUSBDeviceInterface182>);

impl UsbInterface {
    /// Ask IOKit's USB plug-in for `service`'s device interface.
    pub(super) fn open(service: &IoObject) -> Option<Self> {
        let user_client = uuid(USB_DEVICE_USER_CLIENT_TYPE_ID)?;
        let plugin_type = uuid(CF_PLUGIN_INTERFACE_ID)?;
        let device_type = uuid(USB_DEVICE_INTERFACE_ID_182)?;
        let mut plugin: *mut *mut IOCFPlugInInterface = ptr::null_mut();
        let mut score = 0i32;

        // SAFETY: `service` is live for the whole call, the two UUIDs are only
        // borrowed by it, and `plugin`/`score` are live locals it writes.
        let rc = unsafe {
            IOCreatePlugInInterfaceForService(
                service.raw(),
                Some(&user_client),
                Some(&plugin_type),
                &raw mut plugin,
                &raw mut score,
            )
        };
        if rc != SUCCESS {
            return None;
        }
        let plugin = NonNull::new(plugin)?;

        // SAFETY: the plug-in's IUnknown head is live until
        // `IODestroyPlugInInterface` stops and releases it, which happens on
        // every path below — including a missing `QueryInterface` slot. Per the
        // IOKit plug-in ABI the interface pointer itself is the `this`
        // argument, and `device` is a live local the query writes. The
        // interface it yields for `kIOUSBDeviceInterfaceID182` carries its own
        // reference, so it stays valid once the plug-in is gone.
        let device = unsafe {
            let mut device: *mut c_void = ptr::null_mut();
            let queried = (**plugin.as_ptr()).QueryInterface.is_some_and(|query| {
                query(
                    plugin.as_ptr().cast(),
                    device_type.uuid_bytes(),
                    &raw mut device,
                ) == SUCCESS
            });
            let _ = IODestroyPlugInInterface(plugin.as_ptr());
            if !queried {
                return None;
            }
            device
        };
        NonNull::new(device.cast::<*mut IOUSBDeviceInterface182>()).map(Self)
    }

    /// The vtable IOKit installed for this interface.
    fn vtable(&self) -> &IOUSBDeviceInterface182 {
        // SAFETY: `self.0` is the interface pointer IOKit returned — valid
        // until `Release`, which only `Drop` calls — and a COM-style interface
        // pointer points at its own vtable.
        unsafe { &**self.0.as_ptr() }
    }

    /// The `this` argument every vtable entry takes: per the IOKit plug-in ABI
    /// that is the interface pointer itself, not the vtable it points at.
    fn this(&self) -> *mut c_void {
        self.0.as_ptr().cast()
    }

    /// The device's USB vendor id, without opening it.
    pub(super) fn vendor_id(&self) -> Option<u16> {
        let get = self.vtable().GetDeviceVendor?;
        let mut vendor = 0u16;
        // SAFETY: `get` is this interface's own vtable entry, called with the
        // interface as `this`, and it writes only the local `vendor`.
        (unsafe { get(self.this(), &raw mut vendor) } == SUCCESS).then_some(vendor)
    }

    /// The device's USB location id, without opening it.
    pub(super) fn location_id(&self) -> Option<u32> {
        let get = self.vtable().GetLocationID?;
        let mut location = 0u32;
        // SAFETY: as [`Self::vendor_id`]; writes only the local `location`.
        (unsafe { get(self.this(), &raw mut location) } == SUCCESS).then_some(location)
    }

    /// Open the device for control transfers.
    ///
    /// Seize rather than a plain open, so a transfer lands even while the
    /// kernel video driver holds the camera for streaming — in this app or
    /// another one.
    pub(super) fn seize(self) -> Option<SeizedDevice> {
        let open = self.vtable().USBDeviceOpenSeize?;
        // SAFETY: as [`Self::vendor_id`]; takes no out-parameter.
        if unsafe { open(self.this()) } != SUCCESS {
            return None;
        }
        Some(SeizedDevice(self))
    }
}

impl Drop for UsbInterface {
    fn drop(&mut self) {
        let Some(release) = self.vtable().Release else {
            return;
        };
        // SAFETY: this balances the one reference `QueryInterface` handed over
        // in `open`, and `Drop` runs exactly once.
        unsafe { release(self.this()) };
    }
}

/// A [`UsbInterface`] opened with `USBDeviceOpenSeize`. Closes on drop, after
/// which the interface it wraps releases itself.
pub(super) struct SeizedDevice(UsbInterface);

impl SeizedDevice {
    /// Send a request on the device's default control pipe, reporting whether
    /// the device answered it (a camera NAKs controls it does not implement).
    ///
    /// The pipe is not owned by the streaming driver, so this works while the
    /// camera is live.
    #[must_use]
    pub(super) fn control_request(&self, request: &mut IOUSBDevRequest) -> bool {
        let Some(device_request) = self.0.vtable().DeviceRequest else {
            return false;
        };
        // SAFETY: the device is open for exclusive access, which is what
        // `DeviceRequest` requires. It reads `request` and writes through the
        // `pData` pointer inside it, which the caller owns for the whole call.
        unsafe { device_request(self.0.this(), &raw mut *request) == SUCCESS }
    }

    /// How many configurations the device's descriptor advertises.
    pub(super) fn configuration_count(&self) -> Option<u8> {
        let get = self.0.vtable().GetNumberOfConfigurations?;
        let mut count = 0u8;
        // SAFETY: an open device's vtable entry, writing only the local.
        (unsafe { get(self.0.this(), &raw mut count) } == SUCCESS).then_some(count)
    }

    /// Configuration descriptor `index` as the blob IOKit cached for the
    /// device, bounded by the `wTotalLength` in its own header.
    ///
    /// Borrowed from the open device, so it cannot outlive the seize.
    pub(super) fn configuration_descriptor(&self, index: u8) -> Option<&[u8]> {
        let get = self.0.vtable().GetConfigurationDescriptorPtr?;
        let mut header: IOUSBConfigurationDescriptorPtr = ptr::null_mut();
        // SAFETY: an open device's vtable entry, writing only the local
        // `header`. On success it hands back IOUSBLib's cached copy of the
        // configuration descriptor, which stays put for as long as the device
        // is open — hence the borrow of `self`. IOUSBLib sizes that copy from
        // the `wTotalLength` read here, so the blob is exactly that many bytes;
        // the USB spec's nine-byte configuration header is the lower bound
        // checked before the slice is built, and `wTotalLength` is read by
        // value because the descriptor is `#[repr(packed)]`.
        unsafe {
            if get(self.0.this(), index, &raw mut header) != SUCCESS {
                return None;
            }
            let header = NonNull::new(header)?;
            let total = usize::from((*header.as_ptr()).wTotalLength);
            (total >= size_of::<IOUSBConfigurationDescriptor>())
                .then(|| std::slice::from_raw_parts(header.as_ptr().cast::<u8>(), total))
        }
    }
}

impl Drop for SeizedDevice {
    fn drop(&mut self) {
        let Some(close) = self.0.vtable().USBDeviceClose else {
            return;
        };
        // SAFETY: this balances the `USBDeviceOpenSeize` that built `self`, and
        // `Drop` runs exactly once — before the inner interface releases.
        unsafe { close(self.0.this()) };
    }
}
