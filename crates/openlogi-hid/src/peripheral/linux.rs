use openlogi_core::peripheral::{EndpointId, PeripheralError};
use std::{fs, os::unix::fs::MetadataExt as _, path::Path};

#[cfg(target_os = "linux")]
pub(super) fn metadata(
    devices: &[async_hid::Device],
) -> Result<std::collections::BTreeMap<String, super::Metadata>, PeripheralError> {
    use super::Metadata;
    use openlogi_core::peripheral::Transport;
    use std::collections::BTreeMap;

    let mut result = BTreeMap::new();
    for device in devices {
        let async_hid::DeviceId::DevPath(path) = &device.id else {
            return Err(PeripheralError::Unsupported(
                "unknown Linux HID address".into(),
            ));
        };
        let Some(name) = path.file_name() else {
            continue;
        };
        let directory = fs::canonicalize(Path::new("/sys/class/hidraw").join(name).join("device"))
            .map_err(failed)?;
        let uevent = fs::read_to_string(directory.join("uevent")).map_err(|e| {
            PeripheralError::DiscoveryUnavailable(format!("{}: {e}", directory.display()))
        })?;
        let transport = uevent
            .lines()
            .find_map(|line| line.strip_prefix("HID_ID="))
            .and_then(|id| id.split(':').next())
            .and_then(|bus| match bus {
                "0003" => Some(Transport::UsbHid),
                "0005" => Some(Transport::BluetoothHid),
                _ => None,
            });
        let (parent, interface) = if transport == Some(Transport::UsbHid) {
            usb_topology(&directory)?
        } else {
            (sysfs_id(&directory)?, None)
        };
        result.insert(
            format!("{:?}", device.id),
            Metadata {
                transport,
                parent: Some(parent),
                interface,
                ..Metadata::default()
            },
        );
    }
    Ok(result)
}

fn sysfs_id(directory: &Path) -> Result<EndpointId, PeripheralError> {
    // The inode changes when sysfs recreates an attachment at the same USB port.
    let inode = fs::metadata(directory).map_err(failed)?.ino();
    Ok(EndpointId(format!("sysfs:{}:{inode}", directory.display())))
}

fn usb_topology(directory: &Path) -> Result<(EndpointId, Option<u8>), PeripheralError> {
    let mut interface = None;
    for ancestor in directory.ancestors().take(32) {
        let uevent = match fs::read_to_string(ancestor.join("uevent")) {
            Ok(uevent) => uevent,
            // Ancestors above the device tree do not have a uevent attribute.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(failed(error)),
        };
        if uevent.lines().any(|line| line == "DEVTYPE=usb_device") {
            return Ok((sysfs_id(ancestor)?, interface));
        }
        if interface.is_none() && uevent.lines().any(|line| line == "DEVTYPE=usb_interface") {
            let value = fs::read_to_string(ancestor.join("bInterfaceNumber")).map_err(failed)?;
            interface = Some(u8::from_str_radix(value.trim(), 16).map_err(failed)?);
        }
    }
    Err(failed(
        "USB HID endpoint has no verified USB device ancestor within 32 nodes",
    ))
}

fn failed(error: impl std::fmt::Display) -> PeripheralError {
    PeripheralError::DiscoveryUnavailable(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_topology_groups_interfaces_and_receiver_children_but_not_identical_units() {
        let root = tempfile::tempdir().unwrap();
        let mut nodes = Vec::new();
        for unit in ["one", "two"] {
            let device = root.path().join(unit);
            fs::create_dir(&device).unwrap();
            fs::write(
                device.join("uevent"),
                "DEVTYPE=usb_device\nPRODUCT=46d/c548/1\n",
            )
            .unwrap();
            for number in [2, 3] {
                let interface = device.join(format!("interface-{number}"));
                let node = interface.join("hid");
                fs::create_dir_all(&node).unwrap();
                fs::write(interface.join("uevent"), "DEVTYPE=usb_interface\n").unwrap();
                fs::write(
                    interface.join("bInterfaceNumber"),
                    format!("{number:02x}\n"),
                )
                .unwrap();
                fs::write(node.join("uevent"), "HID_ID=0003:0000046D:0000C548\n").unwrap();
                nodes.push(node);
            }
        }
        let (first, interface) = usb_topology(&nodes[0]).unwrap();
        assert_eq!(interface, Some(2));
        let (sibling, interface) = usb_topology(&nodes[1]).unwrap();
        assert_eq!(first, sibling);
        assert_eq!(interface, Some(3));
        assert_ne!(first, usb_topology(&nodes[2]).unwrap().0);
        let child = nodes[0].join("paired-child");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("uevent"), "HID_ID=0003:0000046D:00004076\n").unwrap();
        assert_eq!(usb_topology(&child).unwrap().0, first);
        assert!(
            usb_topology(root.path()).is_err(),
            "missing topology must not group devices"
        );
    }
}
