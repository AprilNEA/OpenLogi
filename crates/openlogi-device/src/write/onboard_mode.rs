//! HID++ `0x8100` onboard-mode reads and writes: whether the device's stored
//! onboard profile or the host controls DPI, report rate and buttons. A
//! device in onboard mode rejects host DPI writes (the PRO X3 SUPERSTRIKE
//! answers `LogitechInternal`), so switching to host mode is what lets them
//! land.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature as _,
        onboard_profiles::{OnboardMode as HidppOnboardMode, OnboardProfilesFeature},
    },
};
use openlogi_core::hid::OnboardMode;
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// Map OpenLogi's [`OnboardMode`] onto the fork's `0x8100` mode. A `From`
/// impl cannot live anywhere: neither crate depends on the other, and this
/// one owns neither type.
fn mode_to_hidpp(mode: OnboardMode) -> HidppOnboardMode {
    match mode {
        OnboardMode::Onboard => HidppOnboardMode::Onboard,
        OnboardMode::Host => HidppOnboardMode::Host,
    }
}

/// The inverse of [`mode_to_hidpp`]. The fork's mode is `#[non_exhaustive]`:
/// a mode it learns later is a read this layer cannot represent, never a
/// default.
fn mode_from_hidpp(mode: HidppOnboardMode) -> Result<OnboardMode, WriteError> {
    match mode {
        HidppOnboardMode::Onboard => Ok(OnboardMode::Onboard),
        HidppOnboardMode::Host => Ok(OnboardMode::Host),
        _ => Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::ReadOnboardMode,
            feature_hex: OnboardProfilesFeature::ID,
        }),
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

async fn read(feature: &OnboardProfilesFeature) -> Result<OnboardMode, WriteError> {
    let mode = feature.get_onboard_mode().await.map_err(|e| {
        classify_hidpp_error(
            e,
            HidppOperation::ReadOnboardMode,
            OnboardProfilesFeature::ID,
        )
    })?;
    mode_from_hidpp(mode)
}

/// Read `route`'s onboard mode.
pub async fn get_onboard_mode(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<OnboardMode, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        read(&*open(&channel, index).await?).await
    })
    .await
}

/// Read the onboard mode on an already-open [`SharedChannel`].
pub async fn get_onboard_mode_on(shared: &SharedChannel) -> Result<OnboardMode, WriteError> {
    read(&*open(shared.channel(), shared.device_index()).await?).await
}

/// Switch `route` to `mode`. Returns the mode the device reports after the
/// write; a device that did not take it surfaces as
/// [`WriteError::UnsupportedResponse`].
pub async fn set_onboard_mode(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    mode: OnboardMode,
) -> Result<OnboardMode, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_on_channel(&channel, index, mode).await
    })
    .await
}

/// [`set_onboard_mode`] on an already-open [`SharedChannel`].
pub async fn set_onboard_mode_on(
    shared: &SharedChannel,
    mode: OnboardMode,
) -> Result<OnboardMode, WriteError> {
    set_on_channel(shared.channel(), shared.device_index(), mode).await
}

async fn set_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    mode: OnboardMode,
) -> Result<OnboardMode, WriteError> {
    let feature = open(channel, index).await?;
    feature
        .set_onboard_mode(mode_to_hidpp(mode))
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
    if echoed != mode {
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteOnboardMode,
            feature_hex: OnboardProfilesFeature::ID,
        });
    }
    debug!(index, ?mode, "onboard mode written");
    Ok(echoed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboard_mode_round_trips() {
        // mode_from_hidpp is the inverse of mode_to_hidpp.
        for mode in [OnboardMode::Onboard, OnboardMode::Host] {
            assert_eq!(mode_from_hidpp(mode_to_hidpp(mode)), Ok(mode));
        }
        assert_eq!(
            mode_to_hidpp(OnboardMode::Host),
            HidppOnboardMode::Host,
            "host mode is the fork's host mode, not a neighbour"
        );
    }
}
