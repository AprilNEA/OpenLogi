//! Shared capability projection for existing protocol owners.

use std::collections::BTreeMap;

use crate::device::{
    Capabilities, DeviceInventory, DeviceKind, LightCapabilities, StandaloneDevice,
};
use crate::device_order::{DeviceIdentity, DeviceStableId};
use crate::hid::{DIRECT_DEVICE_INDEX, DeviceRoute};
pub use openlogi_device_registry::driver::BuiltinDriver;

use super::{
    CameraCapability, Capability, CapabilityEvidence, CapabilityId, CapabilityRecord,
    ConnectionStatus, DescriptorId, DriverId, DriverSelection, DriverSource, Endpoint, EndpointId,
    HidUsage, IdentityEvidence, InputRemapCapability, ModelId, PeripheralRecord, PhysicalDeviceId,
    ScopeKind, SessionId, TargetKind, Transport, WheelCapability,
};

/// Whether a selected implementation retains an existing protocol session owner.
#[must_use]
pub fn owns_protocol(driver: &DriverId) -> bool {
    owns_inventory(driver) || BuiltinDriver::find(driver.as_ref()) == Some(BuiltinDriver::Camera)
}

/// Whether the shared HID inventory owns this implementation's publication.
#[must_use]
pub fn owns_inventory(driver: &DriverId) -> bool {
    matches!(
        BuiltinDriver::find(driver.as_ref()),
        Some(BuiltinDriver::Hidpp | BuiltinDriver::Litra)
    )
}

/// Connection-local address shared by the protocol owner and its IPC clients.
#[must_use]
pub fn endpoint_id(stable: &DeviceStableId) -> EndpointId {
    EndpointId(format!("builtin/{}", stable.runtime_key()))
}

/// Project existing sessions without opening or claiming another protocol channel.
/// With catalog selections, omit unselected routes and preserve exact driver provenance.
/// `None` is reserved for diagnostic and mock snapshots without a running catalog.
#[must_use]
pub fn inventory(
    inventories: &[DeviceInventory],
    standalone: &[StandaloneDevice],
    previous: &[PeripheralRecord],
    generation: u64,
    selected: Option<&[(DeviceRoute, DriverSelection)]>,
) -> Vec<PeripheralRecord> {
    let mut records: Vec<_> = inventories
        .iter()
        .flat_map(|inventory| hidpp(inventory, generation, selected))
        .chain(standalone.iter().filter_map(|device| {
            selected_record(litra(device, generation), Some(&device.route()), selected)
        }))
        .collect();
    for record in &mut records {
        if let Some(old) = previous.iter().find(|old| {
            old.session.endpoint == record.session.endpoint
                && old.model == record.model
                && (record.physical.is_none() || old.physical == record.physical)
                && old.connection == ConnectionStatus::Online
        }) {
            record.session = old.session.clone();
            if record.physical.is_none() {
                record.physical = old.physical.clone();
            }
        }
        record.scopes = vec![if record.physical.is_some() {
            ScopeKind::Physical
        } else {
            ScopeKind::Session
        }];
        for capability in &mut record.capabilities {
            capability.scopes.clone_from(&record.scopes);
        }
        if record.connection != ConnectionStatus::Online {
            for capability in &mut record.capabilities {
                capability.evidence = CapabilityEvidence::LastKnown;
            }
        }
    }
    records
}

fn hidpp(
    inventory: &DeviceInventory,
    generation: u64,
    selected: Option<&[(DeviceRoute, DriverSelection)]>,
) -> Vec<PeripheralRecord> {
    let mut records = Vec::new();
    for device in &inventory.paired {
        let route = DeviceRoute::for_slot(inventory, device.slot);
        let serial = device
            .model_info
            .as_ref()
            .and_then(|m| m.serial_number.as_deref());
        let unit = device.model_info.as_ref().map_or([0; 4], |m| m.unit_id);
        let stable = DeviceStableId::from_parts(route.as_ref(), device.slot, serial, unit);
        let model = device.model_info.as_ref().map_or_else(
            || {
                format!(
                    "pid-{:04x}",
                    device.wpid.unwrap_or(inventory.receiver.product_id)
                )
            },
            crate::device::DeviceModelInfo::model_key,
        );
        let mut record = protocol_record(
            BuiltinDriver::Hidpp,
            &format!("logitech.hidpp.{model}"),
            device
                .codename
                .as_deref()
                .unwrap_or(&inventory.receiver.name),
            device.kind,
            &stable,
            generation,
        );
        record.endpoints.push(Endpoint {
            id: record.session.endpoint.clone(),
            parent: match &stable {
                DeviceStableId::Bolt { receiver_uid, .. } => {
                    Some(EndpointId(format!("receiver:{receiver_uid}")))
                }
                _ => None,
            },
            transport: None,
            vendor_id: inventory.receiver.vendor_id,
            product_id: inventory.receiver.product_id,
            collection: None,
            interface: None,
            report_ids: Vec::new(),
            max_report_bytes: None,
            max_output_report_bytes: None,
            max_feature_report_bytes: None,
        });
        record.physical = device
            .online
            .then(|| DeviceIdentity::from_parts(serial, unit).config_key())
            .flatten()
            .map(|key| PhysicalDeviceId {
                key,
                evidence: IdentityEvidence::Protocol,
            });
        record.capabilities = capabilities(device.kind, device.capabilities, None);
        record.connection = if device.online {
            ConnectionStatus::Online
        } else {
            ConnectionStatus::Offline
        };
        records.extend(selected_record(record, route.as_ref(), selected));
    }
    records
}

fn selected_record(
    mut record: PeripheralRecord,
    route: Option<&DeviceRoute>,
    selected: Option<&[(DeviceRoute, DriverSelection)]>,
) -> Option<PeripheralRecord> {
    if let Some(selected) = selected {
        record.driver = selected
            .iter()
            .find(|(candidate, _)| Some(candidate) == route)?
            .1
            .clone();
    }
    Some(record)
}

fn litra(device: &StandaloneDevice, generation: u64) -> PeripheralRecord {
    let stable = DeviceStableId::from_parts(
        Some(&device.route()),
        DIRECT_DEVICE_INDEX,
        device.serial_number.as_deref(),
        device.unit_id,
    );
    let mut record = protocol_record(
        BuiltinDriver::Litra,
        &format!(
            "usb.{:04x}.{:04x}",
            device.address.vendor_id, device.address.product_id
        ),
        &device.display_name,
        device.kind,
        &stable,
        generation,
    );
    record.endpoints.push(Endpoint {
        id: record.session.endpoint.clone(),
        parent: None,
        transport: None,
        vendor_id: device.address.vendor_id,
        product_id: device.address.product_id,
        collection: Some(HidUsage {
            page: device.address.usage_page,
            usage: device.address.usage_id,
        }),
        interface: None,
        report_ids: Vec::new(),
        max_report_bytes: None,
        max_output_report_bytes: None,
        max_feature_report_bytes: None,
    });
    record.physical = stable.physical_key().map(|key| PhysicalDeviceId {
        key: key.into_string(),
        evidence: IdentityEvidence::VerifiedSerial,
    });
    record.capabilities = capabilities(device.kind, device.capabilities, device.light_capabilities);
    record.connection = if device.online {
        ConnectionStatus::Online
    } else {
        ConnectionStatus::Offline
    };
    record
}

#[expect(
    clippy::expect_used,
    reason = "only compiled prefixes and hexadecimal firmware/USB model keys reach this private constructor"
)]
fn protocol_record(
    driver: BuiltinDriver,
    model: &str,
    name: &str,
    kind: DeviceKind,
    stable: &DeviceStableId,
    generation: u64,
) -> PeripheralRecord {
    PeripheralRecord {
        model: ModelId::try_new(model).expect("canonical protocol model"),
        physical: None,
        session: SessionId {
            endpoint: endpoint_id(stable),
            generation,
        },
        endpoints: Vec::new(),
        name: name.into(),
        kind,
        driver: DriverSelection {
            descriptor: DescriptorId::try_new(format!("{}.{}", driver.id(), model))
                .expect("compiled protocol descriptor"),
            driver: DriverId::try_new(driver.id()).expect("compiled protocol driver"),
            digest: None,
            source: DriverSource::Builtin,
        },
        driver_error: None,
        connection: ConnectionStatus::Online,
        capabilities: Vec::new(),
        scopes: vec![ScopeKind::Physical],
        operations: Vec::new(),
    }
}

/// Adapt native UVC metadata without opening a capture stream or another control session.
///
/// # Panics
/// Panics if compiled identifiers violate the identifier schema.
#[expect(
    clippy::expect_used,
    reason = "compiled IDs and hexadecimal u16 model fields cannot violate identifier syntax or length"
)]
#[must_use]
pub fn camera(camera: crate::camera::Camera, generation: u64) -> PeripheralRecord {
    let model = format!("usb.{:04x}.{:04x}", camera.vendor_id, camera.product_id);
    let id = EndpointId(format!("camera/{}", camera.unique_id));
    let physical = camera
        .serial_number
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|_| PhysicalDeviceId {
            key: camera.config_key(),
            evidence: IdentityEvidence::VerifiedSerial,
        });
    let scopes = physical
        .iter()
        .map(|_| ScopeKind::Physical)
        .chain([ScopeKind::Model, ScopeKind::Session])
        .collect::<Vec<_>>();
    PeripheralRecord {
        model: ModelId::try_new(&model).expect("hexadecimal USB identity"),
        physical,
        session: SessionId {
            endpoint: id.clone(),
            generation,
        },
        endpoints: vec![Endpoint {
            id,
            parent: None,
            transport: Some(Transport::Uvc),
            vendor_id: camera.vendor_id,
            product_id: camera.product_id,
            collection: None,
            interface: None,
            report_ids: Vec::new(),
            max_report_bytes: None,
            max_output_report_bytes: None,
            max_feature_report_bytes: None,
        }],
        name: camera.name.clone(),
        kind: DeviceKind::Camera,
        driver: DriverSelection {
            descriptor: DescriptorId::try_new(format!("{}.{}", BuiltinDriver::Camera.id(), model))
                .expect("compiled camera descriptor"),
            driver: DriverId::try_new(BuiltinDriver::Camera.id()).expect("compiled camera driver"),
            digest: None,
            source: DriverSource::Builtin,
        },
        driver_error: None,
        connection: ConnectionStatus::Online,
        capabilities: vec![CapabilityRecord {
            id: CapabilityId::try_new("camera/main").expect("compiled camera capability"),
            version: 1,
            capability: Capability::Camera(CameraCapability {
                camera,
                state: None,
            }),
            scopes: scopes.clone(),
            unavailable: None,
            evidence: CapabilityEvidence::Declared,
            values: BTreeMap::new(),
        }],
        scopes,
        operations: Vec::new(),
    }
}

/// Normalize legacy probe results once for capability consumers. No session is opened.
///
/// # Panics
/// Panics if compiled capability identifiers violate the identifier schema.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "only fixed capability IDs declared in this function reach the constructor"
)]
pub fn capabilities(
    kind: DeviceKind,
    measured: Option<Capabilities>,
    light: Option<LightCapabilities>,
) -> Vec<CapabilityRecord> {
    let flags = measured.unwrap_or_else(|| Capabilities::presumed_from_kind(kind));
    let mut records = Vec::new();
    let mut add = |id: &str, capability| {
        records.push(CapabilityRecord {
            id: CapabilityId::try_new(id).expect("compiled capability IDs meet the schema"),
            version: 1,
            capability,
            scopes: vec![ScopeKind::Physical],
            unavailable: None,
            evidence: if measured.is_some() || light.is_some() {
                CapabilityEvidence::Probed
            } else {
                CapabilityEvidence::LastKnown
            },
            values: BTreeMap::new(),
        });
    };
    if flags.buttons {
        // HID++ control-table discovery remains with the existing session and editor.
        add(
            "input-remap/main",
            Capability::InputRemap(InputRemapCapability {
                controls: Vec::new(),
                targets: TargetKind::Action,
                per_app: true,
            }),
        );
    }
    if flags.pointer {
        add("pointer/main", Capability::Pointer);
    }
    if flags.lighting {
        add("keyboard-lighting/main", Capability::KeyboardLighting);
    }
    if flags.hires_wheel || flags.scroll_inversion || flags.thumbwheel {
        add(
            "wheel/main",
            Capability::Wheel(WheelCapability {
                resolution: flags.hires_wheel,
                inversion: flags.scroll_inversion,
                horizontal: flags.thumbwheel,
            }),
        );
    }
    if flags.fn_lock {
        add("fn-lock/main", Capability::FnLock);
    }
    if flags.haptic_panel {
        add("haptics/main", Capability::Haptics);
    }
    if let Some(light) = light {
        add("light/main", Capability::Light(light));
    }
    records
}
