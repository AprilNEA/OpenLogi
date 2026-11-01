//! The profile directory, memory page 1.
//!
//! Reverse engineered; follows libratbag's `hidpp10_read_profile_directory`
//! and `hidpp10_write_profile_directory` (`src/hidpp10.c`, MIT). The page
//! opens with one three-byte entry per enabled profile — page, offset, LED
//! mask — ended by an entry whose page byte is `0xFF`. A profile's position
//! in the directory is its index; a profile is enabled exactly when it has an
//! entry. The device's profile count bounds the walk, so a full directory
//! needs no terminator.

use super::{PAGE_SIZE, Page};

/// The memory page that holds the directory.
pub const DIRECTORY_PAGE: u8 = 1;

/// The page byte that ends the entry list.
const TERMINATOR: u8 = 0xff;

/// The size of one entry in bytes.
const ENTRY_SIZE: usize = 3;

/// The most entries a page can hold before its CRC.
const MAX_ENTRIES: usize = (PAGE_SIZE - 2) / ENTRY_SIZE;

/// One enabled profile's directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct DirectoryEntry {
    /// The memory page the profile is stored in.
    pub page: u8,
    /// The offset of the profile within the page. libratbag reads profiles
    /// from the start of their page and only compares this byte against the
    /// current-profile register's address form.
    pub offset: u8,
    /// The LED mask. libratbag ignores it on read and, on write, derives it
    /// from the entry's position `i` as `((0b111 << i) >> 2) & 0b111`
    /// (`1, 3, 7, 6, 4`); a real G700 carries those values too.
    pub led_mask: u8,
}

impl DirectoryEntry {
    fn from_bytes([page, offset, led_mask]: [u8; ENTRY_SIZE]) -> Self {
        Self {
            page,
            offset,
            led_mask,
        }
    }

    fn to_bytes(self) -> [u8; ENTRY_SIZE] {
        [self.page, self.offset, self.led_mask]
    }
}

/// The profile directory, with the page it was read from kept whole so
/// [`Self::to_page`] reproduces every byte the entries do not cover.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ProfileDirectory {
    entries: Vec<DirectoryEntry>,
    /// How many entries the device's profile count allows.
    capacity: usize,
    page: Page,
}

impl ProfileDirectory {
    /// Decodes the directory of a device with `profile_count` profiles
    /// (five on the G700).
    #[must_use]
    pub fn parse(page: &Page, profile_count: usize) -> Self {
        let capacity = profile_count.min(MAX_ENTRIES);
        let entries = page.as_bytes()[..capacity * ENTRY_SIZE]
            .as_chunks::<ENTRY_SIZE>()
            .0
            .iter()
            .copied()
            .take_while(|&[entry_page, ..]| entry_page != TERMINATOR)
            .map(DirectoryEntry::from_bytes)
            .collect();
        Self {
            entries,
            capacity,
            page: page.clone(),
        }
    }

    /// The enabled profiles' entries; the position is the profile index.
    #[must_use]
    pub fn entries(&self) -> &[DirectoryEntry] {
        &self.entries
    }

    /// How many entries the directory can hold.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// This directory without the entry at `index`, or `None` when it has
    /// no such entry. The entries after it move up one position and take
    /// that position's LED mask ([`led_mask_at`]); the bytes the last entry
    /// used become erased flash, so [`Self::to_page`] leaves no stale entry
    /// past the terminator.
    #[must_use]
    pub fn without(&self, index: usize) -> Option<Self> {
        if index >= self.entries.len() {
            return None;
        }
        let mut directory = self.clone();
        directory.entries.remove(index);
        for (position, entry) in directory.entries.iter_mut().enumerate().skip(index) {
            entry.led_mask = led_mask_at(position);
        }
        let vacated = directory.entries.len() * ENTRY_SIZE;
        directory.page.bytes[vacated..vacated + ENTRY_SIZE].fill(TERMINATOR);
        Some(directory)
    }

    /// This directory with an entry for the profile stored at the start of
    /// `page` appended, with its position's LED mask; `None` when the
    /// directory is full or already lists `page`.
    #[must_use]
    pub fn with_appended(&self, page: u8) -> Option<Self> {
        if self.entries.len() >= self.capacity || self.entries.iter().any(|e| e.page == page) {
            return None;
        }
        let mut directory = self.clone();
        let led_mask = led_mask_at(directory.entries.len());
        directory.entries.push(DirectoryEntry {
            page,
            offset: 0,
            led_mask,
        });
        Some(directory)
    }

    /// The directory as a page. For a parsed, unmodified directory this is
    /// the page it came from, byte for byte, CRC included.
    #[must_use]
    pub fn to_page(&self) -> Page {
        let mut page = self.page.clone();
        let mut offset = 0;
        for entry in &self.entries {
            page.bytes[offset..offset + ENTRY_SIZE].copy_from_slice(&entry.to_bytes());
            offset += ENTRY_SIZE;
        }
        if self.entries.len() < self.capacity {
            page.bytes[offset] = TERMINATOR;
        }
        page
    }
}

/// The LED mask of the entry at `position`, as libratbag's
/// `hidpp10_write_profile_directory` derives it: `1, 3, 7, 6, 4`.
#[must_use]
pub fn led_mask_at(position: usize) -> u8 {
    let mask = (0b111u32 << position.min(8)) >> 2;
    mask.to_le_bytes()[0] & 0b111
}

#[cfg(test)]
mod tests {
    use super::{DirectoryEntry, ProfileDirectory, led_mask_at};
    use crate::register::onboard_memory::{PAGE_SIZE, Page};

    fn page_with(prefix: &[u8], fill: u8) -> Page {
        let mut bytes = [fill; PAGE_SIZE];
        bytes[..prefix.len()].copy_from_slice(prefix);
        let mut page = Page::from_bytes(bytes);
        page.update_crc();
        page
    }

    #[test]
    fn entries_end_at_the_terminator() {
        let page = page_with(
            &[0x02, 0x00, 0x01, 0x03, 0x00, 0x02, 0xff, 0x00, 0x00],
            0x00,
        );
        let directory = ProfileDirectory::parse(&page, 5);
        assert_eq!(
            directory.entries(),
            [
                DirectoryEntry {
                    page: 2,
                    offset: 0,
                    led_mask: 1
                },
                DirectoryEntry {
                    page: 3,
                    offset: 0,
                    led_mask: 2
                },
            ]
        );
        assert_eq!(directory.to_page(), page);
    }

    #[test]
    fn the_profile_count_bounds_a_directory_without_terminator() {
        let page = page_with(
            &[2, 0, 0, 3, 0, 0, 4, 0, 0, 5, 0, 0, 6, 0, 0, 7, 0, 0],
            0x5a,
        );
        let directory = ProfileDirectory::parse(&page, 5);
        assert_eq!(directory.entries().len(), 5);
        assert_eq!(directory.entries()[4].page, 6);
        assert_eq!(directory.to_page(), page, "no terminator is invented");
    }

    #[test]
    fn an_erased_directory_has_no_entries() {
        let page = page_with(&[], 0xff);
        let directory = ProfileDirectory::parse(&page, 5);
        assert!(directory.entries().is_empty());
        assert_eq!(directory.to_page(), page);
    }

    #[test]
    fn any_page_round_trips_byte_for_byte() {
        // A pseudo-random page, its CRC deliberately left wrong.
        let mut bytes = [0u8; PAGE_SIZE];
        let mut state = 0x1234_5678u32;
        for byte in &mut bytes {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state.to_le_bytes()[0];
        }
        bytes[0] = 0x02;
        let page = Page::from_bytes(bytes);
        for count in [0, 1, 5, 200] {
            assert_eq!(
                ProfileDirectory::parse(&page, count).to_page(),
                page,
                "{count}"
            );
        }
    }

    /// The directory page of the G700 the 2026-10-31 dump came from: three
    /// entries, the terminator, then bytes Logitech Gaming Software keeps.
    fn dumped() -> Page {
        let mut bytes = [0xff; PAGE_SIZE];
        bytes[..24].copy_from_slice(&[
            0x02, 0x00, 0x01, 0x03, 0x00, 0x03, 0x04, 0x00, 0x07, 0xff, 0xff, 0x4c, 0x47, 0x53,
            0x02, 0x47, 0x53, 0x03, 0x47, 0x53, 0x04, 0xff, 0xff, 0x05,
        ]);
        let mut page = Page::from_bytes(bytes);
        page.update_crc();
        page
    }

    #[test]
    fn led_masks_follow_libratbag() {
        let masks: Vec<u8> = (0..5).map(led_mask_at).collect();
        assert_eq!(masks, [1, 3, 7, 6, 4]);
    }

    #[test]
    fn removing_an_entry_moves_the_rest_up_and_erases_the_last_slot() {
        let directory = ProfileDirectory::parse(&dumped(), 5);

        let without = directory.without(1).unwrap();

        let pages: Vec<u8> = without.entries().iter().map(|e| e.page).collect();
        assert_eq!(pages, [2, 4]);
        assert_eq!(
            without.entries()[1].led_mask,
            3,
            "page 4 takes position 1's mask"
        );
        let page = without.to_page();
        assert_eq!(
            page.as_bytes()[..12],
            [
                0x02, 0x00, 0x01, 0x04, 0x00, 0x03, 0xff, 0xff, 0xff, 0xff, 0xff, 0x4c
            ]
        );
        assert_eq!(page.as_bytes()[12..24], dumped().as_bytes()[12..24]);
        assert_eq!(
            ProfileDirectory::parse(&page, 5).entries(),
            without.entries()
        );
        assert!(directory.without(3).is_none(), "there is no fourth entry");
    }

    #[test]
    fn removing_the_last_entry_leaves_an_empty_directory() {
        let page = page_with(&[0x02, 0x00, 0x01, 0xff], 0xff);
        let directory = ProfileDirectory::parse(&page, 5).without(0).unwrap();
        assert!(directory.entries().is_empty());
        assert!(
            ProfileDirectory::parse(&directory.to_page(), 5)
                .entries()
                .is_empty()
        );
    }

    #[test]
    fn an_appended_entry_takes_its_position_and_mask() {
        let directory = ProfileDirectory::parse(&dumped(), 5);

        let with = directory.with_appended(8).unwrap();

        assert_eq!(
            with.entries()[3],
            DirectoryEntry {
                page: 8,
                offset: 0,
                led_mask: 6
            }
        );
        let page = with.to_page();
        assert_eq!(page.as_bytes()[9..13], [0x08, 0x00, 0x06, 0xff]);
        assert_eq!(ProfileDirectory::parse(&page, 5).entries(), with.entries());
        assert!(directory.with_appended(3).is_none(), "page 3 is listed");
        let full = with.with_appended(9).unwrap();
        assert_eq!(full.entries().len(), 5);
        assert!(full.with_appended(10).is_none(), "five entries fill it");
        assert_eq!(
            ProfileDirectory::parse(&full.to_page(), 5).entries(),
            full.entries()
        );
    }
}
