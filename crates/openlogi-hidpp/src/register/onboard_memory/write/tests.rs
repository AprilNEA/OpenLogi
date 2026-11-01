use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use super::{
    Half, MemoryCommand, MemoryWriter, PageImage, PageWriteError, Refusal, StepError,
    UnrestorablePage, WriteStep,
};
use crate::{
    channel::mock::{MockRawHidChannel, MockRawHidHandle, channel_with_reader},
    protocol::v10::{ErrorType, Hidpp10Error},
    register::{
        current_profile::CurrentProfile,
        onboard_memory::{PAGE_SIZE, Page, directory::ProfileDirectory},
    },
};

const DEVICE: u8 = 1;

/// A HID++ 1.0 mouse with onboard memory, as far as the write path sees it:
/// pages that read, a current-profile register, the HOT upload into the RAM
/// page and the `0xA0` commands. Flash behaves like NOR flash, where a copy
/// can only clear bits, so a copy onto a page that was not erased first
/// corrupts it.
struct Sim {
    pages: Vec<[u8; PAGE_SIZE]>,
    current: [u8; 3],
    /// Where the next upload byte lands in the RAM page, and how many are
    /// left.
    upload: Option<(usize, usize)>,
    fault: Option<Fault>,
}

/// The one way a [`Sim`] misbehaves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// Acknowledge chunks with sequence number 0.
    WrongAck,
    /// Never acknowledge a continuation chunk.
    SilentChunks,
    /// Take uploads without storing them.
    DropUploadData,
    /// Flip a bit of every copy's destination.
    CorruptCopies,
    /// Refuse selecting a profile by index.
    RefuseIndex,
}

impl Sim {
    fn new(pages: usize) -> Self {
        Self {
            pages: vec![[0xff; PAGE_SIZE]; pages],
            current: [0x00, 0x02, 0x00],
            upload: None,
            fault: None,
        }
    }

    fn respond(&mut self, report: &[u8]) -> Vec<Vec<u8>> {
        let reply = match *report {
            [0x10, DEVICE, 0x81, 0x0f, ..] => {
                let [a, b, c] = self.current;
                Some(vec![0x10, DEVICE, 0x81, 0x0f, a, b, c])
            }
            [0x10, DEVICE, 0x80, 0x0f, a, b, c] => {
                if self.fault == Some(Fault::RefuseIndex) && a == 0x00 {
                    Some(error(0x80, 0x0f, ErrorType::InvalidValue))
                } else {
                    self.current = [a, b, c];
                    Some(vec![0x10, DEVICE, 0x80, 0x0f, 0, 0, 0])
                }
            }
            [0x10, DEVICE, 0x83, 0xa2, page, word, 0] => Some(self.read(page, word)),
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
                    return Vec::new();
                };
                let start = usize::from(page) * PAGE_SIZE + usize::from(word) * 2;
                self.upload = Some((start, usize::from(u16::from_be_bytes([size_hi, size_lo]))));
                self.store(data);
                Some(self.ack(sequence))
            }
            [0x11, DEVICE, 0x93, sequence, ref data @ ..] => {
                if self.fault == Some(Fault::SilentChunks) {
                    return Vec::new();
                }
                self.store(data);
                Some(self.ack(sequence))
            }
            [0x11, DEVICE, 0x82, 0xa0, ref data @ ..] => Some(self.memory(data)),
            _ => None,
        };
        reply.into_iter().collect()
    }

    fn read(&self, page: u8, word: u8) -> Vec<u8> {
        let Some(bytes) = self.pages.get(usize::from(page)) else {
            return error(0x83, 0xa2, ErrorType::InvalidParamValue);
        };
        let start = usize::from(word) * 2;
        let mut reply = vec![0x11, DEVICE, 0x83, 0xa2];
        reply.extend_from_slice(&bytes[start..start + 16]);
        reply
    }

    fn store(&mut self, data: &[u8]) {
        let Some((at, left)) = self.upload else {
            return;
        };
        let count = data.len().min(left);
        if self.fault != Some(Fault::DropUploadData) {
            let (page, offset) = (at / PAGE_SIZE, at % PAGE_SIZE);
            self.pages[page][offset..offset + count].copy_from_slice(&data[..count]);
        }
        self.upload = Some((at + count, left - count));
    }

    fn ack(&self, sequence: u8) -> Vec<u8> {
        let echoed = if self.fault == Some(Fault::WrongAck) {
            0
        } else {
            sequence
        };
        vec![0x10, DEVICE, 0x50, 0x01, echoed, 0x00, 0x00]
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
                let source: Vec<u8> = self.pages[usize::from(src)][from..from + len].to_vec();
                let target = &mut self.pages[usize::from(dst)][to..to + len];
                for (byte, new) in target.iter_mut().zip(source) {
                    *byte &= new;
                }
                if self.fault == Some(Fault::CorruptCopies) {
                    target[0] ^= 0x01;
                }
            }
            _ => return error(0x82, 0xa0, ErrorType::InvalidValue),
        }
        vec![0x10, DEVICE, 0x82, 0xa0, 0, 0, 0]
    }
}

fn error(sub_id: u8, address: u8, code: ErrorType) -> Vec<u8> {
    vec![0x10, DEVICE, 0x8f, sub_id, address, code.into(), 0x00]
}

/// A page whose bytes count up from `seed`, with a valid CRC.
fn pattern_page(seed: u8) -> Page {
    let mut bytes = [0u8; PAGE_SIZE];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = seed.wrapping_add(u8::try_from(i % 251).unwrap());
    }
    let mut page = Page::from_bytes(bytes);
    page.update_crc();
    page
}

/// A directory listing profiles on pages 2, 3 and 4.
fn directory_page(entries: &[u8]) -> Page {
    let mut bytes = [0xff; PAGE_SIZE];
    for (i, &page) in entries.iter().enumerate() {
        bytes[i * 3..i * 3 + 3].copy_from_slice(&[page, 0x00, 1]);
    }
    let mut page = Page::from_bytes(bytes);
    page.update_crc();
    page
}

/// The memory of the G700 the stage-1 dump read: 27 pages, a directory
/// with three profiles, a page of unknown purpose at 5, the rest erased.
fn g700_like() -> Sim {
    let mut sim = Sim::new(27);
    sim.pages[0] = *pattern_page(0x90).as_bytes();
    sim.pages[1] = *directory_page(&[2, 3, 4]).as_bytes();
    for page in 2..=5 {
        sim.pages[page] = *pattern_page(u8::try_from(page).unwrap()).as_bytes();
    }
    sim
}

async fn device(
    sim: Sim,
) -> (
    Arc<crate::channel::HidppChannel>,
    MockRawHidHandle,
    Arc<Mutex<Sim>>,
) {
    let (raw, handle) = MockRawHidChannel::new();
    let channel = Arc::new(channel_with_reader(raw).await);
    let sim = Arc::new(Mutex::new(sim));
    let responder = Arc::clone(&sim);
    handle.respond_with(move |report| responder.lock().unwrap().respond(report));
    (channel, handle, sim)
}

fn writer(channel: &crate::channel::HidppChannel) -> MemoryWriter<'_> {
    MemoryWriter::new(channel, DEVICE, 5).with_ack_timeout(Duration::from_millis(100))
}

fn read_frames(page: u8) -> Vec<Vec<u8>> {
    (0..32u8)
        .map(|chunk| vec![0x10, DEVICE, 0x83, 0xa2, page, chunk * 8, 0x00])
        .collect()
}

fn ram_frames() -> Vec<Vec<u8>> {
    read_frames(0).into_iter().take(16).collect()
}

fn select(profile: [u8; 3]) -> Vec<u8> {
    let [a, b, c] = profile;
    vec![0x10, DEVICE, 0x80, 0x0f, a, b, c]
}

fn long_set(address: u8, data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x11, DEVICE, 0x82, address];
    frame.extend_from_slice(data);
    frame.resize(20, 0);
    frame
}

/// The upload of `data`, as libratbag's `hidpp10_send_hot_payload` sends
/// it to RAM page 0, word 0: the reset, then the opening chunk with its
/// header and seven bytes, then sixteen bytes a chunk.
fn upload_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = vec![vec![0x10, DEVICE, 0x80, 0xa1, 0x01, 0x00, 0x00]];
    let mut opening = vec![
        0x11, DEVICE, 0x92, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    ];
    opening.extend_from_slice(&data[..7]);
    frames.push(opening);
    for (sequence, chunk) in (1u8..).zip(data[7..].chunks(16)) {
        let mut frame = vec![0x11, DEVICE, 0x93, sequence];
        frame.extend_from_slice(chunk);
        frame.resize(20, 0);
        frames.push(frame);
    }
    frames
}

/// Every frame a write of `content` over listed or erased page `page` sends,
/// with the device playing profile index 2.
fn write_sequence(page: u8, content: &Page) -> Vec<Vec<u8>> {
    let bytes = content.as_bytes();
    let mut frames = read_frames(page);
    if page != 1 {
        frames.extend(read_frames(1));
    }
    frames.push(vec![0x10, DEVICE, 0x81, 0x0f, 0, 0, 0]);
    frames.push(select([0xff, 0x00, 0x00]));
    frames.extend(upload_frames(&bytes[..256]));
    frames.extend(ram_frames());
    frames.push(long_set(0xa0, &[0x02, 0, 0, 0, 0, 0, page]));
    frames.push(long_set(
        0xa0,
        &[0x03, 0, 0, 0x00, 0, 0, page, 0x00, 0, 0, 0x01, 0x00],
    ));
    frames.extend(upload_frames(&bytes[256..]));
    frames.extend(ram_frames());
    frames.push(long_set(
        0xa0,
        &[0x03, 0, 0, 0x00, 0, 0, page, 0x80, 0, 0, 0x01, 0x00],
    ));
    frames.extend(read_frames(page));
    frames
}

fn sets(handle: &MockRawHidHandle) -> Vec<Vec<u8>> {
    handle
        .written_reports()
        .into_iter()
        .filter(|frame| frame[2] != 0x81 && frame[2] != 0x83)
        .collect()
}

#[test]
fn memory_commands_encode_page_word_offset_and_size() {
    assert_eq!(
        MemoryCommand::Erase { page: 9 }.to_register(),
        [0x02, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        MemoryCommand::CopyFromRam {
            page: 9,
            half: Half::Second
        }
        .to_register(),
        [0x03, 0, 0, 0, 0, 0, 9, 0x80, 0, 0, 0x01, 0x00, 0, 0, 0, 0],
        "from RAM word 0 to word 0x80, 256 bytes big-endian"
    );
}

#[test]
fn a_profile_page_is_staged_twice_and_the_previous_profile_comes_back() {
    futures::executor::block_on(async {
        let (channel, handle, sim) = device(g700_like()).await;
        // A stale CRC: the writer recomputes it.
        let mut content = pattern_page(0x40);
        let mut bytes = *content.as_bytes();
        bytes[PAGE_SIZE - 1] ^= 0xff;
        content = Page::from_bytes(bytes);
        let mut expected = content.clone();
        expected.update_crc();

        writer(&channel).write_page(3, &content).await.unwrap();

        let mut frames = write_sequence(3, &expected);
        frames.push(select([0x00, 0x02, 0x00]));
        assert_eq!(handle.written_reports(), frames);
        let sim = sim.lock().unwrap();
        assert_eq!(
            sim.pages[3],
            *expected.as_bytes(),
            "the page as written, CRC fixed"
        );
        assert_eq!(sim.current, [0x00, 0x02, 0x00], "profile 2 plays again");
        assert_eq!(
            sim.pages[4],
            *pattern_page(4).as_bytes(),
            "no other page touched"
        );
    });
}

#[test]
fn an_erased_page_may_be_written() {
    futures::executor::block_on(async {
        let (channel, _handle, sim) = device(g700_like()).await;

        writer(&channel)
            .write_page(16, &pattern_page(0x16))
            .await
            .unwrap();

        assert_eq!(
            sim.lock().unwrap().pages[16],
            *pattern_page(0x16).as_bytes()
        );
    });
}

#[test]
fn a_page_of_unknown_purpose_is_refused_after_reads_alone() {
    futures::executor::block_on(async {
        let (channel, handle, sim) = device(g700_like()).await;

        let result = writer(&channel).write_page(5, &pattern_page(0x55)).await;

        assert!(
            matches!(
                result,
                Err(PageWriteError::Refused {
                    page: 5,
                    reason: Refusal::UnknownContent
                })
            ),
            "{result:?}"
        );
        assert_eq!(sets(&handle), Vec::<Vec<u8>>::new(), "nothing but reads");
        assert_eq!(sim.lock().unwrap().pages[5], *pattern_page(5).as_bytes());
    });
}

#[test]
fn the_ram_page_is_refused_without_a_frame() {
    futures::executor::block_on(async {
        let (channel, handle, _sim) = device(g700_like()).await;

        for result in [
            writer(&channel).write_page(0, &pattern_page(0)).await,
            writer(&channel)
                .restore_page(0, &PageImage::from_backup(Page::erased()).unwrap())
                .await,
        ] {
            assert!(
                matches!(
                    result,
                    Err(PageWriteError::Refused {
                        page: 0,
                        reason: Refusal::RamPage
                    })
                ),
                "{result:?}"
            );
        }
        assert!(handle.written_reports().is_empty(), "not even a read");
    });
}

#[test]
fn a_page_past_the_last_page_is_refused_without_a_frame() {
    futures::executor::block_on(async {
        let (channel, handle, _sim) = device(g700_like()).await;
        let image = PageImage::from_backup(pattern_page(32)).unwrap();

        let result = writer(&channel).restore_page(32, &image).await;

        assert!(
            matches!(
                result,
                Err(PageWriteError::Refused {
                    page: 32,
                    reason: Refusal::PastLastPage
                })
            ),
            "{result:?}"
        );
        assert!(handle.written_reports().is_empty(), "not even a read");
    });
}

#[test]
fn a_page_past_the_end_of_memory_is_unreadable_and_not_written() {
    futures::executor::block_on(async {
        let (channel, handle, _sim) = device(g700_like()).await;
        let image = PageImage::from_backup(pattern_page(27)).unwrap();

        let result = writer(&channel).restore_page(27, &image).await;

        assert!(
            matches!(
                result,
                Err(PageWriteError::Unreadable {
                    page: 27,
                    source: Hidpp10Error::RegisterAccess(ErrorType::InvalidParamValue)
                })
            ),
            "{result:?}"
        );
        assert_eq!(handle.written_reports(), read_frames(27)[..1]);
    });
}

#[test]
fn a_channel_without_long_reports_is_refused() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::short_only();
        let channel = channel_with_reader(raw).await;

        let result = writer(&channel).write_page(2, &pattern_page(2)).await;

        assert!(
            matches!(
                result,
                Err(PageWriteError::Refused {
                    page: 2,
                    reason: Refusal::NoLongReports
                })
            ),
            "{result:?}"
        );
        assert!(handle.written_reports().is_empty());
    });
}

#[test]
fn a_restore_may_overwrite_a_page_of_unknown_purpose() {
    futures::executor::block_on(async {
        let (channel, _handle, sim) = device(g700_like()).await;
        let image = PageImage::from_backup(pattern_page(0x77)).unwrap();

        writer(&channel).restore_page(5, &image).await.unwrap();

        assert_eq!(sim.lock().unwrap().pages[5], *pattern_page(0x77).as_bytes());
    });
}

#[test]
fn restoring_erased_flash_is_an_erase_alone() {
    futures::executor::block_on(async {
        let (channel, handle, sim) = device(g700_like()).await;
        sim.lock().unwrap().pages[16] = *pattern_page(0x16).as_bytes();
        let image = PageImage::from_backup(Page::erased()).unwrap();
        assert!(image.is_erased());

        writer(&channel).restore_page(16, &image).await.unwrap();

        assert_eq!(
            sets(&handle),
            [
                select([0xff, 0x00, 0x00]),
                long_set(0xa0, &[0x02, 0, 0, 0, 0, 0, 16]),
                select([0x00, 0x02, 0x00]),
            ]
        );
        assert_eq!(
            sim.lock().unwrap().pages[16],
            [0xff; PAGE_SIZE],
            "still no CRC"
        );
    });
}

#[test]
fn a_backup_page_with_a_bad_crc_cannot_be_restored() {
    let mut bytes = *pattern_page(1).as_bytes();
    bytes[0] ^= 0x01;
    let page = Page::from_bytes(bytes);

    let result = PageImage::from_backup(page.clone());

    assert_eq!(
        result,
        Err(UnrestorablePage {
            stored: page.stored_crc(),
            computed: page.computed_crc()
        })
    );
}

#[test]
fn a_directory_write_plays_the_profile_the_caller_names() {
    futures::executor::block_on(async {
        let (channel, handle, sim) = device(g700_like()).await;
        let directory = ProfileDirectory::parse(&directory_page(&[2, 3]), 5);

        writer(&channel)
            .write_directory(&directory, CurrentProfile::Index(1))
            .await
            .unwrap();

        let mut frames = write_sequence(1, &directory.to_page());
        frames.push(select([0x00, 0x01, 0x00]));
        assert_eq!(handle.written_reports(), frames);
        let sim = sim.lock().unwrap();
        assert_eq!(sim.pages[1], *directory_page(&[2, 3]).as_bytes());
        assert_eq!(sim.current, [0x00, 0x01, 0x00]);
    });
}

#[test]
fn a_directory_write_may_keep_the_played_index() {
    futures::executor::block_on(async {
        let (channel, _handle, sim) = device(g700_like()).await;
        let directory = ProfileDirectory::parse(&directory_page(&[2, 3, 4, 6]), 5);

        writer(&channel)
            .write_directory(&directory, CurrentProfile::Index(2))
            .await
            .unwrap();

        assert_eq!(sim.lock().unwrap().current, [0x00, 0x02, 0x00]);
    });
}

#[test]
fn a_directory_without_profiles_or_play_target_is_refused_without_a_frame() {
    futures::executor::block_on(async {
        let (channel, handle, sim) = device(g700_like()).await;
        let empty = ProfileDirectory::parse(&directory_page(&[]), 5);
        let two = ProfileDirectory::parse(&directory_page(&[2, 3]), 5);

        let cases = [
            (&empty, CurrentProfile::Index(0), Refusal::NoProfiles),
            (&two, CurrentProfile::Index(2), Refusal::PlayNotListed),
            (&two, CurrentProfile::Factory, Refusal::PlayNotListed),
            (
                &two,
                CurrentProfile::Address { page: 4, offset: 0 },
                Refusal::PlayNotListed,
            ),
        ];
        for (directory, play, expected) in cases {
            let error = writer(&channel)
                .write_directory(directory, play)
                .await
                .unwrap_err();
            assert!(
                matches!(error, PageWriteError::Refused { page: 1, reason } if reason == expected),
                "{play:?}: {error:?}"
            );
        }
        assert!(handle.written_reports().is_empty());
        assert_eq!(
            sim.lock().unwrap().pages[1],
            *directory_page(&[2, 3, 4]).as_bytes()
        );
    });
}

#[test]
fn an_upload_acknowledged_out_of_order_stops_before_the_erase() {
    futures::executor::block_on(async {
        let mut memory = g700_like();
        memory.fault = Some(Fault::WrongAck);
        let (channel, handle, sim) = device(memory).await;

        let result = writer(&channel).write_page(3, &pattern_page(0x33)).await;

        assert!(
            matches!(
                result,
                Err(PageWriteError::Abandoned {
                    page: 3,
                    step: WriteStep::Upload(Half::First),
                    profile: CurrentProfile::Index(2),
                    source: StepError::UnexpectedAck {
                        chunk: 1,
                        acknowledged: 0
                    }
                })
            ),
            "{result:?}"
        );
        assert!(
            !sets(&handle)
                .iter()
                .any(|frame| frame[2..4] == [0x82, 0xa0]),
            "no erase or copy"
        );
        let sim = sim.lock().unwrap();
        assert_eq!(sim.pages[3], *pattern_page(3).as_bytes());
        assert_eq!(
            sim.current,
            [0x00, 0x02, 0x00],
            "the previous profile again"
        );
    });
}

#[test]
fn an_unacknowledged_chunk_times_out_before_the_erase() {
    futures::executor::block_on(async {
        let mut memory = g700_like();
        memory.fault = Some(Fault::SilentChunks);
        let (channel, handle, sim) = device(memory).await;

        let result = writer(&channel).write_page(3, &pattern_page(0x33)).await;

        assert!(
            matches!(
                result,
                Err(PageWriteError::Abandoned {
                    step: WriteStep::Upload(Half::First),
                    source: StepError::Chunk {
                        chunk: 1,
                        source: Hidpp10Error::Channel(_)
                    },
                    ..
                })
            ),
            "{result:?}"
        );
        assert!(
            !sets(&handle)
                .iter()
                .any(|frame| frame[2..4] == [0x82, 0xa0])
        );
        let sim = sim.lock().unwrap();
        assert_eq!(sim.pages[3], *pattern_page(3).as_bytes());
        assert_eq!(
            sim.current,
            [0x00, 0x02, 0x00],
            "the previous profile again"
        );
    });
}

#[test]
fn an_upload_the_ram_page_does_not_hold_stops_before_the_erase() {
    futures::executor::block_on(async {
        let mut memory = g700_like();
        memory.fault = Some(Fault::DropUploadData);
        let (channel, handle, sim) = device(memory).await;

        let result = writer(&channel).write_page(3, &pattern_page(0x33)).await;

        assert!(
            matches!(
                result,
                Err(PageWriteError::Abandoned {
                    step: WriteStep::VerifyUpload(Half::First),
                    profile: CurrentProfile::Index(2),
                    source: StepError::UploadDiffers { offset: 0 },
                    ..
                })
            ),
            "{result:?}"
        );
        assert!(
            !sets(&handle)
                .iter()
                .any(|frame| frame[2..4] == [0x82, 0xa0])
        );
        let sim = sim.lock().unwrap();
        assert_eq!(sim.pages[3], *pattern_page(3).as_bytes());
        assert_eq!(
            sim.current,
            [0x00, 0x02, 0x00],
            "the previous profile again"
        );
    });
}

#[test]
fn an_upload_that_fails_while_the_factory_profile_played_keeps_it() {
    futures::executor::block_on(async {
        let mut memory = g700_like();
        memory.current = [0xff, 0x00, 0x00];
        memory.fault = Some(Fault::DropUploadData);
        let (channel, handle, _sim) = device(memory).await;

        let result = writer(&channel).write_page(3, &pattern_page(0x33)).await;

        assert!(
            matches!(
                result,
                Err(PageWriteError::Abandoned {
                    profile: CurrentProfile::Factory,
                    ..
                })
            ),
            "{result:?}"
        );
        let selections: Vec<_> = sets(&handle)
            .into_iter()
            .filter(|frame| frame[2..4] == [0x80, 0x0f])
            .collect();
        assert_eq!(
            selections,
            [select([0xff, 0x00, 0x00])],
            "the factory profile is not selected twice"
        );
    });
}

#[test]
fn a_page_that_reads_back_wrong_leaves_the_factory_profile_on() {
    futures::executor::block_on(async {
        let mut memory = g700_like();
        memory.fault = Some(Fault::CorruptCopies);
        let (channel, handle, sim) = device(memory).await;

        let result = writer(&channel).write_page(3, &pattern_page(0x33)).await;

        assert!(
            matches!(result, Err(PageWriteError::Mismatch { page: 3, offset: 0 })),
            "{result:?}"
        );
        assert_eq!(
            sets(&handle).last(),
            Some(&long_set(
                0xa0,
                &[0x03, 0, 0, 0x00, 0, 0, 3, 0x80, 0, 0, 0x01, 0x00]
            )),
            "no profile selected after the read-back"
        );
        assert_eq!(sim.lock().unwrap().current, [0xff, 0x00, 0x00]);
    });
}

#[test]
fn a_refused_reselection_is_reported_after_a_verified_write() {
    futures::executor::block_on(async {
        let mut memory = g700_like();
        memory.fault = Some(Fault::RefuseIndex);
        let (channel, _handle, sim) = device(memory).await;

        let result = writer(&channel).write_page(3, &pattern_page(0x33)).await;

        assert!(
            matches!(
                result,
                Err(PageWriteError::Reselect {
                    page: 3,
                    profile: CurrentProfile::Index(2),
                    source: Hidpp10Error::RegisterAccess(ErrorType::InvalidValue)
                })
            ),
            "{result:?}"
        );
        assert_eq!(sim.lock().unwrap().pages[3], *pattern_page(0x33).as_bytes());
    });
}

#[test]
fn the_factory_profile_stays_selected_when_it_was_playing() {
    futures::executor::block_on(async {
        let mut memory = g700_like();
        memory.current = [0xff, 0x00, 0x00];
        let (channel, handle, _sim) = device(memory).await;

        writer(&channel)
            .write_page(3, &pattern_page(0x33))
            .await
            .unwrap();

        let selections: Vec<_> = sets(&handle)
            .into_iter()
            .filter(|frame| frame[2..4] == [0x80, 0x0f])
            .collect();
        assert_eq!(selections, [select([0xff, 0x00, 0x00])]);
    });
}

/// [`device`], over a transport that breaks — the receiver unplugged — on
/// the write of frame `at`, counted from 0, and on every one after it.
async fn device_vanishing_at(
    sim: Sim,
    at: usize,
) -> (Arc<crate::channel::HidppChannel>, Arc<Mutex<Sim>>) {
    let (raw, handle) = MockRawHidChannel::new();
    let channel = Arc::new(channel_with_reader(raw).await);
    let sim = Arc::new(Mutex::new(sim));
    let responder = Arc::clone(&sim);
    let breaker = handle.clone();
    let mut answered = 0;
    if at == 0 {
        handle.fail_writes();
    }
    handle.respond_with(move |report| {
        let replies = responder.lock().unwrap().respond(report);
        answered += 1;
        if answered == at {
            breaker.fail_writes();
        }
        replies
    });
    (channel, sim)
}

#[test]
fn a_write_cut_off_at_any_frame_names_the_step_and_what_the_page_holds() {
    use super::PageState;

    let content = pattern_page(0x33);
    let original = pattern_page(3);
    let mut half_written = [0xff; PAGE_SIZE];
    half_written[..256].copy_from_slice(&content.as_bytes()[..256]);
    assert_eq!(
        write_sequence(3, &content).len(),
        169,
        "the frame numbers below follow write_sequence"
    );

    let before = *original.as_bytes();
    let cases = [
        (65, WriteStep::SelectFactory, before),
        (66, WriteStep::Upload(Half::First), before),
        (84, WriteStep::VerifyUpload(Half::First), before),
        (100, WriteStep::Erase, before),
        (101, WriteStep::Copy(Half::First), [0xff; PAGE_SIZE]),
        (102, WriteStep::Upload(Half::Second), half_written),
        (120, WriteStep::VerifyUpload(Half::Second), half_written),
        (136, WriteStep::Copy(Half::Second), half_written),
        (137, WriteStep::ReadBack, *content.as_bytes()),
    ];
    futures::executor::block_on(async {
        for (at, expected_step, expected_page) in cases {
            let (channel, sim) = device_vanishing_at(g700_like(), at).await;

            let result = writer(&channel).write_page(3, &content).await;

            let Err(PageWriteError::Failed { page: 3, step, .. }) = result else {
                panic!("frame {at}: {result:?}");
            };
            assert_eq!(step, expected_step, "frame {at}");
            let sim = sim.lock().unwrap();
            assert_eq!(sim.pages[3], expected_page, "frame {at}: the page");
            let factory = [0xff, 0x00, 0x00];
            if step == WriteStep::SelectFactory {
                assert_eq!(sim.current, [0x00, 0x02, 0x00], "frame {at}");
            } else {
                assert_eq!(sim.current, factory, "frame {at}: left on factory");
            }
            let held_before = sim.pages[3] == *original.as_bytes();
            match step.page_state() {
                PageState::Untouched => assert!(held_before, "frame {at}"),
                PageState::MaybeErased => assert!(
                    held_before || sim.pages[3] == [0xff; PAGE_SIZE],
                    "frame {at}"
                ),
                PageState::PartWritten => assert!(!held_before, "frame {at}"),
                PageState::Unverified => {}
            }
        }
    });
}

#[test]
fn a_write_cut_off_before_the_factory_switch_changes_nothing() {
    futures::executor::block_on(async {
        for (at, page) in [(0, Some(3)), (32, Some(1)), (64, None)] {
            let (channel, sim) = device_vanishing_at(g700_like(), at).await;

            let result = writer(&channel).write_page(3, &pattern_page(0x33)).await;

            match (page, &result) {
                (Some(page), Err(PageWriteError::Unreadable { page: read, .. })) => {
                    assert_eq!(*read, page, "frame {at}");
                }
                (None, Err(PageWriteError::CurrentProfileUnreadable(_))) => {}
                _ => panic!("frame {at}: {result:?}"),
            }
            let sim = sim.lock().unwrap();
            assert_eq!(sim.pages[3], *pattern_page(3).as_bytes(), "frame {at}");
            assert_eq!(sim.current, [0x00, 0x02, 0x00], "frame {at}");
        }
    });
}

#[test]
fn a_write_cut_off_at_the_reselection_is_a_reselect_error_over_a_verified_page() {
    futures::executor::block_on(async {
        let (channel, sim) = device_vanishing_at(g700_like(), 169).await;

        let result = writer(&channel).write_page(3, &pattern_page(0x33)).await;

        assert!(
            matches!(
                result,
                Err(PageWriteError::Reselect {
                    page: 3,
                    profile: CurrentProfile::Index(2),
                    source: Hidpp10Error::Channel(_)
                })
            ),
            "{result:?}"
        );
        assert_eq!(sim.lock().unwrap().pages[3], *pattern_page(0x33).as_bytes());
    });
}

#[test]
fn a_failed_write_says_what_the_page_and_the_profile_are_left_as() {
    let failed = |step| {
        PageWriteError::Failed {
            page: 3,
            step,
            source: StepError::Device(Hidpp10Error::UnsupportedResponse),
        }
        .to_string()
    };

    assert_eq!(
        failed(WriteStep::SelectFactory),
        "writing page 3 failed while selecting the factory profile; the page is untouched; \
         the device is left on its factory profile or the previous one"
    );
    assert_eq!(
        failed(WriteStep::Copy(Half::First)),
        "writing page 3 failed while copying the first half; the page is left part-written; \
         the device is left on its factory profile"
    );
    assert_eq!(
        PageWriteError::Abandoned {
            page: 3,
            step: WriteStep::Upload(Half::First),
            profile: CurrentProfile::Index(2),
            source: StepError::Device(Hidpp10Error::UnsupportedResponse),
        }
        .to_string(),
        "writing page 3 failed while uploading the first half, before anything was erased; \
         the page is untouched and Index(2) is selected again"
    );
}
