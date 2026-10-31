use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
};

use hidpp::register::{
    current_profile::CurrentProfile,
    onboard_memory::{PAGE_SIZE, Page, PageImage, UnrestorablePage},
    report_rate::ReportInterval,
    resolution::Resolution,
};

use super::{
    G700Error, ProfileDpiChange, RestoreOutcome, RestoreStep, RestoredPage, plan_restore,
    restore_on, set_current_profile_on, set_profile_dpi_on, set_report_interval_on,
    set_resolution_on,
};
use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

fn valid_page(fill: u8) -> Page {
    let mut page = Page::from_bytes([fill; PAGE_SIZE]);
    page.update_crc();
    page
}

fn bad_crc_page(fill: u8) -> Page {
    let mut bytes = *valid_page(fill).as_bytes();
    bytes[0] ^= 0x01;
    Page::from_bytes(bytes)
}

/// A directory listing profiles on `pages`.
fn directory(pages: &[u8]) -> Page {
    let mut bytes = [0xff; PAGE_SIZE];
    for (i, &page) in pages.iter().enumerate() {
        bytes[i * 3..i * 3 + 3].copy_from_slice(&[page, 0x00, 0x01]);
    }
    let mut page = Page::from_bytes(bytes);
    page.update_crc();
    page
}

/// A G700 whose memory is `pages` and whose settings registers hold what
/// they were last set to, unless `ignore_writes`: then it acknowledges every
/// write and changes nothing.
#[derive(Clone)]
struct Mouse {
    pages: Vec<Page>,
    registers: Arc<Mutex<[[u8; 3]; 2]>>,
    resolution: Arc<Mutex<[u8; 4]>>,
    ignore_writes: bool,
    /// A page whose first byte reads differently every time, as a page read
    /// shifted by a late reply would.
    unstable: Option<(u8, Arc<AtomicU8>)>,
}

impl Mouse {
    fn new(pages: Vec<Page>) -> Self {
        Self {
            pages,
            // 0x0F index 2, 0x64 4 ms.
            registers: Arc::new(Mutex::new([[0x00, 0x02, 0x00], [0x04, 0x00, 0x00]])),
            resolution: Arc::new(Mutex::new([0x24, 0x00, 0x24, 0x00])),
            ignore_writes: false,
            unstable: None,
        }
    }

    fn respond(&self, request: &[u8]) -> Option<Vec<u8>> {
        let slot = |address| match address {
            0x0f => Some(0),
            0x64 => Some(1),
            _ => None,
        };
        match *request {
            [0x10, 1, 0x81, address, ..] => {
                let [a, b, c] = self.registers.lock().unwrap()[slot(address)?];
                Some(vec![0x10, 1, 0x81, address, a, b, c])
            }
            [0x10, 1, 0x80, address, a, b, c] => {
                let slot = slot(address)?;
                if !self.ignore_writes {
                    self.registers.lock().unwrap()[slot] = [a, b, c];
                }
                Some(vec![0x10, 1, 0x80, address, 0, 0, 0])
            }
            [0x10, 1, 0x83, 0x63, ..] => {
                let mut reply = vec![0x11, 1, 0x83, 0x63];
                reply.extend_from_slice(&*self.resolution.lock().unwrap());
                reply.resize(20, 0);
                Some(reply)
            }
            [0x11, 1, 0x82, 0x63, x_lo, x_hi, y_lo, y_hi, ..] => {
                if !self.ignore_writes {
                    *self.resolution.lock().unwrap() = [x_lo, x_hi, y_lo, y_hi];
                }
                Some(vec![0x10, 1, 0x82, 0x63, 0, 0, 0])
            }
            [0x10, 1, 0x83, 0xa2, page, word, 0] => {
                let Some(bytes) = self.pages.get(usize::from(page)) else {
                    return Some(vec![0x10, 1, 0x8f, 0x83, 0xa2, 0x0b, 0x00]);
                };
                let start = usize::from(word) * 2;
                let mut reply = vec![0x11, 1, 0x83, 0xa2];
                reply.extend_from_slice(&bytes.as_bytes()[start..start + 16]);
                if let Some((unstable, reads)) = &self.unstable
                    && *unstable == page
                    && word == 0
                {
                    reply[4] ^= reads.fetch_add(1, Ordering::SeqCst);
                }
                Some(reply)
            }
            _ => None,
        }
    }
}

async fn channel(
    mouse: Mouse,
) -> (
    Arc<hidpp::channel::HidppChannel>,
    crate::channel::scripted::ScriptedRawHidHandle,
) {
    let (raw, handle) =
        ScriptedRawHidChannel::with_dynamic_responder(move |request| mouse.respond(request));
    (scripted_channel(raw).await, handle)
}

fn writes(handle: &crate::channel::scripted::ScriptedRawHidHandle) -> Vec<Vec<u8>> {
    handle
        .written_reports()
        .into_iter()
        .filter(|report| !matches!(report[2], 0x81 | 0x83))
        .collect()
}

#[test]
fn a_restore_keeps_matching_pages_and_erases_only_after_the_directory() {
    let plan = plan_restore(vec![
        (1, directory(&[2]), directory(&[2, 3])),
        (9, Page::erased(), valid_page(0x09)),
        (3, valid_page(0x03), valid_page(0x30)),
        (25, bad_crc_page(0x25), bad_crc_page(0x25)),
    ])
    .unwrap();

    assert_eq!(
        plan,
        [
            RestoreStep::Put(3, PageImage::from_backup(valid_page(0x03)).unwrap()),
            RestoreStep::Keep(25),
            RestoreStep::Put(1, PageImage::from_backup(directory(&[2])).unwrap()),
            RestoreStep::Put(9, PageImage::from_backup(Page::erased()).unwrap()),
        ],
        "content before the directory, erasures after it; a page whose bad CRC \
         the mouse holds too is kept, not refused"
    );
}

#[test]
fn one_unrestorable_page_fails_the_whole_plan() {
    let damaged = bad_crc_page(0x25);

    let result = plan_restore(vec![
        (3, valid_page(0x03), valid_page(0x30)),
        (25, damaged.clone(), valid_page(0x25)),
    ]);

    assert!(
        matches!(
            result,
            Err(G700Error::Unrestorable {
                page: 25,
                source: UnrestorablePage { stored, computed }
            }) if stored == damaged.stored_crc() && computed == damaged.computed_crc()
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_restore_of_matching_pages_reads_and_writes_nothing() {
    let pages = vec![valid_page(0), directory(&[2]), valid_page(2)];
    let (chan, handle) = channel(Mouse::new(pages.clone())).await;

    let restored = restore_on(&chan, 1, vec![(1, pages[1].clone()), (2, pages[2].clone())])
        .await
        .unwrap();

    assert_eq!(
        restored,
        [
            RestoredPage {
                page: 2,
                outcome: RestoreOutcome::Identical
            },
            RestoredPage {
                page: 1,
                outcome: RestoreOutcome::Identical
            },
        ]
    );
    assert!(writes(&handle).is_empty(), "only reads went out");
}

#[tokio::test]
async fn a_restore_of_a_missing_page_fails_before_any_write() {
    let pages = vec![valid_page(0), directory(&[2]), valid_page(2)];
    let (chan, handle) = channel(Mouse::new(pages)).await;

    let result = restore_on(
        &chan,
        1,
        vec![(2, valid_page(0x22)), (27, valid_page(0x27))],
    )
    .await;

    assert!(
        matches!(result, Err(G700Error::PageRead { page: 27, .. })),
        "{result:?}"
    );
    assert!(writes(&handle).is_empty());
}

#[test]
fn a_dpi_change_is_checked_before_any_io() {
    let change = |mode, raw| ProfileDpiChange {
        profile: 0,
        mode,
        x: raw,
        y: raw,
    };
    assert!(matches!(
        change(5, 0x22).validate(),
        Err(G700Error::NoSuchDpiMode { mode: 5 })
    ));
    for raw in [0, 243] {
        assert!(
            matches!(change(0, raw).validate(), Err(G700Error::InvalidDpiStep { raw: r }) if r == raw),
            "{raw}"
        );
    }
    assert_eq!(change(4, 242).validate().unwrap(), change(4, 242));
}

#[test]
fn a_dpi_change_touches_only_its_mode() {
    let page = valid_page(0x11);
    let change = ProfileDpiChange {
        profile: 1,
        mode: 2,
        x: 0x23,
        y: 0x24,
    };

    let (changed, before, after) = change.apply(&page);

    assert_eq!((before.x, before.y), (0x11, 0x11));
    assert_eq!((after.x, after.y, after.leds), (0x23, 0x24, before.leds));
    let differing: Vec<usize> = (0..PAGE_SIZE)
        .filter(|&i| changed.as_bytes()[i] != page.as_bytes()[i])
        .collect();
    assert_eq!(
        differing,
        [8, 9],
        "mode 2's X and Y bytes; the CRC is the writer's"
    );
}

#[tokio::test]
async fn a_dpi_change_for_a_disabled_profile_writes_nothing() {
    let pages = vec![
        valid_page(0),
        directory(&[2, 3]),
        valid_page(2),
        valid_page(3),
    ];
    let (chan, handle) = channel(Mouse::new(pages)).await;

    let result = set_profile_dpi_on(
        &chan,
        1,
        ProfileDpiChange {
            profile: 2,
            mode: 0,
            x: 0x22,
            y: 0x22,
        },
    )
    .await;

    assert!(
        matches!(
            result,
            Err(G700Error::NoSuchProfile {
                index: 2,
                enabled: 2
            })
        ),
        "{result:?}"
    );
    assert!(writes(&handle).is_empty());
}

#[tokio::test]
async fn register_writes_are_read_back() {
    let (chan, handle) = channel(Mouse::new(vec![valid_page(0), directory(&[2, 3])])).await;

    let interval = ReportInterval::try_from(2).unwrap();
    assert_eq!(
        set_report_interval_on(&chan, 1, interval).await.unwrap(),
        interval
    );
    let resolution = Resolution { x: 0x44, y: 0x45 };
    assert_eq!(
        set_resolution_on(&chan, 1, resolution).await.unwrap(),
        resolution
    );
    assert_eq!(
        set_current_profile_on(&chan, 1, CurrentProfile::Index(1))
            .await
            .unwrap(),
        CurrentProfile::Index(1)
    );

    let mut resolution_write = vec![0x11, 1, 0x82, 0x63, 0x44, 0x00, 0x45, 0x00];
    resolution_write.resize(20, 0);
    assert_eq!(
        writes(&handle),
        [
            vec![0x10, 1, 0x80, 0x64, 0x02, 0x00, 0x00],
            resolution_write,
            vec![0x10, 1, 0x80, 0x0f, 0x00, 0x01, 0x00],
        ]
    );
}

#[tokio::test]
async fn a_write_the_mouse_ignores_is_not_applied() {
    let mut mouse = Mouse::new(vec![valid_page(0), directory(&[2, 3])]);
    mouse.ignore_writes = true;
    let (chan, _handle) = channel(mouse).await;

    let interval = set_report_interval_on(&chan, 1, ReportInterval::try_from(1).unwrap()).await;
    assert!(
        matches!(interval, Err(G700Error::ReportIntervalNotApplied { actual, .. }) if actual.millis() == 4),
        "{interval:?}"
    );
    let resolution = set_resolution_on(&chan, 1, Resolution { x: 1, y: 1 }).await;
    assert!(
        matches!(resolution, Err(G700Error::ResolutionNotApplied { actual, .. }) if actual.x == 0x24),
        "{resolution:?}"
    );
    let profile = set_current_profile_on(&chan, 1, CurrentProfile::Factory).await;
    assert!(
        matches!(
            profile,
            Err(G700Error::ProfileNotApplied {
                actual: CurrentProfile::Index(2),
                ..
            })
        ),
        "{profile:?}"
    );
}

#[tokio::test]
async fn a_profile_the_directory_does_not_list_is_not_selected() {
    let (chan, handle) = channel(Mouse::new(vec![valid_page(0), directory(&[2, 3])])).await;

    let result = set_current_profile_on(&chan, 1, CurrentProfile::Index(2)).await;

    assert!(
        matches!(
            result,
            Err(G700Error::NoSuchProfile {
                index: 2,
                enabled: 2
            })
        ),
        "{result:?}"
    );
    assert!(writes(&handle).is_empty());
}

#[tokio::test]
async fn a_dpi_change_needs_a_base_page_it_can_trust() {
    let pages = vec![
        valid_page(0),
        directory(&[2, 3]),
        bad_crc_page(0x11),
        valid_page(0x11),
    ];
    let mut mouse = Mouse::new(pages);
    mouse.unstable = Some((2, Arc::new(AtomicU8::new(2))));
    let (chan, handle) = channel(mouse).await;
    let change = |profile| ProfileDpiChange {
        profile,
        mode: 0,
        x: 0x22,
        y: 0x22,
    };

    let result = set_profile_dpi_on(&chan, 1, change(0)).await;

    assert!(
        matches!(result, Err(G700Error::UnstablePage { page: 2 })),
        "{result:?}"
    );
    assert!(writes(&handle).is_empty(), "nothing written");
    let page_reads = |page| {
        handle
            .written_reports()
            .iter()
            .filter(|report| report[2..6] == [0x83, 0xa2, page, 0x00])
            .count()
    };
    assert_eq!(page_reads(2), 2, "a page without a valid CRC is read twice");
}
