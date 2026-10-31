//! Writing onboard memory: the `0xA0` memory-management register, and the
//! guarded sequence that stages a page in RAM and copies it to flash.
//!
//! Reverse engineered; follows libratbag's `hidpp10_set_profile`,
//! `hidpp10_write_profile_directory`, `hidpp10_erase_memory` and
//! `hidpp10_write_flash` (`src/hidpp10.c`, MIT). A long `SET_LONG_REGISTER`
//! to `0xA0` either erases a flash page (command `02`) or copies bytes from
//! one memory location to another (command `03`): each location a page and a
//! 16-bit word offset, the size in bytes, big-endian. Data reaches the RAM
//! page through the HOT upload, half a page at a time, so a page is written
//! as: upload the first half, erase the page, copy the half over, upload the
//! second half, copy it to word `0x80`.
//!
//! The erase comes before the first copy, so a write cut short leaves the
//! page half written. Like libratbag, [`MemoryWriter`] first switches the
//! device to its factory profile, which also keeps the profile buttons from
//! selecting the page meanwhile, and selects the previous profile again only
//! once the page reads back as written. Beyond libratbag it reads the RAM
//! page back after each upload, so a transfer the device took wrongly stops
//! before anything is erased or copied, and it refuses pages whose content
//! it cannot account for.

use std::{fmt, time::Duration};

use thiserror::Error;

use super::{
    HALF_PAGE, HALF_PAGE_LEN, Page, RAM_PAGE, READ_CHUNK,
    directory::{DIRECTORY_PAGE, ProfileDirectory},
    hot, read_memory, read_page,
};
use crate::{
    channel::{HidppChannel, HidppMessage, LONG_REPORT_LENGTH, SEND_RESPONSE_TIMEOUT},
    protocol::v10::Hidpp10Error,
    register::current_profile::{CurrentProfile, read_current_profile, write_current_profile},
};

/// The memory-management register's address.
const MEMORY_REGISTER: u8 = 0xa0;

/// The command byte that erases a page.
const ERASE: u8 = 0x02;

/// The command byte that copies memory.
const COPY: u8 = 0x03;

/// The word offset where the second half of a page starts.
const SECOND_HALF_WORD: u8 = 0x80;

const _: () = assert!(SECOND_HALF_WORD as usize * 2 == HALF_PAGE);

/// One half of a page, as a write moves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Half {
    /// Bytes `0..256`.
    First,
    /// Bytes `256..512`, the CRC among them.
    Second,
}

impl Half {
    /// The word offset the half starts at.
    fn word_offset(self) -> u8 {
        match self {
            Self::First => 0x00,
            Self::Second => SECOND_HALF_WORD,
        }
    }

    /// This half of `page`.
    fn of(self, page: &Page) -> &[u8; HALF_PAGE] {
        let (halves, _) = page.as_bytes().as_chunks::<HALF_PAGE>();
        match self {
            Self::First => &halves[0],
            Self::Second => &halves[1],
        }
    }
}

impl fmt::Display for Half {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::First => "first half",
            Self::Second => "second half",
        })
    }
}

/// A `0xA0` command, as the write sequence sends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryCommand {
    /// Erase `page` to all `0xFF`.
    Erase { page: u8 },
    /// Copy half a page from the start of [`RAM_PAGE`] into `half` of `page`.
    CopyFromRam { page: u8, half: Half },
}

impl MemoryCommand {
    /// The sixteen bytes the `SET_LONG_REGISTER` carries.
    fn to_register(self) -> [u8; 16] {
        let mut data = [0; 16];
        match self {
            Self::Erase { page } => {
                data[0] = ERASE;
                data[6] = page;
            }
            Self::CopyFromRam { page, half } => {
                data[0] = COPY;
                data[2] = RAM_PAGE;
                data[3] = 0x00;
                data[6] = page;
                data[7] = half.word_offset();
                data[10..12].copy_from_slice(&HALF_PAGE_LEN.to_be_bytes());
            }
        }
        data
    }
}

/// What a page ends up holding.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Image {
    /// Erased flash, as [`Page::erased`].
    Erased,
    /// These bytes, CRC included.
    Written(Box<Page>),
}

impl Image {
    /// What the page reads back as once it holds this image.
    fn expected(&self) -> Page {
        match self {
            Self::Erased => Page::erased(),
            Self::Written(page) => Page::clone(page),
        }
    }
}

/// A page as a backup holds it, checked fit to go back with
/// [`MemoryWriter::restore_page`]: erased flash, or bytes whose CRC holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PageImage(Image);

impl PageImage {
    /// Checks a backed-up page. A page that is neither erased nor carries a
    /// valid CRC cannot go back as it was: rewriting its CRC would change
    /// it, keeping the CRC would write a page the device may reject.
    pub fn from_backup(page: Page) -> Result<Self, UnrestorablePage> {
        if page.is_erased() {
            Ok(Self(Image::Erased))
        } else if page.has_valid_crc() {
            Ok(Self(Image::Written(Box::new(page))))
        } else {
            Err(UnrestorablePage {
                stored: page.stored_crc(),
                computed: page.computed_crc(),
            })
        }
    }

    /// Whether the image is erased flash, which a restore puts back with an
    /// erase alone.
    #[must_use]
    pub fn is_erased(&self) -> bool {
        self.0 == Image::Erased
    }

    /// The page as it reads once restored.
    #[must_use]
    pub fn to_page(&self) -> Page {
        self.0.expected()
    }
}

/// Why [`PageImage::from_backup`] refused a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Error)]
#[error(
    "the page is neither erased nor carries a valid CRC (stored {stored:#06x}, computed {computed:#06x})"
)]
pub struct UnrestorablePage {
    /// The CRC the page carries.
    pub stored: u16,
    /// The CRC its content calls for.
    pub computed: u16,
}

/// Why [`MemoryWriter`] refused a page. Nothing was sent to the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Error)]
#[non_exhaustive]
pub enum Refusal {
    /// The page is [`RAM_PAGE`], which every write is staged in.
    #[error("it is the RAM page that writes are staged in")]
    RamPage,
    /// The channel cannot send the long reports a write needs.
    #[error("the channel cannot send long HID++ reports")]
    NoLongReports,
    /// The page is neither the directory, a page the directory lists, nor
    /// erased flash. Its purpose is not known, so only a restore may write
    /// it.
    #[error("it is neither the directory, a page the directory lists, nor erased")]
    UnknownContent,
}

/// The step of a page write that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WriteStep {
    /// Switching the device to its factory profile.
    SelectFactory,
    /// Uploading a half to the RAM page.
    Upload(Half),
    /// Reading the RAM page back after an upload.
    VerifyUpload(Half),
    /// Erasing the page.
    Erase,
    /// Copying a half from the RAM page into the page.
    Copy(Half),
    /// Reading the page back.
    ReadBack,
}

impl WriteStep {
    /// What a failure at this step leaves the page holding.
    #[must_use]
    pub fn page_state(self) -> PageState {
        match self {
            Self::SelectFactory | Self::Upload(Half::First) | Self::VerifyUpload(Half::First) => {
                PageState::Untouched
            }
            Self::Erase => PageState::MaybeErased,
            Self::Copy(_) | Self::Upload(Half::Second) | Self::VerifyUpload(Half::Second) => {
                PageState::PartWritten
            }
            Self::ReadBack => PageState::Unverified,
        }
    }
}

impl fmt::Display for WriteStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelectFactory => f.write_str("selecting the factory profile"),
            Self::Upload(half) => write!(f, "uploading the {half}"),
            Self::VerifyUpload(half) => write!(f, "verifying the uploaded {half}"),
            Self::Erase => f.write_str("erasing the page"),
            Self::Copy(half) => write!(f, "copying the {half}"),
            Self::ReadBack => f.write_str("reading the page back"),
        }
    }
}

/// What a page holds after a write failed at a [`WriteStep`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageState {
    /// Nothing reached the page; it holds what it held before.
    Untouched,
    /// The erase may or may not have reached the page: it holds what it held
    /// before, or erased flash.
    MaybeErased,
    /// The page was erased and holds part of the new content at most; it no
    /// longer holds what it held before.
    PartWritten,
    /// The whole page was written but did not read back, so it is not known
    /// to hold the new content.
    Unverified,
}

impl fmt::Display for PageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Untouched => "the page is untouched",
            Self::MaybeErased => "the page may be erased",
            Self::PartWritten => "the page is left part-written",
            Self::Unverified => "the page is written but unverified",
        })
    }
}

/// Why a [`WriteStep`] failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StepError {
    /// A register access failed.
    #[error("a register access failed: {0:?}")]
    Device(#[source] Hidpp10Error),
    /// The upload's reset was not acknowledged.
    #[error("the upload reset failed: {0:?}")]
    Reset(#[source] Hidpp10Error),
    /// A chunk of the upload was refused or not acknowledged in time.
    #[error("chunk {chunk} of the upload failed: {source:?}")]
    Chunk {
        /// The chunk's sequence number.
        chunk: u8,
        /// What went wrong.
        #[source]
        source: Hidpp10Error,
    },
    /// The device acknowledged another chunk than the one sent.
    #[error("the device acknowledged chunk {acknowledged} in place of chunk {chunk}")]
    UnexpectedAck {
        /// The chunk sent.
        chunk: u8,
        /// The sequence number the acknowledgement carried.
        acknowledged: u8,
    },
    /// The RAM page does not hold what was uploaded.
    #[error("the RAM page differs from the upload from byte {offset} of the half")]
    UploadDiffers {
        /// The first differing byte, counted from the start of the half.
        offset: usize,
    },
}

/// Why [`MemoryWriter`] did not complete a write.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PageWriteError {
    /// The page may not be written. Nothing was sent.
    #[error("page {page} may not be written: {reason}")]
    Refused {
        /// The page.
        page: u8,
        /// Why.
        reason: Refusal,
    },
    /// A page the write must check first did not read — the target, which
    /// is how a page past the end of the device's memory shows, or the
    /// directory. Nothing was written.
    #[error("page {page} did not read, so nothing was written: {source:?}")]
    Unreadable {
        /// The page that did not read.
        page: u8,
        /// Why.
        #[source]
        source: Hidpp10Error,
    },
    /// The current profile did not read, so it could not be selected again
    /// afterwards. Nothing was written.
    #[error("the current profile did not read, so nothing was written: {0:?}")]
    CurrentProfileUnreadable(#[source] Hidpp10Error),
    /// A step of the write failed. The device is left on its factory
    /// profile — when the failed step is [`WriteStep::SelectFactory`] itself,
    /// on either that or the previous profile — and the page as
    /// [`WriteStep::page_state`] says.
    #[error(
        "writing page {page} failed while {step}; {}; the device is left on its factory profile{}",
        .step.page_state(),
        if *.step == WriteStep::SelectFactory { " or the previous one" } else { "" }
    )]
    Failed {
        /// The page.
        page: u8,
        /// The step that failed.
        step: WriteStep,
        /// Why.
        #[source]
        source: StepError,
    },
    /// The page reads back different from what was written. The device is
    /// left on its factory profile.
    #[error(
        "page {page} reads back different from what was written, from byte {offset}; the device is left on its factory profile"
    )]
    Mismatch {
        /// The page.
        page: u8,
        /// The first differing byte.
        offset: usize,
    },
    /// The page was written and reads back as written, but selecting the
    /// previous profile again failed.
    #[error("page {page} was written, but selecting {profile:?} again failed: {source:?}")]
    Reselect {
        /// The page.
        page: u8,
        /// The profile that was to be selected.
        profile: CurrentProfile,
        /// Why.
        #[source]
        source: Hidpp10Error,
    },
}

/// Which pages a write may touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Guard {
    /// The directory, a page it lists, or erased flash.
    KnownContent,
    /// Any page the device has, as a restore of its own backup needs.
    AnyPage,
}

/// What the device's memory says a page is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Directory,
    Listed,
    Erased,
    Unknown,
}

impl Role {
    fn of(page: u8, current: &Page, directory: &ProfileDirectory) -> Self {
        if page == DIRECTORY_PAGE {
            Self::Directory
        } else if directory.entries().iter().any(|entry| entry.page == page) {
            Self::Listed
        } else if current.is_erased() {
            Self::Erased
        } else {
            Self::Unknown
        }
    }
}

/// The profile to select once `directory` is written in place of the one
/// that held `previous`: `previous` while the new directory still has it,
/// otherwise its first profile, or the factory profile when it lists none.
fn reselection(previous: CurrentProfile, directory: &ProfileDirectory) -> CurrentProfile {
    let entries = directory.entries();
    let kept = match previous {
        CurrentProfile::Index(index) => usize::from(index) < entries.len(),
        CurrentProfile::Address { page, offset } => entries
            .iter()
            .any(|entry| entry.page == page && entry.offset == offset),
        CurrentProfile::Factory => true,
    };
    if kept {
        previous
    } else if entries.is_empty() {
        CurrentProfile::Factory
    } else {
        CurrentProfile::Index(0)
    }
}

/// The first position where `a` and `b` differ.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
}

/// Writes pages of one device's onboard memory, guarded.
///
/// Every write reads the target page and the directory first and refuses a
/// page it cannot account for (see [`Refusal`]); a page the device will not
/// read — past the end of its memory, such as pages 27 to 31 of a G700 — is
/// [`PageWriteError::Unreadable`]. It then reads the current profile, selects
/// the factory profile, writes, reads the page back, and on a match selects
/// the previous profile again. Any failure after the switch leaves the device
/// on its factory profile, which plays no page from flash.
pub struct MemoryWriter<'a> {
    chan: &'a HidppChannel,
    device_index: u8,
    profile_count: usize,
    ack_timeout: Duration,
}

impl<'a> MemoryWriter<'a> {
    /// A writer for the device at `device_index`, whose directory holds up
    /// to `profile_count` profiles (see [`ProfileDirectory::parse`]).
    #[must_use]
    pub fn new(chan: &'a HidppChannel, device_index: u8, profile_count: usize) -> Self {
        Self {
            chan,
            device_index,
            profile_count,
            ack_timeout: SEND_RESPONSE_TIMEOUT,
        }
    }

    /// [`Self::new`], waiting at most `ack_timeout` for each upload chunk's
    /// acknowledgement.
    #[cfg(test)]
    fn with_ack_timeout(mut self, ack_timeout: Duration) -> Self {
        self.ack_timeout = ack_timeout;
        self
    }

    /// Writes `content` to `page`, its CRC recomputed first. The page must
    /// be the directory, a page the directory lists, or erased flash.
    pub async fn write_page(&self, page: u8, content: &Page) -> Result<(), PageWriteError> {
        let mut content = content.clone();
        content.update_crc();
        self.put(
            page,
            &Image::Written(Box::new(content)),
            Guard::KnownContent,
        )
        .await
    }

    /// Writes `directory` to the directory page. When it no longer holds the
    /// profile the device played, its first profile is selected instead, or
    /// the factory profile when it lists none.
    pub async fn write_directory(
        &self,
        directory: &ProfileDirectory,
    ) -> Result<(), PageWriteError> {
        self.write_page(DIRECTORY_PAGE, &directory.to_page()).await
    }

    /// Puts a backed-up page back: an erase alone for erased flash, a write
    /// of the bytes as they were otherwise. Unlike [`Self::write_page`], any
    /// page the device reads may be restored, so a backup of this same
    /// device is the only fit source.
    pub async fn restore_page(&self, page: u8, image: &PageImage) -> Result<(), PageWriteError> {
        self.put(page, &image.0, Guard::AnyPage).await
    }

    async fn put(&self, page: u8, image: &Image, guard: Guard) -> Result<(), PageWriteError> {
        if page == RAM_PAGE {
            return Err(PageWriteError::Refused {
                page,
                reason: Refusal::RamPage,
            });
        }
        if !self
            .chan
            .supports_msg(&HidppMessage::Long([0; LONG_REPORT_LENGTH - 1]))
        {
            return Err(PageWriteError::Refused {
                page,
                reason: Refusal::NoLongReports,
            });
        }

        let current = self.read(page).await?;
        let directory_page = if page == DIRECTORY_PAGE {
            current.clone()
        } else {
            self.read(DIRECTORY_PAGE).await?
        };
        let directory = ProfileDirectory::parse(&directory_page, self.profile_count);
        if guard == Guard::KnownContent && Role::of(page, &current, &directory) == Role::Unknown {
            return Err(PageWriteError::Refused {
                page,
                reason: Refusal::UnknownContent,
            });
        }
        let previous = read_current_profile(self.chan, self.device_index)
            .await
            .map_err(PageWriteError::CurrentProfileUnreadable)?;

        let failed = |step| move |source| PageWriteError::Failed { page, step, source };
        write_current_profile(self.chan, self.device_index, CurrentProfile::Factory)
            .await
            .map_err(StepError::Device)
            .map_err(failed(WriteStep::SelectFactory))?;
        match image {
            Image::Erased => self
                .command(MemoryCommand::Erase { page })
                .await
                .map_err(failed(WriteStep::Erase))?,
            Image::Written(content) => {
                for half in [Half::First, Half::Second] {
                    let data = half.of(content);
                    hot::upload(self.chan, self.device_index, data, self.ack_timeout)
                        .await
                        .map_err(failed(WriteStep::Upload(half)))?;
                    self.verify_upload(data)
                        .await
                        .map_err(failed(WriteStep::VerifyUpload(half)))?;
                    if half == Half::First {
                        self.command(MemoryCommand::Erase { page })
                            .await
                            .map_err(failed(WriteStep::Erase))?;
                    }
                    self.command(MemoryCommand::CopyFromRam { page, half })
                        .await
                        .map_err(failed(WriteStep::Copy(half)))?;
                }
            }
        }

        let expected = image.expected();
        let written = read_page(self.chan, self.device_index, page)
            .await
            .map_err(StepError::Device)
            .map_err(failed(WriteStep::ReadBack))?;
        if let Some(offset) = first_difference(written.as_bytes(), expected.as_bytes()) {
            return Err(PageWriteError::Mismatch { page, offset });
        }

        let profile = if page == DIRECTORY_PAGE {
            reselection(
                previous,
                &ProfileDirectory::parse(&expected, self.profile_count),
            )
        } else {
            previous
        };
        if profile != CurrentProfile::Factory {
            write_current_profile(self.chan, self.device_index, profile)
                .await
                .map_err(|source| PageWriteError::Reselect {
                    page,
                    profile,
                    source,
                })?;
        }
        Ok(())
    }

    async fn read(&self, page: u8) -> Result<Page, PageWriteError> {
        read_page(self.chan, self.device_index, page)
            .await
            .map_err(|source| PageWriteError::Unreadable { page, source })
    }

    async fn command(&self, command: MemoryCommand) -> Result<(), StepError> {
        self.chan
            .write_long_register(self.device_index, MEMORY_REGISTER, command.to_register())
            .await
            .map_err(StepError::Device)
    }

    /// Reads back the half page an upload put at the start of the RAM page.
    async fn verify_upload(&self, data: &[u8; HALF_PAGE]) -> Result<(), StepError> {
        let (chunks, _) = data.as_chunks::<READ_CHUNK>();
        for (word_offset, expected) in (0..=u8::MAX).step_by(READ_CHUNK / 2).zip(chunks) {
            let read = read_memory(self.chan, self.device_index, RAM_PAGE, word_offset)
                .await
                .map_err(StepError::Device)?;
            if let Some(position) = first_difference(&read, expected) {
                return Err(StepError::UploadDiffers {
                    offset: usize::from(word_offset) * 2 + position,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
