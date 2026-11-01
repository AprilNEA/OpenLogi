//! The G700's onboard profiles: reading the directory's five slots, the
//! edits OpenLogi makes to them, and the flash job that brings the played
//! profile's pointer settings and bindings to what the agent asks.
//!
//! The mouse is the source of truth. Everything here starts from what the
//! mouse holds and writes only what an edit changes; nothing is derived from
//! configuration but the button bindings, whose decision is
//! `openlogi_core::binding::flash`'s.
//!
//! # Which pages a new profile may take
//!
//! A G700 has flash pages 2 to 31, and the dumps of the one this was
//! written against show more than the directory accounts for: pages 5 and 6
//! hold profiles Logitech Gaming Software once listed, page 7 macros, 25 and
//! 26 profile-like data with another checksum, and 27 to 31 do not read at
//! all. So a profile is only ever created on, or re-listed from,
//! [`PROFILE_PAGES`]: pages 2 to 4 and 8 to 24. A page outside them is
//! touched only when the directory lists it. `hidpp`'s guarded writer adds
//! its own refusal of any page that is neither listed nor erased.
//!
//! # The directory
//!
//! The directory page is the riskiest page: while it is being rewritten the
//! mouse plays its factory profile, and a directory that lists nothing
//! leaves it nothing else. An edit that changes it writes every other page
//! first and the directory last, never writes one that lists no profile,
//! and names the profile the mouse plays afterwards, which it reads back.

use std::collections::{BTreeSet, VecDeque};

use hidpp::register::{
    current_profile::{CurrentProfile, read_current_profile},
    onboard_memory::{
        Page, READ_CHUNK,
        directory::{DIRECTORY_PAGE, DirectoryEntry, ProfileDirectory},
        g700::{
            self, DPI_MODE_COUNT, G700Profile, PROFILE_COUNT, dpi_from_raw, encode_name,
            raw_from_dpi,
        },
        read_memory,
    },
    report_rate::{ReportInterval, read_report_interval, write_report_interval},
    resolution::{Resolution, read_current_resolution, write_current_resolution},
};
use openlogi_core::binding::flash::plan_flash_bindings;
use openlogi_core::binding::{FlashBinding, FlashPlan, FlashWrite, G700, SlotIntent};
use openlogi_core::hid::{
    DisabledProfile, Dpi, FlashProfileEdit, FlashProfileSettings, FlashProfiles, OnboardMode,
    OnboardProfile, OnboardState, ProfileIndex, ReportRate,
};
use tracing::{debug, info, warn};

use super::G700Link;
use super::bindings::{flash_binding, wire_binding};
use super::configure::{
    G700Error, G700Register, profile_interval_valid, read, read_trusted, set_current_profile_on,
};
use crate::SharedChannel;
use crate::write::WriteError;

/// The pages a profile may be created on or listed again from (see the
/// module documentation).
pub const PROFILE_PAGES: [u8; 20] = [
    2, 3, 4, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
];

const _: () = assert!(
    hidpp::register::onboard_memory::g700::NAME_UNITS
        == openlogi_core::hid::FLASH_PROFILE_NAME_UNITS,
    "the name length clients check is the length a G700 profile stores"
);

/// The word offsets of the chunks a profile listing reads of each profile
/// page: the DPI modes, default mode and report interval (bytes `0..32`),
/// and the metadata marker and name (bytes `64..128`).
const LISTING_CHUNKS: [u8; 6] = [0, 8, 32, 40, 48, 56];

/// A save of a DPI set live to the profile the mouse plays: the DPI mode
/// that held `from` takes `to`. Coalesced saves keep the first `from`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DpiSave {
    /// The DPI the mouse ran at before the live change — the value of the
    /// mode it was in.
    pub from: Dpi,
    /// The DPI it runs at now.
    pub to: Dpi,
}

/// What one flash job brings a G700 to, applied in this order: the profile
/// edits, then the played profile's DPI and report-rate saves together with
/// its button bindings, in one page write.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct G700Job {
    /// Profile edits, oldest first. Each is removed once it is done, so a
    /// job that fails holds the edit that failed first, then the ones not
    /// tried. [`FlashProfileEdit::Activate`] is not a flash write and is
    /// refused here.
    pub edits: VecDeque<FlashProfileEdit>,
    /// The live DPI to save to the played profile.
    pub dpi: Option<DpiSave>,
    /// The report rate to save to the played profile.
    pub rate: Option<ReportRate>,
    /// What the configuration wants of each button slot.
    pub bindings: Vec<SlotIntent>,
}

/// What a [`sync_g700_on`] found and did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct G700Sync {
    /// The directory index of the profile the mouse plays; `None` while it
    /// plays its factory profile, which has no page to hold bindings, so its
    /// bindings and pointer settings were neither read nor written.
    pub profile: Option<u8>,
    /// Where the played profile's bindings went, as they are on the mouse
    /// now: written when [`Self::written`], else as read.
    pub plan: FlashPlan,
    /// Whether any page was rewritten.
    pub written: bool,
}

/// Checks `edit` before it is queued: what can be refused without reading
/// the mouse is.
pub fn check_g700_edit(edit: &FlashProfileEdit) -> Result<(), G700Error> {
    match edit {
        FlashProfileEdit::Rename { name, .. } => {
            encode_name(name).map(drop).map_err(G700Error::InvalidName)
        }
        FlashProfileEdit::DpiModes(modes) => dpi_mode_steps(modes).map(drop),
        FlashProfileEdit::Activate(_)
        | FlashProfileEdit::Enable { .. }
        | FlashProfileEdit::Disable(_)
        | FlashProfileEdit::Duplicate(_) => Ok(()),
    }
}

/// The raw sensor step of each of `modes`, which must be one to
/// [`DPI_MODE_COUNT`] values the sensor table names exactly.
fn dpi_mode_steps(modes: &[Dpi]) -> Result<Vec<u8>, G700Error> {
    if modes.is_empty() || modes.len() > DPI_MODE_COUNT {
        return Err(G700Error::DpiModeCount { count: modes.len() });
    }
    modes
        .iter()
        .map(|dpi| {
            let raw = raw_from_dpi(dpi.into_inner());
            match dpi_from_raw(raw) {
                Some(exact) if exact.get() == dpi.into_inner() => Ok(raw),
                _ => Err(G700Error::DpiNotInTable { dpi: *dpi }),
            }
        })
        .collect()
}

/// The directory position of the profile `current` names; `None` for the
/// factory profile or an address the directory does not list.
pub(super) fn played_entry(current: CurrentProfile, entries: &[DirectoryEntry]) -> Option<usize> {
    match current {
        CurrentProfile::Index(index) => {
            let index = usize::from(index);
            (index < entries.len()).then_some(index)
        }
        CurrentProfile::Address { page, offset } => entries
            .iter()
            .position(|entry| entry.page == page && entry.offset == offset),
        CurrentProfile::Factory => None,
    }
}

async fn current_profile(link: &G700Link) -> Result<CurrentProfile, G700Error> {
    read_current_profile(link.chan(), link.device_index())
        .await
        .map_err(|source| G700Error::RegisterRead {
            register: G700Register::CurrentProfile,
            source,
        })
}

/// The directory position of the profile the G700 behind `shared` plays,
/// `None` for its factory profile: one register read, and a directory read
/// only when the mouse names the profile by address.
pub async fn read_g700_played_profile_on(shared: &SharedChannel) -> Result<Option<u8>, G700Error> {
    let link = G700Link::on_shared(shared)
        .await?
        .ok_or(G700Error::Device(WriteError::DeviceNotFound))?;
    let current = current_profile(&link).await?;
    let position = match current {
        CurrentProfile::Index(index) => Some(usize::from(index)),
        CurrentProfile::Factory => None,
        CurrentProfile::Address { .. } => {
            let directory = read_directory_head(&link).await?;
            played_entry(current, directory.entries())
        }
    };
    Ok(position.and_then(|position| u8::try_from(position).ok()))
}

/// The chunks `words` of `page`, laid over an erased page. For listings
/// only: unlike [`read_trusted`], nothing vouches that the chunks belong
/// together, so no write may start from it.
async fn read_chunks(link: &G700Link, page: u8, words: &[u8]) -> Result<Page, G700Error> {
    let mut bytes = *Page::erased().as_bytes();
    for &word in words {
        let chunk = read_memory(link.chan(), link.device_index(), page, word)
            .await
            .map_err(|source| G700Error::PageRead { page, source })?;
        let start = usize::from(word) * 2;
        bytes[start..start + READ_CHUNK].copy_from_slice(&chunk);
    }
    Ok(Page::from_bytes(bytes))
}

/// The directory's entries from its first chunk, which holds all five.
async fn read_directory_head(link: &G700Link) -> Result<ProfileDirectory, G700Error> {
    const _: () = assert!(PROFILE_COUNT * 3 <= READ_CHUNK);
    Ok(g700::parse_directory(
        &read_chunks(link, DIRECTORY_PAGE, &[0]).await?,
    ))
}

/// The settings `profile` stores, as the wire carries them.
fn settings_of(profile: &G700Profile) -> FlashProfileSettings {
    FlashProfileSettings {
        dpi_modes: profile
            .dpi_modes
            .iter()
            .map(|mode| mode.x_dpi().map(|dpi| Dpi::new(dpi.get())))
            .collect(),
        default_mode: (usize::from(profile.default_dpi_mode) < DPI_MODE_COUNT)
            .then_some(profile.default_dpi_mode),
        report_rate: rate_of_millis(profile.report_interval_millis),
    }
}

fn rate_of_millis(millis: u8) -> Option<ReportRate> {
    ReportInterval::try_from(millis)
        .ok()
        .filter(|interval| profile_interval_valid(*interval))
        .map(|interval| ReportRate::new(interval.rate_hz()))
}

/// The interval whose rate is `rate`, when it is one a profile stores.
fn interval_of_rate(rate: ReportRate) -> Option<ReportInterval> {
    let millis = u8::try_from(1000 / u16::from(rate).max(1)).ok()?;
    ReportInterval::try_from(millis).ok().filter(|interval| {
        profile_interval_valid(*interval) && u16::from(rate) == interval.rate_hz()
    })
}

/// Whether `page`, which no directory entry names, holds a profile that can
/// be listed again: its CRC holds, it carries the metadata block, its report
/// interval is one a profile stores, and every DPI mode is off or a step of
/// the sensor table. Macro pages and erased or foreign data fail one of
/// these.
fn is_stored_profile(page: &Page) -> bool {
    let profile = G700Profile::parse(page);
    page.has_valid_crc()
        && profile.has_metadata()
        && rate_of_millis(profile.report_interval_millis).is_some()
        && profile
            .dpi_modes
            .iter()
            .all(|mode| mode.x == 0 || dpi_from_raw(mode.x).is_some())
}

/// Read the G700 behind `shared`'s profiles: the five directory slots with
/// their names, the profile it plays and that profile's pointer settings,
/// the profiles [`PROFILE_PAGES`] holds that the directory does not list,
/// and whether one of those pages is free for a new profile.
///
/// Read-only, and short: only the chunks a listing needs are read, so the
/// names and settings are for display — every write reads its page whole
/// first. [`FlashProfiles::write`] is left [`FlashWrite::Idle`] for the
/// caller to fill.
pub async fn read_g700_profiles_on(shared: &SharedChannel) -> Result<FlashProfiles, G700Error> {
    let link = G700Link::on_shared(shared)
        .await?
        .ok_or(G700Error::Device(WriteError::DeviceNotFound))?;
    let current = current_profile(&link).await?;
    let directory = read_directory_head(&link).await?;
    let entries = directory.entries();
    let played = played_entry(current, entries);
    let mut profiles = Vec::with_capacity(PROFILE_COUNT);
    let mut active = None;
    for position in 0..PROFILE_COUNT {
        let Some(index) = u8::try_from(position + 1).ok().and_then(ProfileIndex::new) else {
            continue;
        };
        let Some(entry) = entries.get(position) else {
            profiles.push(OnboardProfile {
                index,
                name: None,
                enabled: false,
            });
            continue;
        };
        let profile = G700Profile::parse(&read_chunks(&link, entry.page, &LISTING_CHUNKS).await?);
        if played == Some(position) {
            active = Some(settings_of(&profile));
        }
        profiles.push(OnboardProfile {
            index,
            name: profile.name(),
            enabled: true,
        });
    }
    let mut disabled = Vec::new();
    let mut can_create = false;
    for &page in PROFILE_PAGES
        .iter()
        .filter(|page| !entries.iter().any(|entry| entry.page == **page))
    {
        let head = read_memory(link.chan(), link.device_index(), page, 0)
            .await
            .map_err(|source| G700Error::PageRead { page, source })?;
        if head.iter().all(|&byte| byte == 0xff) {
            can_create = true;
            continue;
        }
        let stored = read(link.chan(), link.device_index(), page).await?;
        if is_stored_profile(&stored) {
            disabled.push(DisabledProfile {
                page,
                name: G700Profile::parse(&stored).name(),
            });
        }
    }
    Ok(FlashProfiles {
        onboard: OnboardState {
            mode: OnboardMode::Onboard,
            active_profile: played
                .and_then(|position| u8::try_from(position + 1).ok())
                .and_then(ProfileIndex::new),
            profiles,
        },
        active,
        disabled,
        can_create,
        write: FlashWrite::Idle,
    })
}

/// Make the G700 behind `shared` play `profile`, an enabled one, now: a
/// register write, read back.
pub async fn activate_g700_profile_on(
    shared: &SharedChannel,
    profile: ProfileIndex,
) -> Result<(), G700Error> {
    let link = G700Link::on_shared(shared)
        .await?
        .ok_or(G700Error::Device(WriteError::DeviceNotFound))?;
    let index = profile.get().get() - 1;
    set_current_profile_on(
        link.chan(),
        link.device_index(),
        CurrentProfile::Index(index),
    )
    .await
    .map(drop)
}

/// Bring the G700 behind `shared` to `job`, to its end, and report what
/// its played profile holds now. Edits come first, each removed from
/// `job.edits` once done; a failed edit stops the job there.
///
/// A page write plays the factory profile and then the previous one again,
/// and the mouse resets its live DPI and report rate whenever it selects a
/// profile. So when a job writes and the mouse still plays the profile it
/// played before, the live values come back afterwards: the DPI saved, or
/// the one the mouse ran at, and likewise the rate.
///
/// The G700's register phase is held throughout. A write cannot be
/// abandoned midway without leaving the mouse on its factory profile, so
/// the caller must let this run to its end.
pub async fn sync_g700_on(
    shared: &SharedChannel,
    job: &mut G700Job,
) -> Result<G700Sync, G700Error> {
    let link = G700Link::on_shared(shared)
        .await?
        .ok_or(G700Error::Device(WriteError::DeviceNotFound))?;
    let (chan, index) = (link.chan(), link.device_index());
    let started_on = played_page(&link).await?;
    let live_resolution = read_current_resolution(chan, index)
        .await
        .map_err(|source| G700Error::RegisterRead {
            register: G700Register::CurrentResolution,
            source,
        })?;
    let live_interval =
        read_report_interval(chan, index)
            .await
            .map_err(|source| G700Error::RegisterRead {
                register: G700Register::ReportInterval,
                source,
            })?;

    let mut written = false;
    while let Some(edit) = job.edits.front() {
        apply_edit(&link, edit).await?;
        written = true;
        job.edits.pop_front();
    }

    let current = current_profile(&link).await?;
    let directory = g700::parse_directory(&read(chan, index, DIRECTORY_PAGE).await?);
    let entries = directory.entries();
    let Some(played) = played_entry(current, entries) else {
        return Ok(G700Sync {
            profile: None,
            plan: FlashPlan::default(),
            written,
        });
    };
    let entry = entries[played];
    let base = read_trusted(chan, index, entry.page).await?;
    let mut profile = G700Profile::parse(&base);
    let held: Vec<FlashBinding> = profile.bindings.iter().map(|b| flash_binding(*b)).collect();
    let mut reserved = BTreeSet::new();
    for other in entries.iter().filter(|other| other.page != entry.page) {
        let page = read(chan, index, other.page).await?;
        reserved.extend(
            G700Profile::parse(&page)
                .bindings
                .iter()
                .flat_map(|binding| flash_binding(*binding).buttons()),
        );
    }
    let plan = plan_flash_bindings(&G700, &held, &reserved, &job.bindings);
    for (slot, binding) in profile.bindings.iter_mut().zip(&plan.bindings) {
        *slot = wire_binding(*binding);
    }
    if let Some(save) = job.dpi {
        save_dpi(&mut profile, save);
    }
    if let Some(rate) = job.rate {
        if let Some(interval) = interval_of_rate(rate) {
            profile.report_interval_millis = interval.millis();
        } else {
            warn!(%rate, "no profile interval for this rate — not saved");
        }
    }
    let page = profile.to_page();
    if page != base {
        info!(
            page = entry.page,
            profile = played,
            "writing the G700's played profile"
        );
        g700::memory_writer(chan, index)
            .write_page(entry.page, &page)
            .await?;
        written = true;
    }

    if written && started_on == Some(entry.page) {
        let resolution = job.dpi.map_or(live_resolution, |save| {
            let raw = u16::from(raw_from_dpi(save.to.into_inner()));
            Resolution { x: raw, y: raw }
        });
        let interval = job.rate.and_then(interval_of_rate).unwrap_or(live_interval);
        restore_live(&link, resolution, interval).await;
    }
    job.dpi = None;
    job.rate = None;
    Ok(G700Sync {
        profile: u8::try_from(played).ok(),
        plan,
        written,
    })
}

/// The page of the profile the mouse plays, `None` for its factory profile.
async fn played_page(link: &G700Link) -> Result<Option<u8>, G700Error> {
    let current = current_profile(link).await?;
    if current == CurrentProfile::Factory {
        return Ok(None);
    }
    let directory = read_directory_head(link).await?;
    Ok(played_entry(current, directory.entries())
        .map(|position| directory.entries()[position].page))
}

/// Puts the live DPI and report interval back after a page write reset
/// them. Best effort: the flash already holds what the job wrote, so a
/// failure here only leaves the mouse on its profile's start values.
async fn restore_live(link: &G700Link, resolution: Resolution, interval: ReportInterval) {
    let (chan, index) = (link.chan(), link.device_index());
    if let Err(error) = write_current_resolution(chan, index, resolution).await {
        warn!(
            ?error,
            "the G700's live DPI did not come back after the write"
        );
    }
    if let Err(error) = write_report_interval(chan, index, interval).await {
        warn!(
            ?error,
            "the G700's live report rate did not come back after the write"
        );
    }
}

/// The mode of `profile` that holds `save.from` takes `save.to`. When no
/// mode holds `from` and one already holds `to`, an earlier attempt of the
/// same save got there; when neither, the mouse was in no mode of its
/// profile and nothing is saved.
fn save_dpi(profile: &mut G700Profile, save: DpiSave) {
    let from = raw_from_dpi(save.from.into_inner());
    let to = raw_from_dpi(save.to.into_inner());
    if let Some(mode) = profile.dpi_modes.iter_mut().find(|mode| mode.x == from) {
        mode.x = to;
        mode.y = to;
    } else if !profile.dpi_modes.iter().any(|mode| mode.x == to) {
        debug!(
            from = %save.from,
            to = %save.to,
            "the G700 ran at a DPI none of its modes holds — not saved"
        );
    }
}

/// One profile edit, to its end.
async fn apply_edit(link: &G700Link, edit: &FlashProfileEdit) -> Result<(), G700Error> {
    check_g700_edit(edit)?;
    let (chan, index) = (link.chan(), link.device_index());
    let directory = g700::parse_directory(&read_trusted(chan, index, DIRECTORY_PAGE).await?);
    let current = current_profile(link).await?;
    let entry_of = |profile: ProfileIndex| {
        let position = usize::from(profile.get().get() - 1);
        directory
            .entries()
            .get(position)
            .copied()
            .ok_or(G700Error::NoSuchProfile {
                index: profile.get().get() - 1,
                enabled: directory.entries().len(),
            })
    };
    match edit {
        FlashProfileEdit::Activate(_) => Err(G700Error::NotAFlashEdit),
        FlashProfileEdit::Rename { profile, name } => {
            let entry = entry_of(*profile)?;
            let mut stored = G700Profile::parse(&read_trusted(chan, index, entry.page).await?);
            stored.set_name(name).map_err(G700Error::InvalidName)?;
            info!(page = entry.page, "renaming a G700 profile");
            g700::memory_writer(chan, index)
                .write_page(entry.page, &stored.to_page())
                .await?;
            Ok(())
        }
        FlashProfileEdit::DpiModes(modes) => {
            write_dpi_modes(link, &directory, current, modes).await
        }
        FlashProfileEdit::Disable(profile) => {
            let position = usize::from(profile.get().get() - 1);
            entry_of(*profile)?;
            if directory.entries().len() <= 1 {
                return Err(G700Error::LastProfile);
            }
            let changed = directory
                .without(position)
                .ok_or(G700Error::NoSuchProfile {
                    index: profile.get().get() - 1,
                    enabled: directory.entries().len(),
                })?;
            let played = played_entry(current, directory.entries());
            let play = match played {
                // The disabled profile played: the one before it plays
                // instead, or the new first.
                Some(played) if played == position => position.saturating_sub(1),
                Some(played) if played > position => played - 1,
                Some(played) => played,
                None => 0,
            };
            info!(position, "disabling a G700 profile");
            write_directory(link, &changed, play).await
        }
        FlashProfileEdit::Enable { page } => {
            if !PROFILE_PAGES.contains(page) {
                return Err(G700Error::NotAStoredProfile { page: *page });
            }
            if directory.entries().iter().any(|entry| entry.page == *page) {
                return Ok(());
            }
            let stored = read_trusted(chan, index, *page).await?;
            if !is_stored_profile(&stored) {
                return Err(G700Error::NotAStoredProfile { page: *page });
            }
            let changed = directory
                .with_appended(*page)
                .ok_or(G700Error::DirectoryFull)?;
            let play = played_entry(current, directory.entries()).unwrap_or(0);
            info!(page, "enabling a G700 profile");
            write_directory(link, &changed, play).await
        }
        FlashProfileEdit::Duplicate(profile) => {
            let source = entry_of(*profile)?;
            duplicate_profile(link, &directory, current, source).await
        }
    }
}

/// Replaces the DPI modes of the profile the mouse plays with `modes`.
async fn write_dpi_modes(
    link: &G700Link,
    directory: &ProfileDirectory,
    current: CurrentProfile,
    modes: &[Dpi],
) -> Result<(), G700Error> {
    let (chan, index) = (link.chan(), link.device_index());
    let steps = dpi_mode_steps(modes)?;
    let position = played_entry(current, directory.entries()).ok_or(G700Error::NoPlayedProfile)?;
    let entry = directory.entries()[position];
    let mut stored = G700Profile::parse(&read_trusted(chan, index, entry.page).await?);
    for (position, mode) in stored.dpi_modes.iter_mut().enumerate() {
        let raw = steps.get(position).copied().unwrap_or(0);
        mode.x = raw;
        mode.y = raw;
    }
    let default = usize::from(stored.default_dpi_mode);
    if default < DPI_MODE_COUNT && default >= steps.len() {
        stored.default_dpi_mode = 0;
    }
    info!(page = entry.page, "writing a G700 profile's DPI modes");
    g700::memory_writer(chan, index)
        .write_page(entry.page, &stored.to_page())
        .await?;
    Ok(())
}

/// Stores a copy of the `source` profile on the first erased profile page
/// and lists it — the directory last, so a failed copy lists nothing.
async fn duplicate_profile(
    link: &G700Link,
    directory: &ProfileDirectory,
    current: CurrentProfile,
    source: DirectoryEntry,
) -> Result<(), G700Error> {
    let (chan, index) = (link.chan(), link.device_index());
    if directory.entries().len() >= directory.capacity() {
        return Err(G700Error::DirectoryFull);
    }
    let content = read_trusted(chan, index, source.page).await?;
    let mut target = None;
    for &page in PROFILE_PAGES
        .iter()
        .filter(|page| !directory.entries().iter().any(|entry| entry.page == **page))
    {
        if read(chan, index, page).await?.is_erased() {
            target = Some(page);
            break;
        }
    }
    let target = target.ok_or(G700Error::NoFreePage)?;
    let changed = directory
        .with_appended(target)
        .ok_or(G700Error::DirectoryFull)?;
    info!(from = source.page, to = target, "copying a G700 profile");
    g700::memory_writer(chan, index)
        .write_page(target, &content)
        .await?;
    let play = played_entry(current, directory.entries()).unwrap_or(0);
    write_directory(link, &changed, play).await
}

/// Writes `directory`, then checks that the mouse plays position `play`.
async fn write_directory(
    link: &G700Link,
    directory: &ProfileDirectory,
    play: usize,
) -> Result<(), G700Error> {
    let play = CurrentProfile::Index(u8::try_from(play).map_err(|_| G700Error::DirectoryFull)?);
    g700::memory_writer(link.chan(), link.device_index())
        .write_directory(directory, play)
        .await?;
    let actual = current_profile(link).await?;
    if actual == play {
        Ok(())
    } else {
        Err(G700Error::ProfileNotApplied {
            requested: play,
            actual,
        })
    }
}

#[cfg(test)]
mod tests;
