//! Relation-preserving synthetic identity projection for semantic profiles.

use anyhow::{Result, anyhow};
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_device::fixture::{
    SyntheticIdentityKind, SyntheticIdentityOrdinal, SyntheticIdentityValue,
    generate_synthetic_identity,
};

pub(super) fn inventory(inventory: &mut DeviceInventory) -> Result<()> {
    if inventory.receiver.unique_id.is_some() {
        let kind = if openlogi_core::hid::speaks_unifying_protocol(inventory.receiver.product_id) {
            SyntheticIdentityKind::UnifyingReceiverRoute
        } else {
            SyntheticIdentityKind::BoltReceiverUid
        };
        inventory.receiver.unique_id = Some(profile_identity(kind, ordinal(0)?)?);
    }
    for (index, device) in inventory.paired.iter_mut().enumerate() {
        let Some(model) = device.model_info.as_mut() else {
            continue;
        };
        let ordinal = ordinal(index)?;
        if model.serial_number.is_some() {
            model.serial_number = Some(profile_identity(
                SyntheticIdentityKind::DeviceSerialNumber,
                ordinal,
            )?);
        }
        if model.unit_id != [0; 4] {
            model.unit_id = unit_id(ordinal)?;
        }
    }
    Ok(())
}

pub(super) fn standalone(device: &mut StandaloneDevice) -> Result<()> {
    let ordinal = ordinal(0)?;
    device.address.identity =
        profile_identity(SyntheticIdentityKind::RawHidProfileIdentity, ordinal)?;
    if device.serial_number.is_some() {
        device.serial_number = Some(profile_identity(
            SyntheticIdentityKind::DeviceSerialNumber,
            ordinal,
        )?);
    }
    if device.unit_id != [0; 4] {
        device.unit_id = unit_id(ordinal)?;
    }
    Ok(())
}

fn ordinal(index: usize) -> Result<SyntheticIdentityOrdinal> {
    let value = u16::try_from(index.saturating_add(1))
        .map_err(|_| anyhow!("captured profile has too many identities to sanitize"))?;
    SyntheticIdentityOrdinal::new(value)
        .map_err(|_| anyhow!("captured profile has too many identities to sanitize"))
}

fn profile_identity(
    kind: SyntheticIdentityKind,
    ordinal: SyntheticIdentityOrdinal,
) -> Result<String> {
    generate_synthetic_identity(kind, ordinal)
        .as_profile_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("fixture identity policy returned the wrong representation"))
}

fn unit_id(ordinal: SyntheticIdentityOrdinal) -> Result<[u8; 4]> {
    match generate_synthetic_identity(SyntheticIdentityKind::DeviceUnitId, ordinal) {
        SyntheticIdentityValue::DeviceUnitId(value) => Ok(value),
        _ => Err(anyhow!(
            "fixture identity policy returned the wrong representation"
        )),
    }
}
