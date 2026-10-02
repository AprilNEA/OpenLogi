//! Explicit executable bindings. Existing protocol owners keep their sessions.

use openlogi_core::{
    camera::Camera,
    device::{DeviceInventory, StandaloneDevice},
    peripheral::{ControlId, PeripheralError, PeripheralRecord, Transport, builtin},
};
use openlogi_device_registry::{driver::BuiltinDriver, native_remap::NATIVE_REMAP_DEVICES};
use openlogi_plugin::{descriptor::Descriptor, selector::Selector};

struct Inventory<'a> {
    hidpp: &'a [DeviceInventory],
    standalone: &'a [StandaloneDevice],
    previous: &'a [PeripheralRecord],
    generation: u64,
    selected: &'a [(
        openlogi_hid::DeviceRoute,
        openlogi_core::peripheral::DriverSelection,
    )],
}

enum Adapter {
    Inventory(fn(&Inventory<'_>) -> Vec<PeripheralRecord>),
    Camera(fn(Camera, u64) -> PeripheralRecord),
    NativeRemap(fn() -> Result<Vec<Descriptor>, PeripheralError>),
}

const BINDINGS: &[(BuiltinDriver, Adapter)] = &[
    (
        BuiltinDriver::Hidpp,
        Adapter::Inventory(|input| {
            builtin::inventory(
                input.hidpp,
                &[],
                input.previous,
                input.generation,
                Some(input.selected),
            )
        }),
    ),
    (
        BuiltinDriver::Litra,
        Adapter::Inventory(|input| {
            builtin::inventory(
                &[],
                input.standalone,
                input.previous,
                input.generation,
                Some(input.selected),
            )
        }),
    ),
    (BuiltinDriver::Camera, Adapter::Camera(builtin::camera)),
    (
        BuiltinDriver::NativeRemap,
        Adapter::NativeRemap(|| {
            NATIVE_REMAP_DEVICES
                .iter()
                .map(|device| {
                    Descriptor::native(device)
                        .map_err(|error| PeripheralError::InvalidSettings(error.to_string()))
                })
                .collect()
        }),
    ),
];

pub(crate) fn inventory(
    hidpp: &[DeviceInventory],
    standalone: &[StandaloneDevice],
    previous: &[PeripheralRecord],
    generation: u64,
    selected: &[(
        openlogi_hid::DeviceRoute,
        openlogi_core::peripheral::DriverSelection,
    )],
) -> Vec<PeripheralRecord> {
    let input = Inventory {
        hidpp,
        standalone,
        previous,
        generation,
        selected,
    };
    BINDINGS
        .iter()
        .filter_map(|(_, adapter)| match adapter {
            Adapter::Inventory(project) => Some(project(&input)),
            _ => None,
        })
        .flatten()
        .collect()
}

pub(super) fn descriptors() -> Result<Vec<Descriptor>, PeripheralError> {
    let mut descriptors = Vec::new();
    for (_, adapter) in BINDINGS {
        if let Adapter::NativeRemap(load) = adapter {
            descriptors.extend(load()?);
        }
    }
    for device in openlogi_device_registry::litra::LITRA_DEVICES {
        descriptors.push(
            Descriptor::protocol(
                BuiltinDriver::Litra,
                Selector {
                    role: ControlId::try_new("controls").map_err(invalid)?,
                    transport: Transport::UsbHid,
                    vendor_id: device.vendor_id,
                    product_id: device.product_id,
                    usage_page: Some(device.usage_page),
                    usage: Some(device.usage_id),
                    interface: None,
                    report_id: None,
                },
            )
            .map_err(invalid)?,
        );
    }
    Ok(descriptors)
}

pub(super) fn observed_descriptors(
    endpoints: &[openlogi_hid::peripheral::DiscoveredEndpoint],
    cameras: &[Camera],
) -> Result<Vec<Descriptor>, PeripheralError> {
    let mut descriptors = descriptors()?;
    for endpoint in endpoints {
        if endpoint.protocol_driver() != Some(BuiltinDriver::Hidpp) {
            continue;
        }
        let facts = &endpoint.endpoint;
        let Some(transport) = facts.transport else {
            continue;
        };
        descriptors.push(
            Descriptor::protocol(
                BuiltinDriver::Hidpp,
                Selector {
                    role: ControlId::try_new("controls").map_err(invalid)?,
                    transport,
                    vendor_id: facts.vendor_id,
                    product_id: facts.product_id,
                    usage_page: facts.collection.map(|c| c.page),
                    usage: facts.collection.map(|c| c.usage),
                    interface: None,
                    report_id: None,
                },
            )
            .map_err(invalid)?,
        );
    }
    for camera in cameras {
        descriptors.push(
            Descriptor::protocol(
                BuiltinDriver::Camera,
                Selector {
                    role: ControlId::try_new("camera").map_err(invalid)?,
                    transport: Transport::Uvc,
                    vendor_id: camera.vendor_id,
                    product_id: camera.product_id,
                    usage_page: None,
                    usage: None,
                    interface: None,
                    report_id: None,
                },
            )
            .map_err(invalid)?,
        );
    }
    Ok(descriptors)
}

pub(super) fn validate(descriptor: &Descriptor) -> Option<Result<(), PeripheralError>> {
    let (_, adapter) = BINDINGS
        .iter()
        .find(|(driver, _)| driver.id() == descriptor.driver().id.as_ref())?;
    Some(match adapter {
        Adapter::NativeRemap(_) => descriptor
            .native_parameters()
            .map(|_| ())
            .map_err(|error| PeripheralError::InvalidSettings(error.to_string())),
        Adapter::Inventory(_) | Adapter::Camera(_) => descriptor
            .protocol_parameters()
            .map(|_| ())
            .map_err(invalid),
    })
}

fn invalid(error: impl std::fmt::Display) -> PeripheralError {
    PeripheralError::InvalidSettings(error.to_string())
}

#[expect(
    clippy::expect_used,
    reason = "the compiled binding table contains exactly one camera adapter"
)]
pub(super) fn camera(camera: Camera, generation: u64) -> PeripheralRecord {
    let project = BINDINGS
        .iter()
        .find_map(|(_, adapter)| match adapter {
            Adapter::Camera(project) => Some(*project),
            _ => None,
        })
        .expect("compiled camera adapter");
    project(camera, generation)
}
