//! HID++ keyboard Fn-lock reads and writes — fn inversion `0x40a3`
//! (multi-host), with the single-host `0x40a2` as fallback.
//!
//! Both features expose one `fnInversionState` byte with the meaning the
//! specifications give it: **inversion on** means a bare F-key performs its
//! printed media/shortcut function and Fn+F-key produces the F-key;
//! **inversion off** means a bare F-key is the F-key and Fn+F-key the
//! function. OpenLogi's `fn_lock` setting is the F-keys-first state, so
//! `fn_lock = true` writes inversion *off* — see [`FnLockState`].
//!
//! Multi-host keyboards store the state per Easy-Switch slot. The `0x40a3`
//! spec offers `0xFF` for "the current host", but the MX Keys S firmware
//! ignores writes addressed that way (Solaar carries the same workaround), so
//! the slot is read from `0x1815 HostsInfo` first and addressed explicitly.
//! `0xFF` is used only for a keyboard that has no `0x1815` at all; a keyboard
//! that has it but does not answer fails the operation, since a write
//! addressed to `0xFF` on such a keyboard is exactly the one the firmware
//! drops.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature as _,
        fn_inversion::{
            FnInversionMultiHostFeature, FnInversionState, FnInversionWithDefaultStateFeature,
        },
        hosts_info::{HostIndex, HostsInfoFeature},
    },
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use openlogi_core::hid::FnLockState;

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

/// The firmware inversion state for an Fn-lock setting: Fn-lock on (F-keys
/// first) is inversion *off*.
fn inversion_for(fn_lock: bool) -> FnInversionState {
    if fn_lock {
        FnInversionState::Off
    } else {
        FnInversionState::On
    }
}

/// The Fn-lock setting a firmware inversion state reports.
fn fn_lock_for(inversion: FnInversionState) -> bool {
    matches!(inversion, FnInversionState::Off)
}

/// Whichever fn-inversion feature the keyboard exposes, normalised onto one
/// getter/setter. Multi-host boards (Easy-Switch) carry `0x40a3`; single-host
/// boards carry `0x40a2`.
enum FnInversion {
    /// `0x40a3 FnInversionForMultiHostDevices`, addressed at the slot the
    /// keyboard is talking to.
    MultiHost {
        feature: Arc<FnInversionMultiHostFeature>,
        host: HostIndex,
    },
    /// `0x40a2 FnInversionWithDefaultState`.
    SingleHost(Arc<FnInversionWithDefaultStateFeature>),
}

impl FnInversion {
    /// Open whichever fn-inversion feature the device exposes. Tries `0x40a3`
    /// first; on a missing-`0x40a3` error (and only that), retries with
    /// `0x40a2`. `operation` names the read or write this open serves, for
    /// the error a failed host lookup carries.
    async fn open(device: &mut Device, operation: HidppOperation) -> Result<Self, WriteError> {
        match open_feature::<FnInversionMultiHostFeature>(device).await {
            Ok(feature) => {
                let host = current_host(device, operation).await?;
                Ok(Self::MultiHost { feature, host })
            }
            Err(err) if is_missing_multi_host(&err) => {
                let feature = open_feature::<FnInversionWithDefaultStateFeature>(device).await?;
                Ok(Self::SingleHost(feature))
            }
            Err(err) => Err(err),
        }
    }

    fn feature_hex(&self) -> u16 {
        match self {
            Self::MultiHost { .. } => 0x40a3,
            Self::SingleHost(_) => 0x40a2,
        }
    }

    async fn get(&self) -> Result<FnLockState, WriteError> {
        let (state, default_state) = match self {
            Self::MultiHost { feature, host } => {
                let info = feature.get_global_fn_inversion(*host).await.map_err(|e| {
                    classify_hidpp_error(e, HidppOperation::ReadFnLock, self.feature_hex())
                })?;
                (info.state, info.default_state)
            }
            Self::SingleHost(feature) => {
                let global = feature.get_global_fn_inversion().await.map_err(|e| {
                    classify_hidpp_error(e, HidppOperation::ReadFnLock, self.feature_hex())
                })?;
                (global.state, global.default_state)
            }
        };
        Ok(FnLockState {
            fn_lock: fn_lock_for(state),
            default_fn_lock: fn_lock_for(default_state),
        })
    }

    /// Write the inversion state and return what the keyboard echoes back.
    async fn set(&self, state: FnInversionState) -> Result<FnLockState, WriteError> {
        let (state, default_state) = match self {
            Self::MultiHost { feature, host } => {
                let info = feature
                    .set_global_fn_inversion(*host, state)
                    .await
                    .map_err(|e| {
                        classify_hidpp_error(e, HidppOperation::WriteFnLock, self.feature_hex())
                    })?;
                (info.state, info.default_state)
            }
            Self::SingleHost(feature) => {
                let global = feature.set_global_fn_inversion(state).await.map_err(|e| {
                    classify_hidpp_error(e, HidppOperation::WriteFnLock, self.feature_hex())
                })?;
                (global.state, global.default_state)
            }
        };
        Ok(FnLockState {
            fn_lock: fn_lock_for(state),
            default_fn_lock: fn_lock_for(default_state),
        })
    }
}

/// The Easy-Switch slot the keyboard is currently talking to, read from
/// `0x1815 HostsInfo`. A keyboard without that feature is addressed as
/// `0xFF`, the spec's own "current host" selector and the only one it
/// offers. A keyboard that has the feature but fails the read fails the
/// `operation`: guessing `0xFF` there would send the MX Keys S the very write
/// its firmware ignores, and the echo check would then blame the keyboard
/// for a lookup that never happened.
async fn current_host(
    device: &mut Device,
    operation: HidppOperation,
) -> Result<HostIndex, WriteError> {
    let hosts_info = match open_feature::<HostsInfoFeature>(device).await {
        Ok(hosts_info) => hosts_info,
        Err(WriteError::FeatureUnsupported { feature_hex })
            if feature_hex == HostsInfoFeature::ID =>
        {
            return Ok(HostIndex::Current);
        }
        Err(error) => return Err(error),
    };
    let info = hosts_info
        .get_feature_info()
        .await
        .map_err(|e| classify_hidpp_error(e, operation, HostsInfoFeature::ID))?;
    Ok(info.current_host)
}

/// Read the keyboard's Fn-lock state on `route`.
pub async fn get_fn_lock(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<FnLockState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_fn_lock_on_channel(&channel, index).await
    })
    .await
}

/// Read the keyboard's Fn-lock state on an already-open [`SharedChannel`].
pub async fn get_fn_lock_on(shared: &SharedChannel) -> Result<FnLockState, WriteError> {
    get_fn_lock_on_channel(shared.channel(), shared.device_index()).await
}

async fn get_fn_lock_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<FnLockState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    FnInversion::open(&mut device, HidppOperation::ReadFnLock)
        .await?
        .get()
        .await
}

/// Write the keyboard's Fn-lock state on `route`: `true` = the F-row sends
/// F1–F12 without holding Fn. Returns the state the keyboard reports after
/// the write; a keyboard that did not take it surfaces as
/// [`WriteError::UnsupportedResponse`].
pub async fn set_fn_lock(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    on: bool,
) -> Result<FnLockState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_fn_lock_on_channel(&channel, index, on).await
    })
    .await
}

/// The Fn-lock write itself, on an already-open channel at HID++ `index`.
pub(super) async fn set_fn_lock_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    on: bool,
) -> Result<FnLockState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let fn_inversion = FnInversion::open(&mut device, HidppOperation::WriteFnLock).await?;
    let echoed = fn_inversion.set(inversion_for(on)).await?;
    if echoed.fn_lock != on {
        // The echo is the firmware's word on what it stored; a mismatch is
        // the MX Keys S 0xFF bug, or a keyboard that only pretends to take
        // the write. Surface it rather than log a success nothing observed.
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteFnLock,
            feature_hex: fn_inversion.feature_hex(),
        });
    }
    debug!(index, on, "fn-lock written");
    Ok(echoed)
}

/// Write keyboard Fn-lock on an already-open [`SharedChannel`] — the fast
/// path that skips enumeration and channel setup.
pub async fn set_fn_lock_on(shared: &SharedChannel, on: bool) -> Result<FnLockState, WriteError> {
    set_fn_lock_on_channel(shared.channel(), shared.device_index(), on).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fn_lock_is_the_inverse_of_firmware_inversion() {
        // Spec (0x40a2 / 0x40a3): inversion ON = bare F-key performs the
        // special function. OpenLogi's fn_lock is the F-keys-first state.
        assert_eq!(inversion_for(true), FnInversionState::Off);
        assert_eq!(inversion_for(false), FnInversionState::On);
        for on in [true, false] {
            assert_eq!(fn_lock_for(inversion_for(on)), on);
        }
    }
}
