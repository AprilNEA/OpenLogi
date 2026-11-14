//! G-series onboard memory: reads, and the one profile write a user asks
//! for. The capture session owns the mode switches.

use std::{num::NonZeroU16, sync::Arc};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::onboard_profiles::{self, OnboardProfilesFeature},
};
use openlogi_core::hid::{OnboardMode, OnboardProfile, OnboardState, ProfileIndex};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::dpi::{Dpi, get_dpi_info_on_channel};
use super::unit_id::unit_id;
use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// A startup-DPI write bound to one physical mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardDefaultDpi {
    /// The `0x0003` unit ID of the mouse the caller chose. The write refuses
    /// any other device on the route: a receiver slot re-paired after the
    /// choice holds a different mouse, whose flash this must not touch.
    pub unit: [u8; 4],
    /// The onboard profile to write.
    pub profile: ProfileIndex,
    /// Its new startup DPI.
    pub dpi: Dpi,
}

/// The startup DPI of one onboard profile, before and after a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardDefaultDpiChange {
    /// The profile written.
    pub profile: ProfileIndex,
    /// Its startup DPI before the write; `None` if its default slot was
    /// unused.
    pub before: Option<Dpi>,
    /// Its startup DPI read back after the write.
    pub after: Dpi,
}

/// Make `write.dpi` the startup DPI of onboard `write.profile` on the mouse
/// `route` reaches: the DPI it runs at in onboard mode, before any host takes
/// over.
///
/// Before anything else the mouse must report `write.unit` as its `0x0003`
/// unit ID; another device, or one without the feature, is
/// [`WriteError::DeviceNotFound`]. `write.dpi` must be a value the sensor
/// reports as supported. The profile's other DPI slots, buttons and lighting
/// are kept. Nothing is written when the profile already starts at the DPI.
/// The sector is read back after the write, and an active profile is
/// re-selected so the mouse loads the change.
pub async fn set_onboard_default_dpi(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    write: OnboardDefaultDpi,
) -> Result<OnboardDefaultDpiChange, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_onboard_default_dpi_on_channel(&channel, index, write).await
    })
    .await
}

async fn set_onboard_default_dpi_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    OnboardDefaultDpi { unit, profile, dpi }: OnboardDefaultDpi,
) -> Result<OnboardDefaultDpiChange, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    if unit_id(&mut device, HidppOperation::WriteOnboard).await? != Some(unit) {
        debug!(
            index,
            "another device answers on the route; not writing onboard memory"
        );
        return Err(WriteError::DeviceNotFound);
    }

    let capabilities = get_dpi_info_on_channel(channel, index).await?.capabilities;
    let target = NonZeroU16::new(dpi.into_inner())
        .filter(|_| capabilities.contains(dpi))
        .ok_or(WriteError::DpiUnsupported { requested: dpi })?;

    let onboard = open_feature::<OnboardProfilesFeature>(&mut device).await?;
    let read_err = |e| classify_hidpp_error(e, HidppOperation::ReadOnboard, 0x8100);
    let write_err = |e| classify_hidpp_error(e, HidppOperation::WriteOnboard, 0x8100);
    let info = onboard.get_info().await.map_err(read_err)?;
    let entry = onboard
        .read_directory(&info)
        .await
        .map_err(read_err)?
        .into_iter()
        .find(|entry| ProfileIndex::from(entry.index) == profile)
        .ok_or(WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteOnboard,
            feature_hex: 0x8100,
        })?;
    let mut sector = onboard
        .read_profile(&info, entry.sector)
        .await
        .map_err(read_err)?;
    let before = sector.default_dpi().map(|dpi| Dpi::new(dpi.get()));
    if sector.default_dpi() == Some(target) {
        return Ok(OnboardDefaultDpiChange {
            profile,
            before,
            after: dpi,
        });
    }
    sector.set_default_dpi(target);
    onboard.write_profile(&sector).await.map_err(write_err)?;

    let written = onboard
        .read_profile(&info, entry.sector)
        .await
        .map_err(read_err)?;
    if written != sector {
        return Err(match written.default_dpi() {
            Some(actual) if actual != target => WriteError::DpiNotApplied {
                requested: dpi,
                actual: Dpi::new(actual.get()),
            },
            _ => WriteError::UnsupportedResponse {
                operation: HidppOperation::WriteOnboard,
                feature_hex: 0x8100,
            },
        });
    }
    let active = onboard.get_current_profile().await.map_err(read_err)?;
    if active == Some(entry.index) {
        onboard
            .set_current_profile(entry.index)
            .await
            .map_err(write_err)?;
    }
    Ok(OnboardDefaultDpiChange {
        profile,
        before,
        after: dpi,
    })
}

/// Read the onboard memory of the device on `route`.
pub async fn get_onboard(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<OnboardState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_onboard_on_channel(&channel, index).await
    })
    .await
}

/// Read the onboard memory on an open [`SharedChannel`].
pub async fn get_onboard_on(shared: &SharedChannel) -> Result<OnboardState, WriteError> {
    get_onboard_on_channel(shared.channel(), shared.device_index()).await
}

async fn get_onboard_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<OnboardState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let onboard = open_feature::<OnboardProfilesFeature>(&mut device).await?;
    let err = |e| classify_hidpp_error(e, HidppOperation::ReadOnboard, 0x8100);
    let info = onboard.get_info().await.map_err(err)?;
    let mode = match onboard.get_mode().await.map_err(err)? {
        onboard_profiles::OnboardMode::Onboard => OnboardMode::Onboard,
        onboard_profiles::OnboardMode::Host => OnboardMode::Host,
    };
    let active_profile = onboard
        .get_current_profile()
        .await
        .map_err(err)?
        .map(ProfileIndex::from);
    let mut profiles = Vec::new();
    for entry in onboard.read_directory(&info).await.map_err(err)? {
        let name = onboard
            .read_profile_name(&info, entry.sector)
            .await
            .unwrap_or_else(|error| {
                debug!(profile = %entry.index, ?error, "profile name read failed");
                None
            });
        profiles.push(OnboardProfile {
            index: entry.index.into(),
            name,
            enabled: entry.enabled,
        });
    }
    Ok(OnboardState {
        mode,
        active_profile,
        profiles,
    })
}

#[cfg(test)]
mod tests;
