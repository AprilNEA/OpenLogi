//! A simulated G700 for the flash tests: pages that read, the
//! current-profile, resolution and report-interval registers, the HOT upload
//! into the RAM page and the `0xA0` erase and copy.
//!
//! Like the real mouse, selecting a profile loads its default DPI mode and
//! report interval into the live registers, so a page write — which plays
//! the factory profile and then the previous one again — resets them. A
//! copy can only clear bits, as on NOR flash.

use std::sync::{Arc, Mutex};

use hidpp::register::onboard_memory::{
    PAGE_SIZE, Page,
    directory::DIRECTORY_PAGE,
    g700::{Binding, ButtonMask, G700Profile, Special, parse_directory},
};

use crate::SharedChannel;
use crate::channel::route::DeviceRoute;
use crate::channel::scripted::{ScriptedRawHidChannel, ScriptedRawHidHandle, scripted_channel};

pub(super) const DEVICE: u8 = 1;

/// The simulated mouse.
pub(super) struct Flash {
    pub(super) pages: Vec<[u8; PAGE_SIZE]>,
    pub(super) current: [u8; 3],
    /// Register `0x63`, raw X and Y steps.
    pub(super) resolution: [u16; 2],
    /// Register `0x64`, milliseconds.
    pub(super) interval: u8,
    upload: Option<(usize, usize)>,
}

impl Flash {
    pub(super) fn new(pages: Vec<[u8; PAGE_SIZE]>, current: [u8; 3]) -> Self {
        Self {
            pages,
            current,
            resolution: [0x24, 0x24],
            interval: 4,
            upload: None,
        }
    }

    /// The page of the profile `current` selects, if it is a listed one.
    fn played_page(&self) -> Option<usize> {
        let directory = parse_directory(&Page::from_bytes(self.pages[usize::from(DIRECTORY_PAGE)]));
        match self.current {
            [0x00, index, _] => directory
                .entries()
                .get(usize::from(index))
                .map(|entry| usize::from(entry.page)),
            [0x01, page, _] => Some(usize::from(page)),
            _ => None,
        }
    }

    /// What the firmware does on a profile selection: the live registers
    /// take the profile's start values.
    fn load_played(&mut self) {
        let Some(page) = self.played_page() else {
            return;
        };
        let profile = G700Profile::parse(&Page::from_bytes(self.pages[page]));
        if let Some(mode) = profile.dpi_modes.get(usize::from(profile.default_dpi_mode)) {
            self.resolution = [mode.x.into(), mode.y.into()];
        }
        self.interval = profile.report_interval_millis;
    }

    pub(super) fn respond(&mut self, report: &[u8]) -> Option<Vec<u8>> {
        match *report {
            [0x10, DEVICE, 0x81, 0x0f, ..] => {
                let [a, b, c] = self.current;
                Some(vec![0x10, DEVICE, 0x81, 0x0f, a, b, c])
            }
            [0x10, DEVICE, 0x80, 0x0f, a, b, c] => {
                self.current = [a, b, c];
                self.load_played();
                Some(vec![0x10, DEVICE, 0x80, 0x0f, 0, 0, 0])
            }
            [0x10, DEVICE, 0x83, 0x63, ..] => {
                let [x, y] = self.resolution;
                let mut reply = vec![0x11, DEVICE, 0x83, 0x63];
                reply.extend_from_slice(&x.to_le_bytes());
                reply.extend_from_slice(&y.to_le_bytes());
                reply.resize(20, 0);
                Some(reply)
            }
            [0x11, DEVICE, 0x82, 0x63, x0, x1, y0, y1, ..] => {
                self.resolution = [u16::from_le_bytes([x0, x1]), u16::from_le_bytes([y0, y1])];
                Some(vec![0x10, DEVICE, 0x82, 0x63, 0, 0, 0])
            }
            [0x10, DEVICE, 0x81, 0x64, ..] => {
                Some(vec![0x10, DEVICE, 0x81, 0x64, self.interval, 0, 0])
            }
            [0x10, DEVICE, 0x80, 0x64, interval, ..] => {
                self.interval = interval;
                Some(vec![0x10, DEVICE, 0x80, 0x64, 0, 0, 0])
            }
            [0x10, DEVICE, 0x83, 0xa2, page, word, 0] => {
                let Some(bytes) = self.pages.get(usize::from(page)) else {
                    return Some(vec![0x10, DEVICE, 0x8f, 0x83, 0xa2, 0x0b, 0x00]);
                };
                let start = usize::from(word) * 2;
                let mut reply = vec![0x11, DEVICE, 0x83, 0xa2];
                reply.extend_from_slice(&bytes[start..start + 16]);
                Some(reply)
            }
            [0x10, DEVICE, 0x80, 0xa1, 0x01, 0x00, 0x00] => {
                self.upload = None;
                Some(vec![0x10, DEVICE, 0x80, 0xa1, 0, 0, 0])
            }
            [0x11, DEVICE, 0x92, sequence, ref rest @ ..] => {
                let [
                    0x01,
                    page,
                    word,
                    0,
                    0,
                    size_hi,
                    size_lo,
                    0,
                    0,
                    ref data @ ..,
                ] = *rest
                else {
                    return None;
                };
                let start = usize::from(page) * PAGE_SIZE + usize::from(word) * 2;
                self.upload = Some((start, usize::from(u16::from_be_bytes([size_hi, size_lo]))));
                self.store(data);
                Some(vec![0x10, DEVICE, 0x50, 0x01, sequence, 0x00, 0x00])
            }
            [0x11, DEVICE, 0x93, sequence, ref data @ ..] => {
                self.store(data);
                Some(vec![0x10, DEVICE, 0x50, 0x01, sequence, 0x00, 0x00])
            }
            [0x11, DEVICE, 0x82, 0xa0, ref data @ ..] => Some(self.memory(data)),
            _ => None,
        }
    }

    fn store(&mut self, data: &[u8]) {
        let Some((at, left)) = self.upload else {
            return;
        };
        let count = data.len().min(left);
        let (page, offset) = (at / PAGE_SIZE, at % PAGE_SIZE);
        self.pages[page][offset..offset + count].copy_from_slice(&data[..count]);
        self.upload = Some((at + count, left - count));
    }

    fn memory(&mut self, data: &[u8]) -> Vec<u8> {
        match *data {
            [0x02, 0, 0, 0, 0, 0, page, ..] => {
                self.pages[usize::from(page)] = [0xff; PAGE_SIZE];
            }
            [
                0x03,
                0,
                src,
                src_word,
                0,
                0,
                dst,
                dst_word,
                0,
                0,
                len_hi,
                len_lo,
                ..,
            ] => {
                let len = usize::from(u16::from_be_bytes([len_hi, len_lo]));
                let from = usize::from(src_word) * 2;
                let to = usize::from(dst_word) * 2;
                let source = self.pages[usize::from(src)][from..from + len].to_vec();
                for (byte, new) in self.pages[usize::from(dst)][to..to + len]
                    .iter_mut()
                    .zip(source)
                {
                    *byte &= new;
                }
            }
            _ => return vec![0x10, DEVICE, 0x8f, 0x82, 0xa0, 0x03, 0x00],
        }
        vec![0x10, DEVICE, 0x82, 0xa0, 0, 0, 0]
    }
}

pub(super) fn button(n: u8) -> Binding {
    Binding::MouseButtons(ButtonMask(1 << (n - 1)))
}

/// A profile page holding `bindings`, named `name`, with a valid CRC.
pub(super) fn named_profile_page(bindings: [Binding; 13], name: &str) -> [u8; PAGE_SIZE] {
    let mut bytes = [0xff; PAGE_SIZE];
    // 400, 800 and 1200 dpi; the last two modes off; default mode 0; 2 ms.
    bytes[..0x19].copy_from_slice(&[
        0x11, 0x11, 0x22, 0x11, 0x22, 0x22, 0x12, 0x11, 0x33, 0x33, 0x22, 0x11, 0x00, 0x00, 0x22,
        0x11, 0x00, 0x00, 0x22, 0x11, 0x00, 0x80, 0x01, 0x10, 0x02,
    ]);
    bytes[0x4a..0x4f].copy_from_slice(b"LGS02");
    bytes[0x4f..499].fill(0);
    let mut page = Page::from_bytes(bytes);
    let mut profile = G700Profile::parse(&page);
    profile.bindings = bindings;
    profile.set_name(name).expect("a test name fits");
    page = profile.to_page();
    page.update_crc();
    *page.as_bytes()
}

/// A profile page holding `bindings`, with a valid CRC.
pub(super) fn profile_page(bindings: [Binding; 13]) -> [u8; PAGE_SIZE] {
    named_profile_page(bindings, "")
}

/// The bindings of the user's first profile as dumped on 2026-10-31.
pub(super) fn first_bindings() -> [Binding; 13] {
    let special = Binding::Special;
    [
        button(1),
        button(2),
        button(3),
        button(4),
        button(5),
        button(6),
        button(7),
        special(Special::BatteryLevel),
        special(Special::ResolutionDown),
        special(Special::ResolutionUp),
        special(Special::ProfileCycleUp),
        special(Special::WheelLeft),
        special(Special::WheelRight),
    ]
}

/// The user's three profiles as dumped on 2026-10-31, the third played, on
/// a memory of 27 pages; page 5 holds content no directory entry names, as
/// on that mouse.
pub(super) fn dumped() -> Flash {
    let first = first_bindings();
    let mut third = first;
    third[3..7].copy_from_slice(&[button(7), button(6), button(4), button(5)]);
    third[8] = button(11);
    third[9] = button(10);
    let mut pages = vec![[0xff; PAGE_SIZE]; 27];
    let mut directory = [0xff; PAGE_SIZE];
    directory[..9].copy_from_slice(&[2, 0, 1, 3, 0, 3, 4, 0, 7]);
    let mut directory = Page::from_bytes(directory);
    directory.update_crc();
    pages[1] = *directory.as_bytes();
    pages[2] = named_profile_page(first, "Profile 1");
    pages[3] = named_profile_page(first, "Профиль 2");
    pages[4] = named_profile_page(third, "Profile 3");
    pages[5] = named_profile_page(first, "Профиль 4");
    Flash::new(pages, [0x00, 0x02, 0x00])
}

/// A G700 behind its receiver, simulated by `flash`.
pub(super) async fn g700(flash: Flash) -> (SharedChannel, ScriptedRawHidHandle, Arc<Mutex<Flash>>) {
    let flash = Arc::new(Mutex::new(flash));
    let responder = Arc::clone(&flash);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        responder.lock().unwrap().respond(request)
    });
    let shared = SharedChannel::new(
        scripted_channel(raw.presenting_as(0xc531)).await,
        DeviceRoute::Unifying {
            receiver_uid: format!("g700-sim-{}", std::process::id()),
            slot: 1,
        },
    );
    (shared, handle, flash)
}

/// Whether every report `handle` saw is a read: register and memory GETs.
pub(super) fn only_reads(handle: &ScriptedRawHidHandle) -> bool {
    handle
        .written_reports()
        .iter()
        .all(|report| report[0] == 0x10 && matches!(report[2], 0x81 | 0x83))
}
