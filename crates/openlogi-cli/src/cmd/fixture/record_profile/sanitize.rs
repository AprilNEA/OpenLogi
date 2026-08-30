//! Relation-preserving synthetic identity projection for semantic profiles.

use openlogi_core::device::{DeviceInventory, StandaloneDevice};

pub(super) fn inventory(inventory: &mut DeviceInventory) {
    if inventory.receiver.unique_id.is_some() {
        let synthetic =
            if openlogi_core::hid::speaks_unifying_protocol(inventory.receiver.product_id) {
                "A0000001"
            } else {
                "OLFXREC000000001"
            };
        inventory.receiver.unique_id = Some(synthetic.to_string());
    }
    for (index, device) in inventory.paired.iter_mut().enumerate() {
        let Some(model) = device.model_info.as_mut() else {
            continue;
        };
        if model.serial_number.is_some() {
            model.serial_number = Some(format!("OLFX{:08}", index + 1));
        }
        if model.unit_id != [0; 4] {
            model.unit_id = synthetic_unit_id(index);
        }
    }
}

pub(super) fn standalone(device: &mut StandaloneDevice) {
    device.address.identity = "openlogi-fixture-raw-1".to_string();
    if device.serial_number.is_some() {
        device.serial_number = Some("OLFX00000001".to_string());
    }
    if device.unit_id != [0; 4] {
        device.unit_id = synthetic_unit_id(0);
    }
}

fn synthetic_unit_id(index: usize) -> [u8; 4] {
    let ordinal = u32::try_from(index + 1)
        .unwrap_or(u32::MAX)
        .min(0x0fff_ffff);
    (0xd000_0000 | ordinal).to_be_bytes()
}
