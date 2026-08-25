//! Opening the HID++ channel that reaches a [`DeviceRoute`].
//!
//! [`DeviceRoute`] itself is pure addressing data with no I/O — it lives in
//! `openlogi_core::hid::route` so the GUI can depend on it without linking
//! this crate's transport. This module re-exports it and adds
//! [`open_route_channel`]: both the write path ([`crate::write`]) and the
//! capture session ([`crate::session::gesture`]) resolve a route to an open channel
//! through it, so the Bolt-vs-direct branch lives in exactly one place.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    receiver::{self, Receiver},
};

pub use openlogi_core::hid::route::{
    DIRECT_DEVICE_INDEX, DeviceRoute, LOGITECH_VENDOR_ID, RECEIVERS, ReceiverBrand,
    ReceiverDescriptor, ReceiverProtocol, find_receiver, is_receiver_pid, receiver_display_name,
    speaks_unifying_protocol,
};

use tracing::warn;

use crate::backend::{BackendError, HidBackend, NodeInfo, RawWriter};
use crate::write::WriteError;

/// Enumerate HID++ candidates and open the channel that reaches `route`.
///
/// For a Bolt route this is the receiver channel (the caller addresses the
/// device through its slot via [`DeviceRoute::device_index`]); for a direct
/// route it is the device's own channel. Returns `None` when nothing matching
/// is currently connected.
pub(crate) async fn open_route_channel(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<Option<Arc<HidppChannel>>, BackendError> {
    if matches!(route, DeviceRoute::RawHid { .. }) {
        return Ok(None);
    }
    for node in backend.enumerate_hidpp().await? {
        // A direct route's vendor/product id is on the unopened node, so skip
        // non-matching ones before paying the ~100ms channel-open cost —
        // otherwise every direct write on a host that also has a Bolt receiver
        // opens the receiver's channel first. The Bolt branch still needs an
        // open channel for `detect`.
        if let DeviceRoute::Direct {
            vendor_id,
            product_id,
        } = route
            && (node.vendor_id != *vendor_id || node.product_id != *product_id)
        {
            continue;
        }
        let Some(channel) = backend.open_hidpp(&node).await? else {
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

/// Open the raw output-report writer for the device `route` reaches, for
/// reports the HID++ wrapper can't model — e.g. the 64-byte `0x12` lighting
/// frames G-series keyboards use. Returns `None` for receiver-slot routes or
/// when no matching node is connected.
///
/// A direct route names one device outright, so the first match wins. A raw
/// route is addressed by its full HID identity tuple, and two nodes answering
/// to the same tuple cannot be told apart — that is an error, not a coin toss.
pub(crate) async fn open_route_writer(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<Option<Box<dyn RawWriter>>, WriteError> {
    let candidates = match route {
        DeviceRoute::Direct { .. } => backend.enumerate_hidpp().await?,
        DeviceRoute::RawHid { .. } => backend.enumerate().await?,
        _ => return Ok(None),
    };
    let mut matched = None;
    for node in candidates {
        if !route_matches_node(route, &node) {
            continue;
        }
        if matches!(route, DeviceRoute::Direct { .. }) {
            return Ok(Some(backend.open_raw_writer(&node).await?));
        }
        if matched.is_some() {
            warn!("multiple raw HID nodes matched one route");
            return Err(WriteError::AmbiguousRawDevice);
        }
        matched = Some(node);
    }
    match matched {
        Some(node) => Ok(Some(backend.open_raw_writer(&node).await?)),
        None => Ok(None),
    }
}

/// Whether `node` is the HID node `route` addresses.
fn route_matches_node(route: &DeviceRoute, node: &NodeInfo) -> bool {
    match route {
        DeviceRoute::Direct {
            vendor_id,
            product_id,
        } => node.vendor_id == *vendor_id && node.product_id == *product_id,
        DeviceRoute::RawHid {
            vendor_id,
            product_id,
            usage_page,
            usage_id,
            identity,
        } => {
            node.vendor_id == *vendor_id
                && node.product_id == *product_id
                && node.usage_page == *usage_page
                && node.usage_id == *usage_id
                && node.identity() == *identity
        }
        DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. } => false,
    }
}
