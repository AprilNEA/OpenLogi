//! Writing a G700's settings registers and onboard memory.
//!
//! Every page goes through `hidpp`'s guarded `MemoryWriter`, which refuses
//! pages it cannot account for, stages and verifies the write and reads the
//! page back; this module decides what to write. Every register write is
//! read back and compared.

use std::fmt;

use hidpp::{
    channel::HidppChannel,
    protocol::v10::Hidpp10Error,
    register::{
        current_profile::{CurrentProfile, read_current_profile, write_current_profile},
        onboard_memory::{
            Page, PageImage, PageWriteError, RAM_PAGE, UnrestorablePage,
            directory::DIRECTORY_PAGE,
            g700::{self, DPI_MODE_COUNT, DpiMode, G700Profile, dpi_from_raw},
            read_page,
        },
        report_rate::{ReportInterval, read_report_interval, write_report_interval},
        resolution::{Resolution, read_current_resolution, write_current_resolution},
    },
};
use thiserror::Error;

use super::{G700Identity, G700Link, read_identity};
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use crate::write::WriteError;

/// A G700 settings register a write went to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum G700Register {
    /// The profile the mouse plays.
    CurrentProfile,
    /// The resolution the sensor runs at.
    CurrentResolution,
    /// The USB report interval.
    ReportInterval,
}

impl fmt::Display for G700Register {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CurrentProfile => "the current-profile register",
            Self::CurrentResolution => "the current-resolution register",
            Self::ReportInterval => "the report-interval register",
        })
    }
}

/// Why a G700 write did not complete.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum G700Error {
    /// The mouse was not found or did not answer.
    #[error(transparent)]
    Device(#[from] WriteError),
    /// Reading a register failed.
    #[error("reading {register} failed: {source:?}")]
    RegisterRead {
        /// The register.
        register: G700Register,
        /// Why.
        #[source]
        source: Hidpp10Error,
    },
    /// Writing a register failed.
    #[error("writing {register} failed: {source:?}")]
    RegisterWrite {
        /// The register.
        register: G700Register,
        /// Why.
        #[source]
        source: Hidpp10Error,
    },
    /// The mouse acknowledged a profile selection but plays another.
    #[error("the mouse plays {actual:?} after selecting {requested:?}")]
    ProfileNotApplied {
        /// What was selected.
        requested: CurrentProfile,
        /// What the register reads back.
        actual: CurrentProfile,
    },
    /// The mouse acknowledged a resolution write but runs at another.
    #[error("the sensor runs at {actual:?} after a write of {requested:?}")]
    ResolutionNotApplied {
        /// What was written.
        requested: Resolution,
        /// What the register reads back.
        actual: Resolution,
    },
    /// The mouse acknowledged a report-interval write but reads back
    /// another.
    #[error("the report interval reads {actual:?} after a write of {requested:?}")]
    ReportIntervalNotApplied {
        /// What was written.
        requested: ReportInterval,
        /// What the register reads back.
        actual: ReportInterval,
    },
    /// A page that decides what to write did not read.
    #[error("page {page} did not read: {source:?}")]
    PageRead {
        /// The page.
        page: u8,
        /// Why.
        #[source]
        source: Hidpp10Error,
    },
    /// A page a write starts from carries no valid CRC and read differently
    /// twice, so there are no bytes to trust as the base of the write.
    #[error("page {page} has no valid CRC and reads differently each time")]
    UnstablePage {
        /// The page.
        page: u8,
    },
    /// A page write was refused or failed.
    #[error(transparent)]
    PageWrite(#[from] PageWriteError),
    /// The directory lists no profile at this index.
    #[error("profile {index} is not enabled: the directory lists {enabled}")]
    NoSuchProfile {
        /// The requested index.
        index: u8,
        /// How many profiles the directory lists.
        enabled: usize,
    },
    /// A profile has no DPI mode at this index.
    #[error("there is no DPI mode {mode}: a profile has {DPI_MODE_COUNT}")]
    NoSuchDpiMode {
        /// The requested mode.
        mode: usize,
    },
    /// The raw sensor step has no DPI table entry.
    #[error("raw DPI step {raw} has no table entry")]
    InvalidDpiStep {
        /// The step.
        raw: u8,
    },
    /// The RAM page was asked to be restored; it never is.
    #[error("page {RAM_PAGE} is RAM and is never restored")]
    RamPage,
    /// The mouse at the route is not the one the backup was taken from.
    #[error("the backup is of another mouse: {}", IdentityDiff(.backup, .device))]
    IdentityMismatch {
        /// What the backup records.
        backup: Box<G700Identity>,
        /// What the mouse reads as.
        device: Box<G700Identity>,
    },
    /// The interval is not one a profile can store: a whole number of
    /// milliseconds that is a rate of 125, 250, 500 or 1000 Hz.
    #[error("{millis} ms is not a report interval a profile stores")]
    InvalidReportInterval {
        /// The interval.
        millis: u8,
    },
    /// A page differs from its backup, and the backup cannot be put back.
    #[error("page {page} differs from the backup, whose copy cannot be restored: {source}")]
    Unrestorable {
        /// The page.
        page: u8,
        /// Why.
        #[source]
        source: UnrestorablePage,
    },
}

/// What a restore did to one page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RestoreOutcome {
    /// The page already matched the backup; nothing was written.
    Identical,
    /// The backup holds erased flash; the page was erased.
    Erased,
    /// The page was written from the backup.
    Written,
}

/// One page of a restore.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RestoredPage {
    /// The page.
    pub page: u8,
    /// What became of it.
    pub outcome: RestoreOutcome,
}

/// One step a restore plans.
#[derive(Debug, PartialEq, Eq)]
enum RestoreStep {
    /// The page matches the backup.
    Keep(u8),
    /// The page goes back to the image.
    Put(u8, PageImage),
}

/// Plans a restore of `pages`, each `(page, backup, device)`, before anything
/// is written: a page the device already holds is kept, any other must have
/// a backup fit to go back. Pages that get content go first, then the
/// directory, then pages that are only erased, so at no point does the
/// directory list a page that is not back yet or one already erased: while
/// it lists one, the mouse may play it.
fn plan_restore(pages: Vec<(u8, Page, Page)>) -> Result<Vec<RestoreStep>, G700Error> {
    let mut steps = pages
        .into_iter()
        .map(|(page, backup, device)| {
            if backup == device {
                Ok(RestoreStep::Keep(page))
            } else {
                PageImage::from_backup(backup)
                    .map(|image| RestoreStep::Put(page, image))
                    .map_err(|source| G700Error::Unrestorable { page, source })
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    steps.sort_by_key(|step| {
        let (RestoreStep::Keep(page) | RestoreStep::Put(page, _)) = *step;
        let erase = matches!(step, RestoreStep::Put(_, image) if image.is_erased());
        let rank = match (page == DIRECTORY_PAGE, erase) {
            (true, _) => 1,
            (false, false) => 0,
            (false, true) => 2,
        };
        (rank, page)
    });
    Ok(steps)
}

/// Whose backup a restore may put back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackupOrigin {
    /// The mouse this identity names; any other is refused before a write.
    Identified(G700Identity),
    /// Any mouse — the caller vouches for the backup.
    Forced,
}

/// Put `backup` pages back where they differ on the G700 at `route`. The
/// backup must be of this same mouse — pages of unknown purpose are restored
/// too — so unless `origin` is [`BackupOrigin::Forced`] the mouse's identity
/// is read first and a mismatch refused before any write.
///
/// Every page is read and checked against its backup first; a page that
/// differs and whose backup is neither erased nor carries a valid CRC fails
/// the restore before anything is written. The RAM page is refused. Pages
/// are written one at a time, each verified by reading it back; on a failure
/// the pages before it are back and the rest untouched.
pub async fn restore_g700_pages(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    origin: &BackupOrigin,
    backup: Vec<(u8, Page)>,
) -> Result<Vec<RestoredPage>, G700Error> {
    if backup.iter().any(|(page, _)| *page == RAM_PAGE) {
        return Err(G700Error::RamPage);
    }
    let link = G700Link::open(backend, route).await?;
    if let BackupOrigin::Identified(expected) = origin {
        let device = read_identity(&link).await?;
        if device != *expected {
            return Err(G700Error::IdentityMismatch {
                backup: Box::new(expected.clone()),
                device: Box::new(device),
            });
        }
    }
    restore_on(link.chan(), link.device_index(), backup).await
}

/// The fields in which two identities differ, for [`G700Error`]'s message.
struct IdentityDiff<'a>(&'a G700Identity, &'a G700Identity);

impl fmt::Display for IdentityDiff<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (backup, device) = (self.0, self.1);
        let mut differences = Vec::new();
        if backup.route != device.route {
            differences.push(format!("route {} vs {}", backup.route, device.route));
        }
        if backup.device_index != device.device_index {
            differences.push(format!(
                "device index {} vs {}",
                backup.device_index, device.device_index
            ));
        }
        if backup.readable_pages != device.readable_pages {
            differences.push(format!(
                "{} vs {} readable pages",
                backup.readable_pages, device.readable_pages
            ));
        }
        if backup.directory != device.directory {
            differences.push(format!(
                "directory {:?} vs {:?}",
                backup.directory, device.directory
            ));
        }
        write!(f, "{} (backup vs mouse)", differences.join(", "))
    }
}

async fn restore_on(
    chan: &HidppChannel,
    device_index: u8,
    backup: Vec<(u8, Page)>,
) -> Result<Vec<RestoredPage>, G700Error> {
    let mut pages = Vec::with_capacity(backup.len());
    for (page, saved) in backup {
        let device = read(chan, device_index, page).await?;
        pages.push((page, saved, device));
    }
    let writer = g700::memory_writer(chan, device_index);
    let mut restored = Vec::new();
    for step in plan_restore(pages)? {
        restored.push(match step {
            RestoreStep::Keep(page) => RestoredPage {
                page,
                outcome: RestoreOutcome::Identical,
            },
            RestoreStep::Put(page, image) => {
                writer.restore_page(page, &image).await?;
                RestoredPage {
                    page,
                    outcome: if image.is_erased() {
                        RestoreOutcome::Erased
                    } else {
                        RestoreOutcome::Written
                    },
                }
            }
        });
    }
    Ok(restored)
}

/// A new resolution for one DPI mode of one onboard profile, in raw sensor
/// steps (see [`dpi_from_raw`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProfileDpiChange {
    /// The profile's directory index.
    pub profile: u8,
    /// The DPI mode, `0..`[`DPI_MODE_COUNT`].
    pub mode: usize,
    /// The X-axis step.
    pub x: u8,
    /// The Y-axis step.
    pub y: u8,
}

/// The DPI mode a [`set_g700_profile_dpi`] changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProfileDpiWrite {
    /// The page the profile lives on.
    pub page: u8,
    /// The mode as it was.
    pub before: DpiMode,
    /// The mode as written.
    pub after: DpiMode,
}

impl ProfileDpiChange {
    /// Checks the change before any I/O.
    fn validate(self) -> Result<Self, G700Error> {
        if self.mode >= DPI_MODE_COUNT {
            return Err(G700Error::NoSuchDpiMode { mode: self.mode });
        }
        for raw in [self.x, self.y] {
            if dpi_from_raw(raw).is_none() {
                return Err(G700Error::InvalidDpiStep { raw });
            }
        }
        Ok(self)
    }

    /// `page` with this change applied, and the mode before and after.
    fn apply(self, page: &Page) -> (Page, DpiMode, DpiMode) {
        let mut profile = G700Profile::parse(page);
        let before = profile.dpi_modes[self.mode];
        let after = DpiMode {
            x: self.x,
            y: self.y,
            ..before
        };
        profile.dpi_modes[self.mode] = after;
        (profile.to_page(), before, after)
    }
}

/// Change one DPI mode of one of the onboard profiles of the G700 at
/// `route`; every other byte of the profile page stays as it was.
pub async fn set_g700_profile_dpi(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    change: ProfileDpiChange,
) -> Result<ProfileDpiWrite, G700Error> {
    let change = change.validate()?;
    let link = G700Link::open(backend, route).await?;
    set_profile_dpi_on(link.chan(), link.device_index(), change).await
}

async fn set_profile_dpi_on(
    chan: &HidppChannel,
    device_index: u8,
    change: ProfileDpiChange,
) -> Result<ProfileDpiWrite, G700Error> {
    let (page, (before, after)) = rewrite_profile(chan, device_index, change.profile, |page| {
        let (page, before, after) = change.apply(page);
        (page, (before, after))
    })
    .await?;
    Ok(ProfileDpiWrite {
        page,
        before,
        after,
    })
}

/// A new report interval for one onboard profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProfileRateChange {
    /// The profile's directory index.
    pub profile: u8,
    /// The interval the profile plays at.
    pub interval: ReportInterval,
}

/// The report interval a [`set_g700_profile_report_interval`] changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProfileRateWrite {
    /// The page the profile lives on.
    pub page: u8,
    /// The interval byte as it was; a profile may hold one that names no
    /// interval.
    pub before: u8,
    /// The interval as written.
    pub after: ReportInterval,
}

/// Change the report interval one of the onboard profiles of the G700 at
/// `route` plays at; every other byte of the profile page stays as it was.
/// The mouse takes it up the next time it selects the profile, which for
/// the profile it plays means the next power-up or profile switch.
pub async fn set_g700_profile_report_interval(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    change: ProfileRateChange,
) -> Result<ProfileRateWrite, G700Error> {
    if !profile_interval_valid(change.interval) {
        return Err(G700Error::InvalidReportInterval {
            millis: change.interval.millis(),
        });
    }
    let link = G700Link::open(backend, route).await?;
    set_profile_rate_on(link.chan(), link.device_index(), change).await
}

async fn set_profile_rate_on(
    chan: &HidppChannel,
    device_index: u8,
    change: ProfileRateChange,
) -> Result<ProfileRateWrite, G700Error> {
    let (page, before) = rewrite_profile(chan, device_index, change.profile, |page| {
        let mut profile = G700Profile::parse(page);
        let before = profile.report_interval_millis;
        profile.report_interval_millis = change.interval.millis();
        (profile.to_page(), before)
    })
    .await?;
    Ok(ProfileRateWrite {
        page,
        before,
        after: change.interval,
    })
}

/// Whether a profile may store `interval`: the rates the G700 offers, as
/// libratbag's `ReportRates=125;250;500;1000` has them.
pub(super) fn profile_interval_valid(interval: ReportInterval) -> bool {
    matches!(interval.millis(), 1 | 2 | 4 | 8)
}

/// Rewrites the page of directory profile `profile` with what `edit` makes
/// of it, and returns the page number with what `edit` returned beside the
/// page. The base is read so it can be trusted (see [`read_trusted`]).
async fn rewrite_profile<T>(
    chan: &HidppChannel,
    device_index: u8,
    profile: u8,
    edit: impl FnOnce(&Page) -> (Page, T),
) -> Result<(u8, T), G700Error> {
    let directory = g700::parse_directory(&read(chan, device_index, DIRECTORY_PAGE).await?);
    let entry = directory
        .entries()
        .get(usize::from(profile))
        .ok_or(G700Error::NoSuchProfile {
            index: profile,
            enabled: directory.entries().len(),
        })?;
    let base = read_trusted(chan, device_index, entry.page).await?;
    let (page, edited) = edit(&base);
    g700::memory_writer(chan, device_index)
        .write_page(entry.page, &page)
        .await?;
    Ok((entry.page, edited))
}

/// Set the report interval of the G700 at `route`, until it next switches
/// profile; returns the interval it reads back.
pub async fn set_g700_report_interval(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    interval: ReportInterval,
) -> Result<ReportInterval, G700Error> {
    let link = G700Link::open(backend, route).await?;
    set_report_interval_on(link.chan(), link.device_index(), interval).await
}

pub(super) async fn set_report_interval_on(
    chan: &HidppChannel,
    device_index: u8,
    interval: ReportInterval,
) -> Result<ReportInterval, G700Error> {
    let register = G700Register::ReportInterval;
    write_report_interval(chan, device_index, interval)
        .await
        .map_err(|source| G700Error::RegisterWrite { register, source })?;
    let actual = read_report_interval(chan, device_index)
        .await
        .map_err(|source| G700Error::RegisterRead { register, source })?;
    applied(interval, actual, |requested, actual| {
        G700Error::ReportIntervalNotApplied { requested, actual }
    })
}

/// Set the resolution the sensor of the G700 at `route` runs at, until it
/// next switches DPI mode or profile; returns the resolution it reads back.
pub async fn set_g700_resolution(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    resolution: Resolution,
) -> Result<Resolution, G700Error> {
    let link = G700Link::open(backend, route).await?;
    set_resolution_on(link.chan(), link.device_index(), resolution).await
}

pub(super) async fn set_resolution_on(
    chan: &HidppChannel,
    device_index: u8,
    resolution: Resolution,
) -> Result<Resolution, G700Error> {
    let register = G700Register::CurrentResolution;
    write_current_resolution(chan, device_index, resolution)
        .await
        .map_err(|source| G700Error::RegisterWrite { register, source })?;
    let actual = read_current_resolution(chan, device_index)
        .await
        .map_err(|source| G700Error::RegisterRead { register, source })?;
    applied(resolution, actual, |requested, actual| {
        G700Error::ResolutionNotApplied { requested, actual }
    })
}

/// Make the G700 at `route` play `profile`: an index the directory lists,
/// or the factory profile. Returns the profile it reads back.
pub async fn set_g700_current_profile(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    profile: CurrentProfile,
) -> Result<CurrentProfile, G700Error> {
    let link = G700Link::open(backend, route).await?;
    set_current_profile_on(link.chan(), link.device_index(), profile).await
}

async fn set_current_profile_on(
    chan: &HidppChannel,
    device_index: u8,
    profile: CurrentProfile,
) -> Result<CurrentProfile, G700Error> {
    if let CurrentProfile::Index(index) = profile {
        let directory = g700::parse_directory(&read(chan, device_index, DIRECTORY_PAGE).await?);
        let enabled = directory.entries().len();
        if usize::from(index) >= enabled {
            return Err(G700Error::NoSuchProfile { index, enabled });
        }
    }
    let register = G700Register::CurrentProfile;
    write_current_profile(chan, device_index, profile)
        .await
        .map_err(|source| G700Error::RegisterWrite { register, source })?;
    let actual = read_current_profile(chan, device_index)
        .await
        .map_err(|source| G700Error::RegisterRead { register, source })?;
    applied(profile, actual, |requested, actual| {
        G700Error::ProfileNotApplied { requested, actual }
    })
}

/// `actual` when it is what was `requested`, else the error `mismatch` makes.
fn applied<T: PartialEq>(
    requested: T,
    actual: T,
    mismatch: impl FnOnce(T, T) -> G700Error,
) -> Result<T, G700Error> {
    if actual == requested {
        Ok(actual)
    } else {
        Err(mismatch(requested, actual))
    }
}

/// Reads `page` as the base of a rewrite. Every `0xA2` reply has the same
/// header, so a late reply can stand in for another chunk, and the rewrite
/// would carry the shifted bytes to flash with a fresh CRC that hides them.
/// A valid CRC vouches for the bytes; without one the page must read the
/// same twice.
pub(super) async fn read_trusted(
    chan: &HidppChannel,
    device_index: u8,
    page: u8,
) -> Result<Page, G700Error> {
    let first = read(chan, device_index, page).await?;
    if first.has_valid_crc() || read(chan, device_index, page).await? == first {
        Ok(first)
    } else {
        Err(G700Error::UnstablePage { page })
    }
}

pub(super) async fn read(
    chan: &HidppChannel,
    device_index: u8,
    page: u8,
) -> Result<Page, G700Error> {
    read_page(chan, device_index, page)
        .await
        .map_err(|source| G700Error::PageRead { page, source })
}

#[cfg(test)]
mod tests;
