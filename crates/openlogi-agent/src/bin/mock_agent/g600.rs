//! A stand-in G600 for the built-in demo: onboard DPI the three G600 calls
//! read and change, refusing what the driver refuses.

use std::sync::Arc;
use std::time::Duration;

use openlogi_core::device::{Capabilities, DeviceKind, RawDeviceAddress, StandaloneDevice};
use openlogi_core::hid::g600::{
    ActiveSelection, DpiSettings, DpiStage, DpiState, G600_DRIVER_ID, G600_NAME, G600_PRODUCT_ID,
    G600_USAGE_ID, G600_USAGE_PAGE, G600Failure, OnboardProfile, ProfileDpi, StageAfterStore,
};
use openlogi_core::hid::{DeviceRoute, LOGITECH_VENDOR_ID, WriteError};
use openlogi_fixture::{
    ProfileDeviceSettings, SyntheticIdentityKind, SyntheticIdentityOrdinal,
    generate_synthetic_identity,
};
use tokio::sync::Mutex;

use super::profile::unsupported_settings;

/// How long a profile write keeps a client waiting, as a real G600's
/// one-second commit does.
pub(super) const PROFILE_WRITE_TIME: Duration = Duration::from_millis(1200);

/// The demo's G600, with a synthetic identity so it collides with no real
/// mouse, and the settings entry that marks it as having no HID++ settings.
pub(super) fn demo_device() -> Result<(StandaloneDevice, ProfileDeviceSettings), String> {
    let ordinal = SyntheticIdentityOrdinal::new(2).map_err(|error| error.to_string())?;
    let identity =
        generate_synthetic_identity(SyntheticIdentityKind::RawHidProfileIdentity, ordinal)
            .as_profile_str()
            .ok_or("a raw-HID synthetic identity is a string")?
            .to_owned();
    let device = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: LOGITECH_VENDOR_ID,
            product_id: G600_PRODUCT_ID,
            usage_page: G600_USAGE_PAGE,
            usage_id: G600_USAGE_ID,
            identity,
        },
        display_name: G600_NAME.to_owned(),
        manufacturer: Some("Logitech".to_owned()),
        serial_number: None,
        unit_id: [0; 4],
        kind: DeviceKind::Mouse,
        online: true,
        capabilities: Some(Capabilities::default()),
        light_capabilities: None,
        driver_id: G600_DRIVER_ID.to_owned(),
        registry_model_id: None,
    };
    let settings = unsupported_settings(device.route());
    Ok((device, settings))
}

/// The onboard DPI of every G600 a profile lists, each behind a lock of its
/// own that a call holds until it is done, as the agent serves one G600
/// request per mouse at a time.
#[derive(Default)]
pub(super) struct MockG600s {
    mice: Vec<(DeviceRoute, Arc<Mutex<DpiState>>)>,
}

impl MockG600s {
    /// One G600 per standalone device the G600 driver owns, each on its
    /// factory-like starting state.
    pub(super) fn for_devices<'a>(devices: impl IntoIterator<Item = &'a StandaloneDevice>) -> Self {
        Self {
            mice: devices
                .into_iter()
                .filter(|device| device.driver_id == G600_DRIVER_ID)
                .map(|device| (device.route(), Arc::new(Mutex::new(starting_state()))))
                .collect(),
        }
    }

    /// The G600 on `route`, to lock for one call.
    pub(super) fn mouse(&self, route: &DeviceRoute) -> Result<Arc<Mutex<DpiState>>, WriteError> {
        self.mice
            .iter()
            .find(|(g600, _)| g600 == route)
            .map(|(_, state)| Arc::clone(state))
            .ok_or_else(|| G600Failure::NotG600Route.into())
    }
}

/// Store `settings` in `profile` of `mouse` and put it on the stage `after`
/// asks for, as the driver does: a stage that cannot follow the write is
/// refused before anything changes, a profile that already holds `settings`
/// answers at once, and any other keeps the caller waiting while the mouse
/// stores it.
pub(super) async fn store_profile(
    mouse: &mut DpiState,
    profile: OnboardProfile,
    settings: &DpiSettings,
    after: StageAfterStore,
) -> Result<(), WriteError> {
    let mut stored = *mouse;
    stored
        .store(profile, settings, after)
        .map_err(G600Failure::from)?;
    if stored.profile(profile) != mouse.profile(profile) {
        tokio::time::sleep(PROFILE_WRITE_TIME).await;
    }
    *mouse = stored;
    Ok(())
}

/// Switch `mouse` to `stage` of its active profile, unless it is disabled
/// there.
pub(super) fn select_stage(mouse: &mut DpiState, stage: DpiStage) -> Result<(), WriteError> {
    let profile = mouse.active.profile;
    if !mouse.profile(profile).is_enabled(stage) {
        return Err(G600Failure::StageDisabled { profile, stage }.into());
    }
    mouse.active.dpi_stage = stage;
    Ok(())
}

/// Profile 1 on its second stage; each profile a different shape, one with
/// a disabled stage, so every editor state shows.
fn starting_state() -> DpiState {
    let stage = |number| DpiStage::from_number(number);
    let profile = |stages, default| ProfileDpi {
        stages,
        default_stage: stage(default),
        shift: Some(400),
    };
    DpiState {
        active: ActiveSelection {
            profile: OnboardProfile::ALL[0],
            dpi_stage: DpiStage::ALL[1],
        },
        profiles: [
            profile([Some(800), Some(1600), Some(3200), None], 2),
            profile([Some(400), Some(800), Some(1200), Some(2400)], 1),
            profile([Some(1200), None, None, None], 1),
        ],
    }
}
