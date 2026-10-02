use std::collections::BTreeMap;

use openlogi_core::peripheral::{EndpointId, PeripheralError, Transport};
use windows_sys::Win32::Devices::{
    DeviceAndDriverInstallation::{
        CM_Get_DevNode_PropertyW, CM_Get_Device_Interface_PropertyW, CM_Get_Parent,
        CM_Locate_DevNodeW, CR_NO_SUCH_DEVNODE, CR_NO_SUCH_VALUE, CR_SUCCESS,
    },
    Properties::{
        DEVPKEY_Device_ContainerId, DEVPKEY_Device_EnumeratorName, DEVPKEY_Device_InstanceId,
        DEVPROP_TYPE_GUID, DEVPROP_TYPE_STRING,
    },
};

use super::Metadata;

pub(super) fn metadata(
    devices: &[async_hid::Device],
) -> Result<BTreeMap<String, Metadata>, PeripheralError> {
    let mut result = BTreeMap::new();
    for device in devices {
        let async_hid::DeviceId::UncPath(path) = &device.id else {
            return Err(PeripheralError::Unsupported(
                "unknown Windows HID address".into(),
            ));
        };
        let mut path = path.to_vec();
        path.push(0);
        let node = device_node(&path)?;
        let transport = transport(node)?;
        let parent = Some(container(node)?);
        let limits = crate::transport::report_capabilities(&path).map_err(failed)?;
        result.insert(
            format!("{:?}", device.id),
            Metadata {
                parent,
                transport,
                interface: None,
                max_report_bytes: Some(u32::from(limits.InputReportByteLength)),
                max_output_report_bytes: Some(u32::from(limits.OutputReportByteLength)),
                max_feature_report_bytes: Some(u32::from(limits.FeatureReportByteLength)),
            },
        );
    }
    Ok(result)
}

#[expect(
    unsafe_code,
    reason = "Configuration Manager fills a correctly sized and aligned GUID"
)]
fn container(node: u32) -> Result<EndpointId, PeripheralError> {
    let mut container = windows_sys::core::GUID::default();
    let mut bytes = u32::try_from(std::mem::size_of_val(&container)).map_err(failed)?;
    let mut kind = 0;
    // SAFETY: The GUID output is aligned, initialized, and has the supplied byte capacity.
    let status = unsafe {
        CM_Get_DevNode_PropertyW(
            node,
            std::ptr::from_ref(&DEVPKEY_Device_ContainerId),
            &raw mut kind,
            (&raw mut container).cast(),
            &raw mut bytes,
            0,
        )
    };
    if status != CR_SUCCESS || kind != DEVPROP_TYPE_GUID {
        return Err(failed(format!("device container: {status}")));
    }
    let id = (u128::from(container.data1) << 96)
        | (u128::from(container.data2) << 80)
        | (u128::from(container.data3) << 64)
        | u128::from(u64::from_be_bytes(container.data4));
    if id == 0 {
        return Err(failed("device container is unavailable"));
    }
    // Plug and Play containers group a physical device's interfaces. This ID is never persisted as user identity.
    Ok(EndpointId(format!("container:{id:032x}")))
}

#[expect(
    unsafe_code,
    reason = "Configuration Manager fills bounded UTF-16 buffers and a devnode output"
)]
fn device_node(path: &[u16]) -> Result<u32, PeripheralError> {
    let mut instance = [0_u16; 512];
    let mut bytes = 1024_u32;
    let mut kind = 0;
    // SAFETY: path is NUL-terminated; instance is an aligned writable buffer with the supplied byte size.
    let result = unsafe {
        CM_Get_Device_Interface_PropertyW(
            path.as_ptr(),
            std::ptr::from_ref(&DEVPKEY_Device_InstanceId),
            &raw mut kind,
            instance.as_mut_ptr().cast(),
            &raw mut bytes,
            0,
        )
    };
    if result != CR_SUCCESS || kind != DEVPROP_TYPE_STRING {
        return Err(failed(format!("interface instance ID: {result}")));
    }
    let mut node = 0;
    // SAFETY: The documented string property is NUL-terminated; node is a valid output.
    let result = unsafe { CM_Locate_DevNodeW(&raw mut node, instance.as_ptr(), 0) };
    if result != CR_SUCCESS {
        return Err(failed(format!("locate devnode: {result}")));
    }
    Ok(node)
}

fn transport(mut node: u32) -> Result<Option<Transport>, PeripheralError> {
    for _ in 0..32 {
        match enumerator(node)?.as_deref() {
            Some("USB") => return Ok(Some(Transport::UsbHid)),
            Some("BTHENUM" | "BTHLEDEVICE" | "BTHLE") => return Ok(Some(Transport::BluetoothHid)),
            _ => {}
        }
        let Some(next) = parent(node)? else {
            return Ok(None);
        };
        node = next;
    }
    Err(PeripheralError::ResourceLimit(
        "device ancestry exceeds 32 nodes".into(),
    ))
}

#[expect(
    unsafe_code,
    reason = "Configuration Manager reads a bounded documented string property"
)]
fn enumerator(node: u32) -> Result<Option<String>, PeripheralError> {
    let mut buffer = [0_u16; 256];
    let mut bytes = 512_u32;
    let mut kind = 0;
    // SAFETY: All output buffers are initialized, aligned, and have their exact byte capacity.
    let result = unsafe {
        CM_Get_DevNode_PropertyW(
            node,
            std::ptr::from_ref(&DEVPKEY_Device_EnumeratorName),
            &raw mut kind,
            buffer.as_mut_ptr().cast(),
            &raw mut bytes,
            0,
        )
    };
    if result == CR_NO_SUCH_VALUE {
        return Ok(None);
    }
    if result != CR_SUCCESS || kind != DEVPROP_TYPE_STRING {
        return Err(failed(format!("device enumerator: {result}")));
    }
    let end = buffer
        .iter()
        .position(|c| *c == 0)
        .ok_or_else(|| failed("unterminated device enumerator"))?;
    String::from_utf16(&buffer[..end])
        .map(|s| Some(s.to_ascii_uppercase()))
        .map_err(failed)
}

#[expect(
    unsafe_code,
    reason = "Configuration Manager writes one parent devnode to a valid output"
)]
fn parent(node: u32) -> Result<Option<u32>, PeripheralError> {
    let mut parent = 0;
    // SAFETY: node came from Configuration Manager; parent is a writable output.
    match unsafe { CM_Get_Parent(&raw mut parent, node, 0) } {
        CR_SUCCESS => Ok(Some(parent)),
        CR_NO_SUCH_DEVNODE => Ok(None),
        code => Err(failed(format!("device parent: {code}"))),
    }
}

fn failed(error: impl std::fmt::Display) -> PeripheralError {
    PeripheralError::DiscoveryUnavailable(error.to_string())
}
