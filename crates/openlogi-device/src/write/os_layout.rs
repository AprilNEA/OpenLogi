//! Keep a keyboard's `0x4531 MultiPlatform` layout on the host OS — the
//! "keep keyboard in OS layout" setting of Logi Options+.
//!
//! A multi-platform keyboard keeps one platform (key layout) per Easy-Switch
//! slot. Logi Options+ 2.8 (`feature_4531_multi_platform::_update_platform`)
//! picks the platform whose descriptor covers the host OS
//! (`platforms_info::find`), compares it with the platform the current slot
//! holds, and writes it only when they differ. It writes only when the
//! keyboard advertises `SET_HOST_PLATFORM`, the slot is below the host count
//! and the platform below the platform count (`platforms_info::set_platform`).
//! [`keep_os_layout_on`] makes the same decision, and then reads the slot back,
//! because `setHostPlatform` replies with nothing to check.

#[cfg(test)]
mod tests;

use std::cmp::Reverse;
use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature as _,
        hosts_info::HostIndex,
        multi_platform::{
            MultiPlatformCapabilities, MultiPlatformFeature, OsMask, PlatformDescriptor,
        },
    },
};
use tracing::debug;

use crate::SharedChannel;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature};

/// The operating system a keyboard's platform descriptors are matched against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostOs {
    /// The OS, as its `0x4531` descriptor bit.
    pub os: OsMask,
    /// The OS version, or `None` when the host does not report one. An
    /// unknown version is taken to be newer than every bounded range, so only
    /// a descriptor with no upper bound can match it.
    pub version: Option<OsVersion>,
}

impl HostOs {
    /// The OS this build runs on, its version unknown, or `None` on an OS no
    /// `0x4531` descriptor names.
    #[must_use]
    pub const fn current() -> Option<Self> {
        let os = if cfg!(target_os = "macos") {
            OsMask::MACOS
        } else if cfg!(windows) {
            OsMask::WINDOWS
        } else if cfg!(target_os = "linux") {
            OsMask::LINUX
        } else {
            return None;
        };
        Some(Self { os, version: None })
    }
}

/// An OS version as a platform descriptor bounds it: a major version and a
/// revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OsVersion {
    /// Major version.
    pub major: u8,
    /// Revision.
    pub revision: u8,
}

/// What [`keep_os_layout_on`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsLayoutOutcome {
    /// The current slot already holds the host OS's platform.
    AlreadySet {
        /// The platform the slot holds.
        platform_index: u8,
    },
    /// The current slot was switched to the host OS's platform.
    Switched {
        /// The platform the slot held before, `None` when undefined.
        from: Option<u8>,
        /// The platform it holds now.
        to: u8,
    },
    /// The keyboard does not let software set its platform, or reports a
    /// current slot outside its own host count.
    NotSettable,
    /// No platform descriptor covers the host OS.
    NoMatchingPlatform,
    /// The caller withdrew the write before it was sent: the setting was
    /// turned off, or the device disabled, while the job waited.
    Withdrawn,
}

/// The part of a platform descriptor the selection reads, with each end of
/// its version range packed as `major << 8 | revision` (`0` leaves that end
/// open).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Candidate {
    platform: u8,
    descriptor: u8,
    os: OsMask,
    from: u16,
    to: u16,
}

impl From<&PlatformDescriptor> for Candidate {
    fn from(descriptor: &PlatformDescriptor) -> Self {
        Self {
            platform: descriptor.platform_index,
            descriptor: descriptor.descriptor_index,
            os: descriptor.os_mask,
            from: u16::from_be_bytes([descriptor.from_version, descriptor.from_revision]),
            to: u16::from_be_bytes([descriptor.to_version, descriptor.to_revision]),
        }
    }
}

impl Candidate {
    /// Whether this descriptor covers `host`'s OS and version.
    fn covers(self, host: HostOs) -> bool {
        if !self.os.intersects(host.os) {
            return false;
        }
        match host.version {
            Some(version) => {
                let version = u16::from_be_bytes([version.major, version.revision]);
                (self.from == 0 || version >= self.from) && (self.to == 0 || version <= self.to)
            }
            None => self.to == 0,
        }
    }
}

/// The platform for `host`, chosen the way Logi Options+ 2.8 does it
/// (`platforms_info::find`): among the descriptors that cover the host, the
/// one with the highest upper bound wins, then the one with the lowest lower
/// bound, and on a tie the first in platform and descriptor order. A range
/// open at the top has upper bound `0`, so a bounded match ranks before it.
fn select_platform(candidates: impl IntoIterator<Item = Candidate>, host: HostOs) -> Option<u8> {
    candidates
        .into_iter()
        .filter(|candidate| candidate.covers(host))
        .min_by_key(|candidate| {
            (
                Reverse(candidate.to),
                candidate.from,
                candidate.platform,
                candidate.descriptor,
            )
        })
        .map(|candidate| candidate.platform)
}

/// Keep the keyboard's current Easy-Switch slot on `host`'s platform, on an
/// already-open [`SharedChannel`]. A slot already on it is left alone; a
/// write the keyboard does not read back surfaces as
/// [`WriteError::UnsupportedResponse`].
///
/// `still_wanted` is asked before the first request and again just before the
/// write. When it says no, nothing more is sent and the outcome is
/// [`OsLayoutOutcome::Withdrawn`], so a job that waited for the receiver does
/// not override a preference the user turned off in the meantime.
pub async fn keep_os_layout_on(
    shared: &SharedChannel,
    host: HostOs,
    still_wanted: impl Fn() -> bool,
) -> Result<OsLayoutOutcome, WriteError> {
    keep_os_layout_on_channel(shared.channel(), shared.device_index(), host, still_wanted).await
}

async fn keep_os_layout_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    host: HostOs,
    still_wanted: impl Fn() -> bool,
) -> Result<OsLayoutOutcome, WriteError> {
    if !still_wanted() {
        return Ok(OsLayoutOutcome::Withdrawn);
    }
    let classify =
        |e| classify_hidpp_error(e, HidppOperation::WriteOsLayout, MultiPlatformFeature::ID);
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<MultiPlatformFeature>(&mut device).await?;
    let info = feature.get_feature_infos().await.map_err(classify)?;
    let slot = match info.current_host {
        HostIndex::Slot(slot) if slot < info.host_count => HostIndex::Slot(slot),
        _ => return Ok(OsLayoutOutcome::NotSettable),
    };
    if !info
        .capabilities
        .contains(MultiPlatformCapabilities::SET_HOST_PLATFORM)
    {
        return Ok(OsLayoutOutcome::NotSettable);
    }
    let mut descriptors = Vec::with_capacity(usize::from(info.descriptor_count));
    for descriptor_index in 0..info.descriptor_count {
        descriptors.push(
            feature
                .get_platform_descriptor(descriptor_index)
                .await
                .map_err(classify)?,
        );
    }
    let Some(target) = select_platform(descriptors.iter().map(Candidate::from), host)
        .filter(|&platform| platform < info.platform_count)
    else {
        return Ok(OsLayoutOutcome::NoMatchingPlatform);
    };
    let before = feature
        .get_host_platform(slot)
        .await
        .map_err(classify)?
        .platform_index;
    if before == Some(target) {
        return Ok(OsLayoutOutcome::AlreadySet {
            platform_index: target,
        });
    }
    if !still_wanted() {
        return Ok(OsLayoutOutcome::Withdrawn);
    }
    feature
        .set_host_platform(slot, target)
        .await
        .map_err(classify)?;
    let after = feature
        .get_host_platform(slot)
        .await
        .map_err(classify)?
        .platform_index;
    if after != Some(target) {
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteOsLayout,
            feature_hex: MultiPlatformFeature::ID,
        });
    }
    debug!(
        index,
        ?before,
        target,
        "keyboard platform set to the host OS"
    );
    Ok(OsLayoutOutcome::Switched {
        from: before,
        to: target,
    })
}
