//! Best-effort pairing status for Easy-Switch host slots.

use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::{
        CreatableFeature,
        hosts_info::{HostIndex, HostSlotStatus, HostsInfoFeature},
    },
};
use tracing::debug;

use super::timed_hidpp;

/// Pairing status explicitly reported for one Easy-Switch host slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportedHostSlot {
    /// The device reports that nothing is paired in this slot.
    Empty,
    /// The device reports that this slot is paired.
    Paired,
    /// The device cannot report a trustworthy pairing status for this slot.
    Unknown,
}

/// Resolved access to a keyboard's reportable host-slot state.
#[derive(Clone)]
pub(super) enum ReportedHostSlotReader {
    /// The keyboard explicitly does not expose `HostsInfo`.
    Unsupported,
    /// Feature discovery failed, so support could not be determined safely.
    Unavailable,
    /// The keyboard exposes `HostsInfo` at this runtime feature index.
    Supported(Arc<HostsInfoFeature>),
}

impl ReportedHostSlotReader {
    pub(super) async fn resolve(device: &mut Device) -> Self {
        let feature = timed_hidpp(
            "locating hosts-info feature",
            device.root().get_feature(HostsInfoFeature::ID),
        )
        .await;
        match feature {
            Ok(Some(info)) => Self::Supported(device.add_feature::<HostsInfoFeature>(info.index)),
            Ok(None) => Self::Unsupported,
            Err(error) => {
                debug!(%error, "hosts-info lookup failed; slot validation is unavailable");
                Self::Unavailable
            }
        }
    }

    pub(super) fn is_supported(&self) -> bool {
        matches!(self, Self::Supported(_))
    }

    /// Read one physical host key while the keyboard is reachable.
    ///
    /// `ChangeHost`'s host count includes empty RF channels, and both a
    /// physical host key and `setCurrentHost` can therefore leave for an
    /// unpaired slot. `HostsInfo` is the only source of per-slot pairing
    /// status. Unsupported, timed-out, malformed, or errored reads remain
    /// [`ReportedHostSlot::Unknown`]. A commanded transition treats that as
    /// advisory; an announced transition requires a fresh paired result while
    /// reachable. Physical departure cannot validate the destination slot.
    pub(super) async fn read_one(&self, host: u8) -> ReportedHostSlot {
        match self {
            Self::Supported(feature) => read_host_slot(feature, host).await,
            Self::Unsupported | Self::Unavailable => ReportedHostSlot::Unknown,
        }
    }
}

/// Read one slot while preparing a commanded transition.
pub(super) async fn reported_host_slot(device: &mut Device, host: u8) -> ReportedHostSlot {
    ReportedHostSlotReader::resolve(device)
        .await
        .read_one(host)
        .await
}

async fn read_host_slot(feature: &HostsInfoFeature, host: u8) -> ReportedHostSlot {
    match timed_hidpp(
        "reading host slot status",
        feature.get_host_info(HostIndex::Slot(host)),
    )
    .await
    {
        Ok(slot) if slot.host_index != HostIndex::Slot(host) => {
            debug!(host, returned = ?slot.host_index, "host slot response identified a different slot");
            ReportedHostSlot::Unknown
        }
        Ok(slot) => match slot.status {
            HostSlotStatus::Empty => ReportedHostSlot::Empty,
            HostSlotStatus::Paired => ReportedHostSlot::Paired,
            _ => ReportedHostSlot::Unknown,
        },
        Err(error) => {
            debug!(host, %error, "host slot status is unreadable; treating the slot as unknown");
            ReportedHostSlot::Unknown
        }
    }
}
