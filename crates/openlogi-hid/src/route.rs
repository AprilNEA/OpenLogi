//! Opening the HID++ channel that reaches a [`DeviceRoute`].
//!
//! [`DeviceRoute`] itself is pure addressing data with no I/O — it lives in
//! `openlogi_core::hid::route` so the GUI can depend on it without linking
//! this crate's transport. This module re-exports it and adds
//! [`open_route_channel`]: both the write path ([`crate::write`]) and the
//! capture session ([`crate::gesture`]) resolve a route to an open channel
//! through it, so the Bolt-vs-direct branch lives in exactly one place.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    receiver::{self, Receiver},
};

pub use openlogi_core::hid::route::{
    BOLT_PIDS, DIRECT_DEVICE_INDEX, DeviceRoute, LIGHTSPEED_PIDS, LOGITECH_VENDOR_ID,
    UNIFYING_PIDS, is_receiver_pid, receiver_display_name, speaks_unifying_protocol,
};

use crate::transport::{enumerate_hidpp_devices, open_hidpp_channel};

/// Enumerate HID++ candidates and open the channel that reaches `route`.
///
/// For a Bolt route this is the receiver channel (the caller addresses the
/// device through its slot via [`DeviceRoute::device_index`]); for a direct
/// route it is the device's own channel. Returns `None` when nothing matching
/// is currently connected.
pub(crate) async fn open_route_channel(
    route: &DeviceRoute,
) -> Result<Option<Arc<HidppChannel>>, async_hid::HidError> {
    if matches!(route, DeviceRoute::RawHid { .. }) {
        return Ok(None);
    }
    let candidates = enumerate_hidpp_devices().await?;
    for dev in candidates {
        // A direct route's vendor/product id is on the unopened `DeviceInfo`
        // (`async_hid::Device` derefs to it), so skip non-matching nodes before
        // paying the ~100ms channel-open cost — otherwise every direct write on
        // a host that also has a Bolt receiver opens the receiver's channel
        // first. The Bolt branch still needs an open channel for `detect`.
        if let DeviceRoute::Direct {
            vendor_id,
            product_id,
        } = route
            && (dev.vendor_id != *vendor_id || dev.product_id != *product_id)
        {
            continue;
        }
        let Some((_, channel)) = open_hidpp_channel(dev).await? else {
            continue;
        };
        match route {
            DeviceRoute::Bolt { receiver_uid, .. } => {
                let Some(Receiver::Bolt(bolt)) = receiver::detect(Arc::clone(&channel)) else {
                    continue;
                };
                if let Ok(uid) = bolt.get_unique_id().await
                    && uid.eq_ignore_ascii_case(receiver_uid)
                {
                    return Ok(Some(channel));
                }
            }
            DeviceRoute::Unifying { receiver_uid, .. } => {
                let Some(Receiver::Unifying(unifying)) = receiver::detect(Arc::clone(&channel))
                else {
                    continue;
                };
                if let Ok(uid) = unifying.get_unique_id().await
                    && uid.eq_ignore_ascii_case(receiver_uid)
                {
                    return Ok(Some(channel));
                }
            }
            DeviceRoute::Direct { .. } => return Ok(Some(channel)),
            DeviceRoute::RawHid { .. } => unreachable!("raw HID route entered HID++ channel path"),
        }
    }
    Ok(None)
}
