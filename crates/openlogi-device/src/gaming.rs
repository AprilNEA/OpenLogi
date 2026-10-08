//! Gaming memory inspection, independent of the host and the desktop UI.
//!
//! Reading never switches the device to onboard mode. Backups retain every
//! sector, including unknown bytes, so later edits need not recreate profiles.

use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::onboard_profiles::{OnboardMode, OnboardProfilesFeature},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    DeviceRoute,
    backend::{BackendError, HidBackend},
    channel::route::open_route_channel,
    write::{WriteError, open_feature},
};

pub mod profile;

#[cfg(test)]
mod tests;

/// Failure to inspect or validate gaming memory.
#[derive(Debug, Error)]
pub enum GamingError {
    /// Native transport failed.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// Resolving a device or feature failed.
    #[error(transparent)]
    Device(#[from] WriteError),
    /// Firmware rejected a protocol operation.
    #[error(transparent)]
    Protocol(#[from] hidpp::protocol::v20::Hidpp20Error),
    /// Data cannot be safely interpreted by this implementation.
    #[error("unsupported or invalid gaming data: {0}")]
    Invalid(String),
}

/// Versioned local backup, deliberately separate from the agent IPC protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamingBackup {
    /// Backup file schema, currently 1.
    pub schema_version: u8,
    /// Device route on which the backup was captured.
    pub route: DeviceRoute,
    /// Memory format advertised by the firmware.
    pub memory_model: u8,
    /// Profile layout advertised by the firmware.
    pub profile_format: u8,
    /// Macro layout advertised by the firmware.
    pub macro_format: u8,
    /// Writable profile count.
    pub profile_count: u8,
    /// Physical button count.
    pub button_count: u8,
    /// Bytes in each sector, including its checksum.
    pub sector_size: u16,
    /// Execution mode at capture time (1 onboard, 2 host).
    pub mode: u8,
    /// Active one-based profile sector at capture time.
    pub active_profile: u16,
    /// User memory only, indexed by physical sector number.
    pub sectors: Vec<Vec<u8>>,
}

impl GamingBackup {
    /// Check the supported G502 X layout before exposing editable fields.
    pub fn validate_g502x(&self) -> Result<(), GamingError> {
        if self.route
            != (DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc098,
            })
            || self.schema_version != 1
            || self.memory_model != 1
            || ![3, 5].contains(&self.profile_format)
            || self.macro_format != 1
            || ![255, 256].contains(&self.sector_size)
            || !(1..=16).contains(&self.button_count)
            || self.profile_count == 0
            || self.sectors.len() <= usize::from(self.profile_count)
            || self.sectors.len() > 256
            || self
                .sectors
                .iter()
                .any(|s| s.len() != usize::from(self.sector_size))
        {
            return Err(GamingError::Invalid(
                "expected memory/macro format 1, profile format 3 or 5, 255/256-byte sectors"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Decode profiles referenced by the directory without inventing slot addresses.
    pub fn profiles(&self) -> Result<Vec<profile::ProfileSummary>, GamingError> {
        self.validate_g502x()?;
        if !profile::checksum_valid(&self.sectors[0]) {
            return Err(GamingError::Invalid("invalid profile-directory CRC".into()));
        }
        self.sectors[0][..usize::from(self.sector_size) - 2]
            .as_chunks::<4>()
            .0
            .iter()
            .take(usize::from(self.profile_count))
            .take_while(|entry| entry[..2] != [0xff, 0xff])
            .map(|entry| {
                let sector = u16::from_be_bytes([entry[0], entry[1]]);
                if sector == 0 {
                    return Err(GamingError::Invalid(
                        "profile directory references itself".into(),
                    ));
                }
                let bytes = self
                    .sectors
                    .get(usize::from(sector))
                    .ok_or_else(|| GamingError::Invalid("profile sector outside backup".into()))?;
                profile::ProfileSummary::decode(sector, entry[2] != 0, bytes, self.button_count)
            })
            .collect()
    }
}

/// An offline edit with its complete original memory as a stale-state precondition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileEdit {
    /// Local edit schema.
    pub schema_version: u8,
    /// Complete original capture; recovery does not depend on decoding the patch.
    pub original: GamingBackup,
    /// Physical profile sector named by the directory.
    pub sector: u16,
    /// Semantic edits, revalidated immediately before programming.
    pub patch: profile::ProfilePatch,
}

impl ProfileEdit {
    /// Validate and render the proposed replacement without touching hardware.
    pub fn render(&self) -> Result<Vec<u8>, GamingError> {
        self.original.validate_g502x()?;
        if self.schema_version != 1
            || !profile::checksum_valid(&self.original.sectors[0])
            || !self
                .original
                .profiles()?
                .iter()
                .any(|p| p.sector == self.sector)
        {
            return Err(GamingError::Invalid(
                "edit does not target a valid profile directory entry".into(),
            ));
        }
        profile::patch_profile(
            &self.original.sectors[usize::from(self.sector)],
            self.original.button_count,
            &self.patch,
        )
    }

    /// Compare against a fresh capture; another program's edits cannot be overwritten.
    pub fn verify_current(&self, current: &GamingBackup) -> Result<(), GamingError> {
        current.validate_g502x()?;
        if current.route != self.original.route
            || current.profile_format != self.original.profile_format
            || current.button_count != self.original.button_count
            || current.sector_size != self.original.sector_size
            || current.profile_count != self.original.profile_count
            || current.sectors != self.original.sectors
        {
            return Err(GamingError::Invalid(
                "device memory changed since the backup; create a fresh edit".into(),
            ));
        }
        Ok(())
    }
}

/// Program a prepared single-profile edit and read every byte back.
///
/// Requires onboard mode and an unchanged full backup. The caller must persist
/// that backup before calling and exclude G HUB/other device writers. Mode and
/// active-profile selection are never changed implicitly. An error after the
/// first write may leave partially programmed memory; it is never retried.
pub async fn apply_profile_edit(
    backend: &dyn HidBackend,
    edit: &ProfileEdit,
) -> Result<bool, GamingError> {
    let feature = open_onboard(backend, &edit.original.route).await?;
    apply_with_feature(&feature, edit).await
}

async fn apply_with_feature(
    feature: &OnboardProfilesFeature,
    edit: &ProfileEdit,
) -> Result<bool, GamingError> {
    let replacement = edit.render()?;
    let current = backup_with_feature(feature, &edit.original.route).await?;
    edit.verify_current(&current)?;
    if replacement == current.sectors[usize::from(edit.sector)] {
        return Ok(false);
    }
    if current.mode != 1 {
        return Err(GamingError::Invalid(
            "device is in host mode; quit G HUB and explicitly select onboard mode first".into(),
        ));
    }

    let live = feature
        .read_sector(edit.sector, current.sector_size)
        .await?;
    if live != current.sectors[usize::from(edit.sector)] {
        return Err(GamingError::Invalid(
            "profile changed immediately before write".into(),
        ));
    }
    feature.write_sector(edit.sector, &replacement).await?;
    if feature
        .read_sector(edit.sector, current.sector_size)
        .await?
        != replacement
    {
        return Err(GamingError::Invalid(
            "flash readback mismatch; retain backup and stop device writes".into(),
        ));
    }
    Ok(true)
}

/// Capture user memory. No mode switch, profile selection, or flash write occurs.
pub async fn backup(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<GamingBackup, GamingError> {
    let feature = open_onboard(backend, route).await?;
    backup_with_feature(&feature, route).await
}

async fn backup_with_feature(
    feature: &OnboardProfilesFeature,
    route: &DeviceRoute,
) -> Result<GamingBackup, GamingError> {
    let descriptor = feature.descriptor().await?;
    let mode = feature.mode().await?;
    let active_profile = feature.active_profile().await?;
    let mut sectors = Vec::with_capacity(usize::from(descriptor.sector_count));
    for sector in 0..u16::from(descriptor.sector_count) {
        sectors.push(feature.read_sector(sector, descriptor.sector_size).await?);
    }
    Ok(GamingBackup {
        schema_version: 1,
        route: route.clone(),
        memory_model: descriptor.memory_model,
        profile_format: descriptor.profile_format,
        macro_format: descriptor.macro_format,
        profile_count: descriptor.profile_count,
        button_count: descriptor.button_count,
        sector_size: descriptor.sector_size,
        mode: mode.into(),
        active_profile,
        sectors,
    })
}

async fn open_onboard(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<Arc<OnboardProfilesFeature>, GamingError> {
    let channel = open_route_channel(backend, route)
        .await?
        .ok_or(WriteError::DeviceNotFound)?;
    let index = route.device_index();
    let mut device = Device::new(channel, index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    Ok(open_feature::<OnboardProfilesFeature>(&mut device).await?)
}

/// Restore a successfully applied edit; unrelated subsequent edits are rejected.
/// A partially written or externally changed profile requires manual recovery.
pub async fn restore_profile_edit(
    backend: &dyn HidBackend,
    edit: &ProfileEdit,
) -> Result<bool, GamingError> {
    let expected = edit.render()?;
    let mut current = backup(backend, &edit.original.route).await?;
    let original = &edit.original.sectors[usize::from(edit.sector)];
    if current.sectors.get(usize::from(edit.sector)) == Some(original) {
        return Ok(false);
    }
    if current.mode != 1 || current.sectors.get(usize::from(edit.sector)) != Some(&expected) {
        return Err(GamingError::Invalid(
            "restore requires onboard mode and the exact applied profile".into(),
        ));
    }
    current.sectors[usize::from(edit.sector)].clone_from(original);
    edit.verify_current(&current)?;
    let feature = open_onboard(backend, &edit.original.route).await?;
    if feature
        .read_sector(edit.sector, current.sector_size)
        .await?
        != expected
    {
        return Err(GamingError::Invalid(
            "profile changed immediately before restore".into(),
        ));
    }
    feature.write_sector(edit.sector, original).await?;
    if feature
        .read_sector(edit.sector, current.sector_size)
        .await?
        != *original
    {
        return Err(GamingError::Invalid(
            "restore readback mismatch; stop device writes".into(),
        ));
    }
    Ok(true)
}

/// Explicitly choose host/onboard mode. This never programs flash.
pub async fn set_mode(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    mode: u8,
) -> Result<(), GamingError> {
    let feature = open_onboard(backend, route).await?;
    set_mode_with_feature(&feature, route, mode).await
}

async fn set_mode_with_feature(
    feature: &OnboardProfilesFeature,
    route: &DeviceRoute,
    mode: u8,
) -> Result<(), GamingError> {
    let mode = OnboardMode::try_from(mode)
        .map_err(|_| GamingError::Invalid("mode must be 1 (onboard) or 2 (host)".into()))?;
    let current = backup_with_feature(feature, route).await?;
    current.validate_g502x()?;
    if mode == OnboardMode::Onboard
        && !current
            .profiles()?
            .iter()
            .any(|p| p.enabled && p.checksum_valid)
    {
        return Err(GamingError::Invalid(
            "no valid enabled onboard profile".into(),
        ));
    }
    feature.set_mode(mode).await?;
    if feature.mode().await? != mode {
        return Err(GamingError::Invalid("mode readback mismatch".into()));
    }
    Ok(())
}

/// Activate an enabled profile without any flash writes; suitable for app switching.
pub async fn select_profile(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    sector: u16,
) -> Result<(), GamingError> {
    let feature = open_onboard(backend, route).await?;
    select_with_feature(&feature, route, sector).await
}

async fn select_with_feature(
    feature: &OnboardProfilesFeature,
    route: &DeviceRoute,
    sector: u16,
) -> Result<(), GamingError> {
    let current = backup_with_feature(feature, route).await?;
    if current.mode != 1
        || !current
            .profiles()?
            .iter()
            .any(|p| p.sector == sector && p.enabled && p.checksum_valid)
    {
        return Err(GamingError::Invalid(
            "select requires onboard mode and an enabled valid profile".into(),
        ));
    }
    feature.set_active_profile(sector).await?;
    if feature.active_profile().await? != sector {
        return Err(GamingError::Invalid(
            "active-profile readback mismatch".into(),
        ));
    }
    Ok(())
}

/// Read using the agent's authoritative channel, without opening a second HID handle.
pub async fn backup_on(shared: &crate::SharedChannel) -> Result<GamingBackup, GamingError> {
    let feature = open_shared(shared).await?;
    backup_with_feature(&feature, shared.route()).await
}

/// Apply an edit on the agent-owned channel. Caller owns the complete transaction lease.
pub async fn apply_on(
    shared: &crate::SharedChannel,
    edit: &ProfileEdit,
) -> Result<bool, GamingError> {
    if !shared.matches(&edit.original.route) {
        return Err(GamingError::Invalid(
            "backup belongs to a different device".into(),
        ));
    }
    let feature = open_shared(shared).await?;
    apply_with_feature(&feature, edit).await
}

/// Explicit execution-mode change on a leased agent channel.
pub async fn set_mode_on(shared: &crate::SharedChannel, mode: u8) -> Result<(), GamingError> {
    let feature = open_shared(shared).await?;
    set_mode_with_feature(&feature, shared.route(), mode).await
}

/// Explicit profile selection on a leased agent channel; does not program flash.
pub async fn select_on(shared: &crate::SharedChannel, sector: u16) -> Result<(), GamingError> {
    let feature = open_shared(shared).await?;
    select_with_feature(&feature, shared.route(), sector).await
}

async fn open_shared(
    shared: &crate::SharedChannel,
) -> Result<Arc<OnboardProfilesFeature>, GamingError> {
    let index = shared.device_index();
    let mut device = Device::new(Arc::clone(shared.channel()), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    Ok(open_feature::<OnboardProfilesFeature>(&mut device).await?)
}
