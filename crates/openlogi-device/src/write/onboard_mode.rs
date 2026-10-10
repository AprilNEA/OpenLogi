//! HID++ `0x8100` onboard-mode reads and writes: whether the device's stored
//! onboard profile or the host controls DPI, report rate and buttons.
//!
//! OpenLogi exposes this as `onboard_profiles`: `true` is
//! [`OnboardMode::Onboard`], `false` is [`OnboardMode::Host`]. A device in
//! onboard mode rejects host DPI writes (the PRO X3 SUPERSTRIKE answers
//! `LogitechInternal`), so switching to host mode is what lets them land.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature as _,
        onboard_profiles::{OnboardMode, OnboardProfilesFeature},
    },
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// The onboard mode an `onboard_profiles` setting selects.
fn mode_for(onboard_profiles: bool) -> OnboardMode {
    if onboard_profiles {
        OnboardMode::Onboard
    } else {
        OnboardMode::Host
    }
}

async fn open(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<Arc<OnboardProfilesFeature>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    open_feature::<OnboardProfilesFeature>(&mut device).await
}

async fn read(feature: &OnboardProfilesFeature) -> Result<bool, WriteError> {
    let mode = feature.get_onboard_mode().await.map_err(|e| {
        classify_hidpp_error(
            e,
            HidppOperation::ReadOnboardMode,
            OnboardProfilesFeature::ID,
        )
    })?;
    Ok(mode == OnboardMode::Onboard)
}

/// Read whether `route`'s onboard profiles are active.
pub async fn get_onboard_profiles(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<bool, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        read(&*open(&channel, index).await?).await
    })
    .await
}

/// Read whether the onboard profiles are active on an already-open
/// [`SharedChannel`].
pub async fn get_onboard_profiles_on(shared: &SharedChannel) -> Result<bool, WriteError> {
    read(&*open(shared.channel(), shared.device_index()).await?).await
}

/// Switch `route` between its onboard profiles (`true`) and host control
/// (`false`). Returns the mode the device reports after the write; a device
/// that did not take it surfaces as [`WriteError::UnsupportedResponse`].
pub async fn set_onboard_profiles(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    onboard_profiles: bool,
) -> Result<bool, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_on_channel(&channel, index, onboard_profiles).await
    })
    .await
}

/// [`set_onboard_profiles`] on an already-open [`SharedChannel`].
pub async fn set_onboard_profiles_on(
    shared: &SharedChannel,
    onboard_profiles: bool,
) -> Result<bool, WriteError> {
    set_on_channel(shared.channel(), shared.device_index(), onboard_profiles).await
}

async fn set_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    onboard_profiles: bool,
) -> Result<bool, WriteError> {
    let feature = open(channel, index).await?;
    feature
        .set_onboard_mode(mode_for(onboard_profiles))
        .await
        .map_err(|e| {
            classify_hidpp_error(
                e,
                HidppOperation::WriteOnboardMode,
                OnboardProfilesFeature::ID,
            )
        })?;
    // The write itself echoes nothing; read the mode back so a device that
    // ignored it is reported instead of logged as a success.
    let echoed = read(&feature).await?;
    if echoed != onboard_profiles {
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteOnboardMode,
            feature_hex: OnboardProfilesFeature::ID,
        });
    }
    debug!(index, onboard_profiles, "onboard mode written");
    Ok(echoed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboard_profiles_on_is_onboard_mode() {
        assert_eq!(mode_for(true), OnboardMode::Onboard);
        assert_eq!(mode_for(false), OnboardMode::Host);
    }
}
