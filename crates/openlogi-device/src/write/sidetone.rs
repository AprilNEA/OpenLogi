//! Headset sidetone reads and writes: how much of a headset's microphone it
//! plays back into its own earcups. Most headsets use HID++ `0x8300
//! Sidetone`; the PRO X 2 LIGHTSPEED uses `0x0604 HeadsetAudioSidetone`.
//!
//! The headset keeps the level across power cycles (measured on a G535), so
//! nothing here is saved or written again on reconnect.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel, device::Device, feature::CreatableFeature,
    feature::headset_audio_sidetone::HeadsetAudioSidetoneFeature,
    feature::sidetone::SidetoneFeature, protocol::v20::Hidpp20Error,
};
use openlogi_core::hid::SidetoneLevel;
use tracing::debug;

use super::{
    HidppOperation, WriteError, classify_hidpp_error, open_feature, open_feature_with_version,
    with_route,
};
use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::{DeviceRoute, PRO_X_2_PRODUCT_ID};

/// Read the sidetone level of the headset `route` reaches.
pub async fn get_sidetone(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<SidetoneLevel, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let control = open_sidetone_control(&channel, index).await?;
        read(&control).await
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
        let control = open_sidetone_control(&channel, index).await?;
        write(&control, index, level).await
    })
    .await
}

/// Read the sidetone level on an already-open [`SharedChannel`].
pub async fn get_sidetone_on(shared: &SharedChannel) -> Result<SidetoneLevel, WriteError> {
    let control = open_sidetone_control(shared.channel(), shared.device_index()).await?;
    read(&control).await
}

/// Write `level` on an already-open [`SharedChannel`] and return the level
/// the headset confirms.
pub async fn set_sidetone_on(
    shared: &SharedChannel,
    level: SidetoneLevel,
) -> Result<SidetoneLevel, WriteError> {
    let index = shared.device_index();
    let control = open_sidetone_control(shared.channel(), index).await?;
    write(&control, index, level).await
}

/// Behind the PRO X 2's Centurion bridge, `0x0604` sits at this fixed
/// feature index at version 0 (measured in #1285).
const PRO_X_2_SIDETONE_INDEX: u8 = 7;

/// The feature a headset sets its sidetone through.
enum SidetoneControl {
    /// HID++ `0x8300 Sidetone`. A write echoes the level the headset applied.
    Sidetone(Arc<SidetoneFeature>),
    /// HID++ `0x0604 HeadsetAudioSidetone` at `version`, whose reply layout
    /// depends on the version. A write does not echo the level.
    HeadsetAudio {
        feature: Arc<HeadsetAudioSidetoneFeature>,
        version: u8,
    },
}

async fn read(control: &SidetoneControl) -> Result<SidetoneLevel, WriteError> {
    let operation = HidppOperation::ReadSidetone;
    let raw = match control {
        SidetoneControl::Sidetone(feature) => feature
            .get_sidetone_level()
            .await
            .map_err(|error| classify(error, operation, SidetoneFeature::ID))?,
        SidetoneControl::HeadsetAudio { feature, version } => feature
            .get_sidetone_level(*version)
            .await
            .map_err(|error| classify(error, operation, HeadsetAudioSidetoneFeature::ID))?,
    };
    parse(raw, operation, control.feature_id())
}

/// On `0x8300` the confirmation is the write's own echo, not a fresh read: a
/// G535 reads back the previous level for about 200 ms after a write. A
/// `0x0604` write has no echo, so the acknowledged write confirms the level.
async fn write(
    control: &SidetoneControl,
    index: u8,
    level: SidetoneLevel,
) -> Result<SidetoneLevel, WriteError> {
    let operation = HidppOperation::WriteSidetone;
    let applied = match control {
        SidetoneControl::Sidetone(feature) => {
            let raw = feature
                .set_sidetone_level(level.into_inner())
                .await
                .map_err(|error| classify(error, operation, SidetoneFeature::ID))?;
            parse(raw, operation, SidetoneFeature::ID)?
        }
        SidetoneControl::HeadsetAudio { feature, version } => {
            feature
                .set_sidetone_level(*version, level.into_inner())
                .await
                .map_err(|error| classify(error, operation, HeadsetAudioSidetoneFeature::ID))?;
            level
        }
    };
    debug!(index, %level, %applied, "sidetone written");
    Ok(applied)
}

impl SidetoneControl {
    fn feature_id(&self) -> u16 {
        match self {
            Self::Sidetone(_) => SidetoneFeature::ID,
            Self::HeadsetAudio { .. } => HeadsetAudioSidetoneFeature::ID,
        }
    }
}

/// A level above 100 is not a reading; the checked type rejects it.
fn parse(
    raw: u8,
    operation: HidppOperation,
    feature_hex: u16,
) -> Result<SidetoneLevel, WriteError> {
    SidetoneLevel::try_new(raw)
        .map_err(|_| classify(Hidpp20Error::UnsupportedResponse, operation, feature_hex))
}

fn classify(error: Hidpp20Error, operation: HidppOperation, feature_hex: u16) -> WriteError {
    classify_hidpp_error(error, operation, feature_hex)
}

/// Open `0x8300` if the headset has it, else `0x0604`. A headset with neither
/// reports `0x8300` as the missing feature.
async fn open_sidetone_control(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<SidetoneControl, WriteError> {
    if channel.product_id == PRO_X_2_PRODUCT_ID {
        let feature =
            HeadsetAudioSidetoneFeature::new(Arc::clone(channel), index, PRO_X_2_SIDETONE_INDEX);
        return Ok(SidetoneControl::HeadsetAudio {
            feature: Arc::new(feature),
            version: 0,
        });
    }
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    match open_feature::<SidetoneFeature>(&mut device).await {
        Ok(feature) => Ok(SidetoneControl::Sidetone(feature)),
        Err(WriteError::FeatureUnsupported { .. }) => {
            match open_feature_with_version::<HeadsetAudioSidetoneFeature>(&mut device).await {
                Ok((feature, version)) => Ok(SidetoneControl::HeadsetAudio { feature, version }),
                Err(WriteError::FeatureUnsupported { .. }) => Err(WriteError::FeatureUnsupported {
                    feature_hex: SidetoneFeature::ID,
                }),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
