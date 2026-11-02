//! HID++ `0x8300 Sidetone` reads and writes: how much of a headset's
//! microphone it plays back into its own earcups.
//!
//! The headset keeps the level across power cycles (measured on a G535), so
//! nothing here is saved or written again on reconnect.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel, device::Device, feature::CreatableFeature,
    feature::sidetone::SidetoneFeature, protocol::v20::Hidpp20Error,
};
use openlogi_core::hid::SidetoneLevel;
use tracing::debug;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};
use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

/// Read the sidetone level of the headset `route` reaches.
pub async fn get_sidetone(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<SidetoneLevel, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let feature = open_sidetone_feature(&channel, index).await?;
        read(&feature).await
    })
    .await
}

/// Write `level` to the headset `route` reaches and return the level it
/// confirms.
pub async fn set_sidetone(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    level: SidetoneLevel,
) -> Result<SidetoneLevel, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let feature = open_sidetone_feature(&channel, index).await?;
        write(&feature, index, level).await
    })
    .await
}

/// Read the sidetone level on an already-open [`SharedChannel`].
pub async fn get_sidetone_on(shared: &SharedChannel) -> Result<SidetoneLevel, WriteError> {
    let feature = open_sidetone_feature(shared.channel(), shared.device_index()).await?;
    read(&feature).await
}

/// Write `level` on an already-open [`SharedChannel`] and return the level
/// the headset confirms.
pub async fn set_sidetone_on(
    shared: &SharedChannel,
    level: SidetoneLevel,
) -> Result<SidetoneLevel, WriteError> {
    let index = shared.device_index();
    let feature = open_sidetone_feature(shared.channel(), index).await?;
    write(&feature, index, level).await
}

async fn read(feature: &SidetoneFeature) -> Result<SidetoneLevel, WriteError> {
    let raw = feature
        .get_sidetone_level()
        .await
        .map_err(|error| classify(error, HidppOperation::ReadSidetone))?;
    parse(raw, HidppOperation::ReadSidetone)
}

/// The confirmation is the write's own echo, not a fresh read: a G535 reads
/// back the previous level for about 200 ms after a write.
async fn write(
    feature: &SidetoneFeature,
    index: u8,
    level: SidetoneLevel,
) -> Result<SidetoneLevel, WriteError> {
    let raw = feature
        .set_sidetone_level(level.into_inner())
        .await
        .map_err(|error| classify(error, HidppOperation::WriteSidetone))?;
    let applied = parse(raw, HidppOperation::WriteSidetone)?;
    debug!(index, %level, %applied, "sidetone written");
    Ok(applied)
}

/// The wrapper already refuses a level above 100, so this cannot fail on a
/// reply it accepted; the conversion still goes through the checked type.
fn parse(raw: u8, operation: HidppOperation) -> Result<SidetoneLevel, WriteError> {
    SidetoneLevel::try_new(raw).map_err(|_| classify(Hidpp20Error::UnsupportedResponse, operation))
}

fn classify(error: Hidpp20Error, operation: HidppOperation) -> WriteError {
    classify_hidpp_error(error, operation, SidetoneFeature::ID)
}

async fn open_sidetone_feature(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<Arc<SidetoneFeature>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    open_feature::<SidetoneFeature>(&mut device).await
}

#[cfg(test)]
mod tests;
