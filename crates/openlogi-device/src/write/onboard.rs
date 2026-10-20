//! HID++ `0x8100 OnboardProfiles`: the mode switch between a gaming mouse's
//! onboard memory and software control, and edits to the profile it runs.
//!
//! A save edits the active profile sector in place, so everything this
//! layer does not name (button bindings, macros, the name) is written back
//! unchanged, and is read back before the firmware is told to reload it. A
//! mouse whose user memory was never written runs a read-only factory
//! profile; the first save copies that into user profile 1 and lists it in
//! the directory, which the firmware then runs instead.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature as _,
        color_led_effects::Rgb as WireRgb,
        onboard_profiles::{
            MemoryModel, OnboardMode, OnboardProfilesFeature, OnboardProfilesInfo, ProfileFormat,
            SectorAddress,
            profile::{
                DPI_SLOTS, DirectoryEntry, ProfileDirectory, ProfileLighting, ProfileSector,
                is_sealed,
            },
        },
    },
    protocol::v20::Hidpp20Error,
};
use openlogi_core::color::Rgb;
use openlogi_core::hid::{
    Dpi, MemoryMode, OnboardChangeProblem, OnboardLighting, OnboardMemory, OnboardProfile,
    OnboardProfileChange, ReportRate,
};
use tracing::{debug, info, warn};

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::dpi::get_dpi_info_on_channel;
use super::report_rate::get_report_rate_info_on_channel;
use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

const FEATURE: u16 = OnboardProfilesFeature::ID;

/// Factory profile 1, which a mouse whose user memory holds no profile runs.
const FACTORY_PROFILE: SectorAddress = SectorAddress::new(0x0101);

fn classify(operation: HidppOperation) -> impl Fn(Hidpp20Error) -> WriteError {
    move |error| classify_hidpp_error(error, operation, FEATURE)
}

fn memory_mode(mode: OnboardMode) -> MemoryMode {
    match mode {
        OnboardMode::Onboard => MemoryMode::Onboard,
        OnboardMode::Host => MemoryMode::Software,
    }
}

fn onboard_mode(mode: MemoryMode) -> OnboardMode {
    match mode {
        MemoryMode::Onboard => OnboardMode::Onboard,
        MemoryMode::Software => OnboardMode::Host,
    }
}

fn core_rgb(color: WireRgb) -> Rgb {
    Rgb::new(color.red, color.green, color.blue)
}

fn wire_rgb(color: Rgb) -> WireRgb {
    let (red, green, blue) = color.components();
    WireRgb { red, green, blue }
}

fn onboard_lighting(lighting: ProfileLighting) -> OnboardLighting {
    match lighting {
        ProfileLighting::Off => OnboardLighting::Off,
        ProfileLighting::Fixed(color) => OnboardLighting::Fixed(core_rgb(color)),
        ProfileLighting::Cycle {
            period_ms,
            brightness,
        } => OnboardLighting::Cycle {
            period_ms,
            brightness,
        },
        ProfileLighting::Breathing {
            color,
            period_ms,
            brightness,
        } => OnboardLighting::Breathing {
            color: core_rgb(color),
            period_ms,
            brightness,
        },
        ProfileLighting::Other { .. } => OnboardLighting::Unsupported,
    }
}

/// The profile bytes for `lighting`; `None` keeps what the zone holds.
fn profile_lighting(lighting: OnboardLighting) -> Option<ProfileLighting> {
    Some(match lighting {
        OnboardLighting::Off => ProfileLighting::Off,
        OnboardLighting::Fixed(color) => ProfileLighting::Fixed(wire_rgb(color)),
        OnboardLighting::Cycle {
            period_ms,
            brightness,
        } => ProfileLighting::Cycle {
            period_ms,
            brightness,
        },
        OnboardLighting::Breathing {
            color,
            period_ms,
            brightness,
        } => ProfileLighting::Breathing {
            color: wire_rgb(color),
            period_ms,
            brightness,
        },
        OnboardLighting::Unsupported => return None,
    })
}

/// The decoded view of a profile sector.
fn describe(sector: SectorAddress, profile: &ProfileSector) -> Result<OnboardProfile, WriteError> {
    let lighting = (0..profile.led_zones())
        .map(|zone| {
            profile
                .lighting(zone)
                .map(onboard_lighting)
                .map_err(|_| unsupported(HidppOperation::ReadOnboardMemory))
        })
        .collect::<Result<_, _>>()?;
    let interval_ms = profile.report_interval_ms();
    let rate = ReportRate::from_interval_ms(interval_ms);
    Ok(OnboardProfile {
        number: sector.profile_number(),
        name: profile.name(),
        factory: sector.is_rom(),
        // A rate the profile's interval does not express (a malformed or
        // out-of-range field) is unknown rather than rounded from nothing.
        report_rate: (rate.interval_ms() == Some(interval_ms)).then_some(rate),
        dpi_slots: profile
            .dpi_slots()
            .into_iter()
            .map(|slot| slot.map(Dpi::new))
            .collect(),
        default_dpi_slot: profile.default_dpi_slot(),
        lighting,
    })
}

fn unsupported(operation: HidppOperation) -> WriteError {
    WriteError::UnsupportedResponse {
        operation,
        feature_hex: FEATURE,
    }
}

/// The profile onboard mode runs. In host mode the firmware answers
/// `getCurrentProfile` with the directory sector, which runs nothing, so the
/// profile it returns to — the saved one — stands in for it.
pub(crate) async fn running_profile(
    feature: &OnboardProfilesFeature,
    sector_size: u16,
    operation: HidppOperation,
) -> Result<SectorAddress, WriteError> {
    let current = feature
        .get_current_profile()
        .await
        .map_err(classify(operation))?;
    if current == SectorAddress::DIRECTORY {
        saved_profile(feature, sector_size, operation).await
    } else {
        Ok(current)
    }
}

/// The profile the user keeps in memory: the first enabled one the directory
/// lists, else the first listed, else factory profile 1 for a mouse whose
/// user memory holds none.
pub(crate) async fn saved_profile(
    feature: &OnboardProfilesFeature,
    sector_size: u16,
    operation: HidppOperation,
) -> Result<SectorAddress, WriteError> {
    let directory = feature
        .read_sector(SectorAddress::DIRECTORY, sector_size)
        .await
        .map_err(classify(operation))?;
    let entries = ProfileDirectory::decode(&directory)
        .map(|directory| directory.entries().to_vec())
        .unwrap_or_default();
    Ok(entries
        .iter()
        .find(|entry| entry.enabled)
        .or_else(|| entries.first())
        .map_or(FACTORY_PROFILE, |entry| entry.sector))
}

/// The onboard memory of one device, opened for one operation.
struct OnboardMemoryAccess {
    channel: Arc<HidppChannel>,
    index: u8,
    feature: Arc<OnboardProfilesFeature>,
}

/// A memory layout this layer can edit.
struct EditableLayout {
    format: ProfileFormat,
    sector_size: u16,
}

impl EditableLayout {
    fn of(info: &OnboardProfilesInfo) -> Result<Self, WriteError> {
        let refused = WriteError::OnboardLayoutUnsupported {
            memory_model: info.memory_model_id,
            profile_format: info.profile_format_id,
        };
        if !matches!(info.memory_model(), Ok(MemoryModel::G402)) {
            return Err(refused);
        }
        let format = info.profile_format().map_err(|_| refused.clone())?;
        Ok(Self {
            format,
            sector_size: info.sector_size,
        })
    }
}

impl OnboardMemoryAccess {
    async fn open(channel: &Arc<HidppChannel>, index: u8) -> Result<Self, WriteError> {
        let mut device = Device::new(Arc::clone(channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<OnboardProfilesFeature>(&mut device).await?;
        Ok(Self {
            channel: Arc::clone(channel),
            index,
            feature,
        })
    }

    async fn mode(&self, operation: HidppOperation) -> Result<MemoryMode, WriteError> {
        self.feature
            .get_mode()
            .await
            .map(memory_mode)
            .map_err(classify(operation))
    }

    async fn layout(&self, operation: HidppOperation) -> Result<EditableLayout, WriteError> {
        let info = self.feature.get_info().await.map_err(classify(operation))?;
        EditableLayout::of(&info)
    }

    /// The active profile's sector and decoded bytes.
    async fn active_profile(
        &self,
        layout: &EditableLayout,
        operation: HidppOperation,
    ) -> Result<(SectorAddress, ProfileSector), WriteError> {
        let sector = running_profile(&self.feature, layout.sector_size, operation).await?;
        let bytes = self
            .feature
            .read_sector(sector, layout.sector_size)
            .await
            .map_err(classify(operation))?;
        let decoded = if sector.is_rom() {
            ProfileSector::decode_factory(layout.format, bytes)
        } else {
            ProfileSector::decode(layout.format, bytes)
        };
        let profile = decoded.map_err(|error| {
            warn!(index = self.index, ?sector, %error, "active onboard profile does not decode");
            unsupported(operation)
        })?;
        Ok((sector, profile))
    }

    /// Put the mouse back on its saved profile when it runs a factory one or
    /// none. The firmware falls back to its factory profile when it leaves
    /// host mode, and that profile's lighting and DPI are not what the user
    /// saved; a user profile the mouse runs is left alone.
    async fn select_saved_profile(&self, operation: HidppOperation) -> Result<(), WriteError> {
        let layout = match self.layout(operation).await {
            Ok(layout) => layout,
            Err(WriteError::OnboardLayoutUnsupported { .. }) => return Ok(()),
            Err(error) => return Err(error),
        };
        let current = self
            .feature
            .get_current_profile()
            .await
            .map_err(classify(operation))?;
        if !(current.is_rom() || current == SectorAddress::DIRECTORY) {
            return Ok(());
        }
        let saved = saved_profile(&self.feature, layout.sector_size, operation).await?;
        if saved == current {
            return Ok(());
        }
        // Back from host mode the firmware refuses its user profiles until
        // the directory is written again; G HUB and libratbag end every
        // commit with that write, which is what re-arms them.
        if self.feature.set_current_profile(saved).await.is_err() {
            self.rewrite_directory(&layout, operation).await?;
            self.feature
                .set_current_profile(saved)
                .await
                .map_err(classify(operation))?;
        }
        info!(index = self.index, from = ?current, to = ?saved, "saved onboard profile selected");
        Ok(())
    }

    /// Make the firmware run `profile` afresh, so an edit to the profile it
    /// runs applies now and a temporary effect another program set over it
    /// (an LED override, say) is gone.
    ///
    /// Selecting the profile the mouse already runs reloads nothing: the G403
    /// acknowledges it and keeps every live value. Leaving onboard mode and
    /// returning does reload, but returns on the factory profile, and the
    /// firmware takes its user profiles again only once the directory is
    /// rewritten — so the reselection re-arms it when refused.
    async fn reload(
        &self,
        profile: SectorAddress,
        layout: &EditableLayout,
        operation: HidppOperation,
    ) -> Result<(), WriteError> {
        self.feature
            .set_mode(OnboardMode::Host)
            .await
            .map_err(classify(operation))?;
        self.feature
            .set_mode(OnboardMode::Onboard)
            .await
            .map_err(classify(operation))?;
        if self.feature.set_current_profile(profile).await.is_err() {
            self.rewrite_directory(layout, operation).await?;
            self.feature
                .set_current_profile(profile)
                .await
                .map_err(classify(operation))?;
        }
        debug!(index = self.index, ?profile, "onboard profile reloaded");
        Ok(())
    }

    /// Write the directory back as it stands, which makes the firmware take
    /// its user profiles again.
    async fn rewrite_directory(
        &self,
        layout: &EditableLayout,
        operation: HidppOperation,
    ) -> Result<(), WriteError> {
        let current = self
            .feature
            .read_sector(SectorAddress::DIRECTORY, layout.sector_size)
            .await
            .map_err(classify(operation))?;
        let Some(directory) = ProfileDirectory::decode(&current) else {
            return Ok(());
        };
        let bytes = if is_sealed(&current) {
            current
        } else {
            directory.encode(usize::from(layout.sector_size))
        };
        self.write_verified(SectorAddress::DIRECTORY, &bytes, operation)
            .await
    }

    async fn read(&self) -> Result<OnboardMemory, WriteError> {
        let operation = HidppOperation::ReadOnboardMemory;
        let mode = self.mode(operation).await?;
        let profile = match self.layout(operation).await {
            Ok(layout) => {
                let (sector, profile) = self.active_profile(&layout, operation).await?;
                Some(describe(sector, &profile)?)
            }
            Err(WriteError::OnboardLayoutUnsupported {
                memory_model,
                profile_format,
            }) => {
                debug!(
                    index = self.index,
                    memory_model, profile_format, "onboard memory layout not decoded"
                );
                None
            }
            Err(error) => return Err(error),
        };
        Ok(OnboardMemory { mode, profile })
    }

    async fn set_mode(&self, mode: MemoryMode) -> Result<OnboardMemory, WriteError> {
        let operation = HidppOperation::WriteMemoryMode;
        if self.mode(operation).await? != mode {
            self.feature
                .set_mode(onboard_mode(mode))
                .await
                .map_err(classify(operation))?;
            debug!(index = self.index, ?mode, "onboard mode written");
        }
        if mode == MemoryMode::Onboard {
            self.select_saved_profile(operation).await?;
        }
        self.read().await
    }

    /// The stored user profile matching factory profile `number`, when the
    /// mouse holds one.
    ///
    /// A listed sector that does not decode is refused rather than replaced:
    /// it may hold the button bindings and macros of a profile the user saved.
    /// A sector the directory omits but that still decodes is recovered — the
    /// save's directory write lists it again. An undecodable unlisted sector
    /// is empty user memory, which the first save fills from the factory
    /// profile.
    async fn stored_user_profile(
        &self,
        number: u8,
        layout: &EditableLayout,
        operation: HidppOperation,
    ) -> Result<Option<ProfileSector>, WriteError> {
        let sector = SectorAddress::user_profile(number);
        let directory = self
            .feature
            .read_sector(SectorAddress::DIRECTORY, layout.sector_size)
            .await
            .map_err(classify(operation))?;
        let listed = ProfileDirectory::decode(&directory).is_some_and(|directory| {
            directory
                .entries()
                .iter()
                .any(|entry| entry.sector == sector)
        });
        let bytes = self
            .feature
            .read_sector(sector, layout.sector_size)
            .await
            .map_err(classify(operation))?;
        match ProfileSector::decode(layout.format, bytes) {
            Ok(profile) => Ok(Some(profile)),
            Err(error) if listed => {
                warn!(
                    index = self.index,
                    ?sector,
                    %error,
                    "listed onboard profile does not decode"
                );
                Err(WriteError::OnboardProfileUnreadable { profile: number })
            }
            Err(_) => Ok(None),
        }
    }

    /// Refuse `change` unless the profile and the sensor can take all of it.
    async fn validate(
        &self,
        profile: &ProfileSector,
        change: &OnboardProfileChange,
    ) -> Result<(), WriteError> {
        let reject = |problem| Err(WriteError::OnboardChangeRejected(problem));
        if change.dpi_slots.len() > DPI_SLOTS {
            return reject(OnboardChangeProblem::TooManyDpiSlots {
                max: u8::try_from(DPI_SLOTS).unwrap_or(u8::MAX),
            });
        }
        let default = change.default_dpi_slot;
        if change
            .dpi_slots
            .get(usize::from(default))
            .copied()
            .flatten()
            .is_none()
        {
            return reject(OnboardChangeProblem::DefaultSlotEmpty { slot: default });
        }
        if change.lighting.len() != profile.led_zones() {
            return reject(OnboardChangeProblem::LightingZones {
                zones: u8::try_from(profile.led_zones()).unwrap_or(u8::MAX),
            });
        }
        let sensor = get_dpi_info_on_channel(&self.channel, self.index)
            .await?
            .capabilities;
        if let Some(dpi) = change
            .dpi_slots
            .iter()
            .flatten()
            .find(|&&dpi| !sensor.contains(dpi))
        {
            return reject(OnboardChangeProblem::DpiUnsupported { dpi: *dpi });
        }
        let rates = get_report_rate_info_on_channel(&self.channel, self.index).await?;
        if !rates.supports(change.report_rate) || change.report_rate.interval_ms().is_none() {
            return reject(OnboardChangeProblem::ReportRateUnsupported {
                rate: change.report_rate,
            });
        }
        Ok(())
    }

    async fn write(&self, change: &OnboardProfileChange) -> Result<OnboardMemory, WriteError> {
        let operation = HidppOperation::WriteOnboardProfile;
        let layout = self.layout(operation).await?;
        let (active, running) = self.active_profile(&layout, operation).await?;
        if active.profile_number() != change.profile {
            return Err(WriteError::OnboardChangeRejected(
                OnboardChangeProblem::ProfileChanged {
                    expected: change.profile,
                    active: active.profile_number(),
                },
            ));
        }
        // A factory profile cannot be written: its edit lands in the user
        // profile of the same number. A mouse that fell back to factory
        // profile 1 (after host mode, or because selecting the saved profile
        // failed) may still have that user profile stored, bindings and all;
        // its bytes are the base of the edit, not the factory ones. Only a
        // user sector that holds no profile is copied from the factory one.
        let (target, original) = if active.is_rom() {
            let number = active.profile_number();
            match self.stored_user_profile(number, &layout, operation).await? {
                Some(stored) => (SectorAddress::user_profile(number), stored),
                None => (SectorAddress::user_profile(number), running),
            }
        } else {
            (active, running)
        };
        self.validate(&original, change).await?;

        // Each field is written only when the change asks for something else
        // than it holds: the firmware and other software encode the same
        // value in more than one way (an "off" effect padded with `0xff` or
        // with zeros), and an untouched field keeps its exact bytes.
        let mut edited = original.clone();
        let rejected = |_| unsupported(operation);
        let stored_slots = original.dpi_slots();
        for (slot, stored) in stored_slots.iter().enumerate() {
            let dpi = change
                .dpi_slots
                .get(slot)
                .copied()
                .flatten()
                .map(Dpi::into_inner);
            if dpi != *stored {
                edited.set_dpi_slot(slot, dpi).map_err(rejected)?;
            }
        }
        if change.default_dpi_slot != original.default_dpi_slot() {
            edited
                .set_default_dpi_slot(change.default_dpi_slot)
                .map_err(rejected)?;
        }
        let interval =
            change
                .report_rate
                .interval_ms()
                .ok_or(WriteError::OnboardChangeRejected(
                    OnboardChangeProblem::ReportRateUnsupported {
                        rate: change.report_rate,
                    },
                ))?;
        if interval != original.report_interval_ms() {
            edited.set_report_interval_ms(interval).map_err(rejected)?;
        }
        for (zone, lighting) in change.lighting.iter().enumerate() {
            let stored = original.lighting(zone).map_err(rejected)?;
            if let Some(lighting) = profile_lighting(*lighting)
                && lighting != stored
            {
                edited.set_lighting(zone, lighting).map_err(rejected)?;
            }
        }

        let bytes = edited.encode();
        if !active.is_rom() && bytes == original.encode() {
            debug!(
                index = self.index,
                "onboard profile already holds the change"
            );
            return self.read().await;
        }
        self.write_verified(target, &bytes, operation)
            .await
            .inspect_err(|_| {
                warn!(
                    index = self.index,
                    ?target,
                    "onboard profile write not kept"
                );
            })?;
        // The directory goes last, as G HUB and libratbag commit: writing it
        // is what makes the firmware take the user profiles it lists.
        self.list_in_directory(target, layout.sector_size, operation)
            .await?;
        // In host mode the mouse runs no profile; it loads the edited one
        // when it returns to onboard mode.
        if self.mode(operation).await? == MemoryMode::Onboard {
            self.reload(target, &layout, operation).await?;
        }
        info!(index = self.index, ?target, "onboard profile written");
        self.read().await
    }

    /// Write `bytes` to `sector` and read them back. A write or read-back
    /// that fails, or bytes that read back otherwise, leaves the sector
    /// restored to what it held when that was a profile the firmware could
    /// run; the original error is returned either way.
    async fn write_verified(
        &self,
        sector: SectorAddress,
        bytes: &[u8],
        operation: HidppOperation,
    ) -> Result<(), WriteError> {
        let size = u16::try_from(bytes.len()).map_err(|_| unsupported(operation))?;
        let before = self
            .feature
            .read_sector(sector, size)
            .await
            .map_err(classify(operation))?;
        // From here the sector may hold part of `bytes`: a write the device
        // rejects halfway, or one that reads back wrong, would otherwise leave
        // the mouse on a torn profile.
        let written = async {
            self.feature
                .write_sector(sector, bytes)
                .await
                .map_err(classify(operation))?;
            let after = self
                .feature
                .read_sector(sector, size)
                .await
                .map_err(classify(operation))?;
            if after == bytes {
                Ok(())
            } else {
                Err(WriteError::OnboardProfileNotApplied)
            }
        }
        .await;
        if written.is_err()
            && is_sealed(&before)
            && let Err(error) = self.feature.write_sector(sector, &before).await
        {
            warn!(
                index = self.index,
                ?sector,
                ?error,
                "onboard sector restore failed"
            );
        }
        written
    }

    /// Make the directory list `sector`, enabled, keeping what it lists, and
    /// write it — unchanged bytes included.
    async fn list_in_directory(
        &self,
        sector: SectorAddress,
        sector_size: u16,
        operation: HidppOperation,
    ) -> Result<(), WriteError> {
        let current = self
            .feature
            .read_sector(SectorAddress::DIRECTORY, sector_size)
            .await
            .map_err(classify(operation))?;
        let mut entries = ProfileDirectory::decode(&current)
            .map(|directory| directory.entries().to_vec())
            .unwrap_or_default();
        match entries.iter_mut().find(|entry| entry.sector == sector) {
            Some(entry) if entry.enabled => {
                if is_sealed(&current) {
                    return self
                        .write_verified(SectorAddress::DIRECTORY, &current, operation)
                        .await;
                }
            }
            Some(entry) => entry.enabled = true,
            None => {
                entries.push(DirectoryEntry {
                    sector,
                    enabled: true,
                });
                entries.sort_by_key(|entry| entry.sector);
            }
        }
        let bytes = ProfileDirectory::new(entries).encode(usize::from(sector_size));
        self.write_verified(SectorAddress::DIRECTORY, &bytes, operation)
            .await
    }
}

/// Read the onboard mode and active profile of the device on `route`.
pub async fn read_onboard_memory(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<OnboardMemory, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        OnboardMemoryAccess::open(&channel, index)
            .await?
            .read()
            .await
    })
    .await
}

/// [`read_onboard_memory`] on an already-open [`SharedChannel`].
pub async fn read_onboard_memory_on(shared: &SharedChannel) -> Result<OnboardMemory, WriteError> {
    OnboardMemoryAccess::open(shared.channel(), shared.device_index())
        .await?
        .read()
        .await
}

/// Switch the device on `route` to `mode` and read back where it stands.
pub async fn set_memory_mode(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    mode: MemoryMode,
) -> Result<OnboardMemory, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        OnboardMemoryAccess::open(&channel, index)
            .await?
            .set_mode(mode)
            .await
    })
    .await
}

/// [`set_memory_mode`] on an already-open [`SharedChannel`].
pub async fn set_memory_mode_on(
    shared: &SharedChannel,
    mode: MemoryMode,
) -> Result<OnboardMemory, WriteError> {
    OnboardMemoryAccess::open(shared.channel(), shared.device_index())
        .await?
        .set_mode(mode)
        .await
}

/// Write `change` into the active onboard profile of the device on `route`
/// and read back where it stands. A change the profile or sensor cannot take
/// is refused with [`WriteError::OnboardChangeRejected`] before anything is
/// written.
pub async fn write_onboard_profile(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    change: OnboardProfileChange,
) -> Result<OnboardMemory, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        OnboardMemoryAccess::open(&channel, index)
            .await?
            .write(&change)
            .await
    })
    .await
}

/// [`write_onboard_profile`] on an already-open [`SharedChannel`].
pub async fn write_onboard_profile_on(
    shared: &SharedChannel,
    change: &OnboardProfileChange,
) -> Result<OnboardMemory, WriteError> {
    OnboardMemoryAccess::open(shared.channel(), shared.device_index())
        .await?
        .write(change)
        .await
}

#[cfg(test)]
mod tests {
    // The report-interval conversions this module uses live on
    // `openlogi_core::hid::ReportRate` and are tested there.
}
