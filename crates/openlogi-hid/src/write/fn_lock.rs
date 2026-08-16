//! HID++ keyboard Fn-lock writes — fn inversion `0x40a3` (multi-host), with
//! the single-host `0x40a2` as fallback.
//!
//! "Fn-lock on" means the F-row sends plain F1–F12 without holding Fn
//! ([`FnInversionState::On`]); off restores the printed media/shortcut
//! functions, with Fn+key producing the F-keys. Multi-host keyboards store the
//! state per Easy-Switch slot, so the `0x40a3` path addresses
//! [`HostIndex::Current`] — the slot the keyboard is talking to right now.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        fn_inversion::{
            FnInversionMultiHostFeature, FnInversionState, FnInversionWithDefaultStateFeature,
        },
        hosts_info::HostIndex,
    },
};
use tracing::debug;

use crate::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// Whether a failure to open the `0x40a3` multi-host feature should trigger
/// the `0x40a2` single-host fallback. Only a missing-`0x40a3` feature
/// qualifies; transport and protocol errors propagate unchanged.
fn is_missing_multi_host(err: &WriteError) -> bool {
    matches!(
        err,
        WriteError::FeatureUnsupported { feature_hex } if *feature_hex == 0x40a3
    )
}

/// Whichever fn-inversion feature the keyboard exposes, normalised onto one
/// setter. Multi-host boards (Easy-Switch) carry `0x40a3`; single-host boards
/// carry `0x40a2`.
enum FnInversion {
    /// `0x40a3 FnInversionForMultiHostDevices`.
    MultiHost(Arc<FnInversionMultiHostFeature>),
    /// `0x40a2 FnInversionWithDefaultState`.
    SingleHost(Arc<FnInversionWithDefaultStateFeature>),
}

impl FnInversion {
    /// Open whichever fn-inversion feature the device exposes. Tries `0x40a3`
    /// first; on a missing-`0x40a3` error (and only that), retries with
    /// `0x40a2`.
    async fn open(device: &mut Device) -> Result<Self, WriteError> {
        match open_feature::<FnInversionMultiHostFeature>(device).await {
            Ok(feature) => Ok(Self::MultiHost(feature)),
            Err(err) if is_missing_multi_host(&err) => {
                let feature = open_feature::<FnInversionWithDefaultStateFeature>(device).await?;
                Ok(Self::SingleHost(feature))
            }
            Err(err) => Err(err),
        }
    }

    /// Write the inversion state (for the current host on `0x40a3`).
    async fn set(&self, state: FnInversionState) -> Result<(), WriteError> {
        match self {
            Self::MultiHost(feature) => {
                feature
                    .set_global_fn_inversion(HostIndex::Current, state)
                    .await
                    .map_err(|e| classify_hidpp_error(e, HidppOperation::WriteFnLock, 0x40a3))?;
            }
            Self::SingleHost(feature) => {
                feature
                    .set_global_fn_inversion(state)
                    .await
                    .map_err(|e| classify_hidpp_error(e, HidppOperation::WriteFnLock, 0x40a2))?;
            }
        }
        Ok(())
    }
}

/// Write the keyboard's Fn-lock state: `true` = F-row sends F1–F12 directly.
pub async fn set_fn_lock(route: &DeviceRoute, on: bool) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        set_fn_lock_on_channel(&channel, index, on).await
    })
    .await
}

/// The Fn-lock write itself, on an already-open channel at HID++ `index`.
pub(super) async fn set_fn_lock_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    on: bool,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let fn_inversion = FnInversion::open(&mut device).await?;
    fn_inversion.set(FnInversionState::from(on)).await?;
    debug!(index, on, "fn-lock written");
    Ok(())
}
