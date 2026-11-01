use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use hidpp::register::onboard_memory::{
    PAGE_SIZE, Page,
    g700::{Binding, G700Profile, KeyModifiers, parse_directory, raw_from_dpi},
};
use openlogi_core::binding::{ButtonId, FlashBinding, G700, MouseButtonNumber, SlotIntent};
use openlogi_core::hid::{Dpi, FlashProfileEdit, ProfileIndex, ReportRate};

use super::super::sim::{
    Flash, button, dumped, first_bindings, g700, named_profile_page, only_reads, profile_page,
};
use super::{
    DpiSave, G700Job, G700Sync, activate_g700_profile_on, check_g700_edit, read_g700_profiles_on,
    sync_g700_on,
};
use crate::SharedChannel;
use crate::channel::route::DeviceRoute;
use crate::channel::scripted::{ScriptedRawHidChannel, ScriptedRawHidHandle, scripted_channel};
use crate::write::g700::G700Error;

async fn sync_bindings(
    shared: &SharedChannel,
    bindings: Vec<SlotIntent>,
) -> Result<G700Sync, G700Error> {
    sync_g700_on(
        shared,
        &mut G700Job {
            bindings,
            ..G700Job::default()
        },
    )
    .await
}

fn profile(n: u8) -> ProfileIndex {
    ProfileIndex::new(n).unwrap()
}

async fn edit(shared: &SharedChannel, edits: Vec<FlashProfileEdit>) -> Result<G700Sync, G700Error> {
    sync_g700_on(
        shared,
        &mut G700Job {
            edits: edits.into(),
            ..G700Job::default()
        },
    )
    .await
}

fn directory_pages(flash: &Mutex<Flash>) -> Vec<u8> {
    parse_directory(&Page::from_bytes(flash.lock().unwrap().pages[1]))
        .entries()
        .iter()
        .map(|entry| entry.page)
        .collect()
}

fn page(flash: &Mutex<Flash>, number: usize) -> G700Profile {
    G700Profile::parse(&Page::from_bytes(flash.lock().unwrap().pages[number]))
}

/// The pages the job erased, in order: every page write starts with one.
fn erased_pages(handle: &ScriptedRawHidHandle) -> Vec<u8> {
    handle
        .written_reports()
        .iter()
        .filter(|report| report[..5] == [0x11, 0x01, 0x82, 0xa0, 0x02])
        .map(|report| report[10])
        .collect()
}

fn dpi(value: u16) -> Dpi {
    Dpi::new(value)
}

fn intents(changes: &[(ButtonId, SlotIntent)]) -> Vec<SlotIntent> {
    let mut intents = vec![SlotIntent::Keep; 13];
    for (button, intent) in changes {
        intents[usize::from(G700.slot_of(*button).unwrap())] = intent.clone();
    }
    intents
}

fn number(n: u8) -> MouseButtonNumber {
    MouseButtonNumber::try_new(n).unwrap()
}

fn played_bindings(flash: &Mutex<Flash>) -> [Binding; 13] {
    G700Profile::parse(&Page::from_bytes(flash.lock().unwrap().pages[4])).bindings
}

#[tokio::test]
async fn a_sync_that_changes_nothing_only_reads() {
    let (shared, handle, flash) = g700(dumped()).await;
    let before = flash.lock().unwrap().pages.clone();

    let sync = sync_bindings(&shared, intents(&[])).await.unwrap();

    assert_eq!(sync.profile, Some(2));
    assert!(!sync.written);
    assert_eq!(sync.plan.bindings[8], FlashBinding::MouseButtons(1 << 10));
    assert!(sync.plan.routed.is_empty());
    assert_eq!(flash.lock().unwrap().pages, before);
    assert!(
        handle
            .written_reports()
            .iter()
            .all(|report| report[0] == 0x10 && matches!(report[2], 0x81 | 0x83)),
        "only register and memory reads go out"
    );
}

#[tokio::test]
async fn a_changed_slot_is_written_to_the_played_profile_and_routed() {
    let (shared, _handle, flash) = g700(dumped()).await;
    let untouched = flash.lock().unwrap().pages[2];
    let chord = FlashBinding::Key {
        modifiers: 0x05,
        usage: 0x06,
    };

    let sync = sync_bindings(
        &shared,
        intents(&[
            (ButtonId::G6, SlotIntent::Agent),
            (ButtonId::G7, SlotIntent::Native(chord)),
        ]),
    )
    .await
    .unwrap();

    assert!(sync.written);
    assert_eq!(
        sync.plan.routed,
        BTreeMap::from([(number(16), ButtonId::G6)])
    );
    let bindings = played_bindings(&flash);
    assert_eq!(bindings[5], button(16), "G6 presses the agent button");
    assert_eq!(
        bindings[6],
        Binding::Key {
            modifiers: KeyModifiers::LEFT_CTRL | KeyModifiers::LEFT_ALT,
            usage: 0x06
        },
        "modifier flags are written"
    );
    assert_eq!(bindings[8], button(11), "the rest is as it was");
    let page = Page::from_bytes(flash.lock().unwrap().pages[4]);
    assert!(page.has_valid_crc());
    assert_eq!(flash.lock().unwrap().pages[2], untouched);
    assert_eq!(
        flash.lock().unwrap().current,
        [0x00, 0x02, 0x00],
        "the profile plays again"
    );

    // Writing the same intents again finds nothing to change.
    let again = sync_bindings(
        &shared,
        intents(&[
            (ButtonId::G6, SlotIntent::Agent),
            (ButtonId::G7, SlotIntent::Native(chord)),
        ]),
    )
    .await
    .unwrap();
    assert!(!again.written);
    assert_eq!(again.plan.routed, sync.plan.routed);
}

#[tokio::test]
async fn an_agent_button_another_profile_presses_is_left_to_it() {
    let mut flash = dumped();
    let mut first = G700Profile::parse(&Page::from_bytes(flash.pages[2])).bindings;
    first[5] = button(16);
    first[6] = button(15);
    flash.pages[2] = profile_page(first);
    let (shared, _handle, flash) = g700(flash).await;

    let sync = sync_bindings(&shared, intents(&[(ButtonId::G8, SlotIntent::Agent)]))
        .await
        .unwrap();

    assert_eq!(
        sync.plan.routed,
        BTreeMap::from([(number(14), ButtonId::G8)])
    );
    assert_eq!(played_bindings(&flash)[7], button(14));
}

#[tokio::test]
async fn the_factory_profile_holds_no_bindings_to_sync() {
    let mut flash = dumped();
    flash.current = [0xff, 0x00, 0x00];
    let (shared, handle, _flash) = g700(flash).await;

    let sync = sync_bindings(&shared, intents(&[(ButtonId::G6, SlotIntent::Agent)]))
        .await
        .unwrap();

    assert_eq!(sync.profile, None);
    assert!(!sync.written && sync.plan.bindings.is_empty());
    assert!(
        handle
            .written_reports()
            .iter()
            .all(|report| matches!(report[2], 0x81 | 0x83))
    );
}

#[tokio::test]
async fn another_device_is_not_synced() {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(|_| None);
    let shared = SharedChannel::new(
        scripted_channel(raw.presenting_as(0xc548)).await,
        DeviceRoute::Bolt {
            receiver_uid: "bolt".into(),
            slot: 1,
        },
    );

    let result = sync_bindings(&shared, intents(&[])).await;

    assert!(
        matches!(
            result,
            Err(G700Error::Device(crate::write::WriteError::DeviceNotFound))
        ),
        "{result:?}"
    );
    assert!(handle.written_reports().is_empty());
}

#[tokio::test]
async fn a_listing_reads_the_slots_names_and_played_settings_without_writing() {
    let (shared, handle, _flash) = g700(dumped()).await;

    let listing = read_g700_profiles_on(&shared).await.unwrap();

    let names: Vec<_> = listing
        .onboard
        .profiles
        .iter()
        .map(|p| (p.index.get().get(), p.enabled, p.name.clone()))
        .collect();
    assert_eq!(
        names,
        [
            (1, true, Some("Profile 1".to_string())),
            (2, true, Some("Профиль 2".to_string())),
            (3, true, Some("Profile 3".to_string())),
            (4, false, None),
            (5, false, None),
        ]
    );
    assert_eq!(listing.onboard.active_profile, Some(profile(3)));
    let active = listing.active.unwrap();
    assert_eq!(
        active.dpi_modes,
        [Some(dpi(400)), Some(dpi(800)), Some(dpi(1200)), None, None]
    );
    assert_eq!(active.default_mode, Some(0));
    assert_eq!(active.report_rate, Some(ReportRate::new(500)));
    assert!(
        listing.disabled.is_empty(),
        "page 5 is outside the profile pages"
    );
    assert!(listing.can_create);
    assert!(only_reads(&handle));
}

#[tokio::test]
async fn a_profile_disabled_earlier_is_offered_again_and_a_macro_page_is_not() {
    let mut flash = dumped();
    flash.pages[8] = named_profile_page(first_bindings(), "Old");
    // Valid CRC, no metadata block: macros, as page 7 of the dumped mouse.
    let mut macros = Page::from_bytes([0x22; PAGE_SIZE]);
    macros.update_crc();
    flash.pages[9] = *macros.as_bytes();
    let (shared, _handle, _flash) = g700(flash).await;

    let listing = read_g700_profiles_on(&shared).await.unwrap();

    let disabled: Vec<_> = listing
        .disabled
        .iter()
        .map(|p| (p.page, p.name.clone()))
        .collect();
    assert_eq!(disabled, [(8, Some("Old".to_string()))]);
}

#[tokio::test]
async fn disabling_the_played_profile_plays_the_one_before_it() {
    let (shared, handle, flash) = g700(dumped()).await;
    let pages_before = flash.lock().unwrap().pages.clone();

    edit(&shared, vec![FlashProfileEdit::Disable(profile(3))])
        .await
        .unwrap();

    assert_eq!(directory_pages(&flash), [2, 3]);
    assert_eq!(flash.lock().unwrap().current, [0x00, 0x01, 0x00]);
    assert_eq!(erased_pages(&handle), [1], "only the directory is written");
    let flash = flash.lock().unwrap();
    assert_eq!(
        flash.pages[4], pages_before[4],
        "the page keeps its profile"
    );
}

#[tokio::test]
async fn disabling_an_earlier_profile_keeps_the_played_one_playing() {
    let (shared, _handle, flash) = g700(dumped()).await;

    edit(&shared, vec![FlashProfileEdit::Disable(profile(1))])
        .await
        .unwrap();

    assert_eq!(directory_pages(&flash), [3, 4]);
    assert_eq!(
        flash.lock().unwrap().current,
        [0x00, 0x01, 0x00],
        "page 4 moved up to index 1 and still plays"
    );
}

#[tokio::test]
async fn the_last_enabled_profile_is_never_disabled() {
    let mut flash = dumped();
    let mut directory = Page::from_bytes([0xff; PAGE_SIZE]);
    let mut bytes = *directory.as_bytes();
    bytes[..3].copy_from_slice(&[4, 0, 1]);
    directory = Page::from_bytes(bytes);
    directory.update_crc();
    flash.pages[1] = *directory.as_bytes();
    flash.current = [0x00, 0x00, 0x00];
    let (shared, handle, flash) = g700(flash).await;

    let mut job = G700Job {
        edits: VecDeque::from([FlashProfileEdit::Disable(profile(1))]),
        ..G700Job::default()
    };
    let result = sync_g700_on(&shared, &mut job).await;

    assert!(matches!(result, Err(G700Error::LastProfile)), "{result:?}");
    assert_eq!(job.edits.len(), 1, "the failed edit stays in the job");
    assert!(only_reads(&handle));
    assert_eq!(directory_pages(&flash), [4]);
}

#[tokio::test]
async fn a_copy_lands_on_the_first_erased_profile_page_before_it_is_listed() {
    let (shared, handle, flash) = g700(dumped()).await;

    edit(&shared, vec![FlashProfileEdit::Duplicate(profile(2))])
        .await
        .unwrap();

    assert_eq!(
        directory_pages(&flash),
        [2, 3, 4, 8],
        "pages 6 and 7 are erased here but are no profile pages"
    );
    assert_eq!(
        erased_pages(&handle),
        [8, 1],
        "the page first, the directory last"
    );
    let flash = flash.lock().unwrap();
    assert_eq!(flash.pages[8], flash.pages[3]);
    assert_eq!(
        flash.current,
        [0x00, 0x02, 0x00],
        "the played profile plays on"
    );
    let directory = parse_directory(&Page::from_bytes(flash.pages[1]));
    assert_eq!(directory.entries()[3].led_mask, 6);
}

#[tokio::test]
async fn a_copy_needs_a_free_slot_and_a_free_page() {
    let mut flash = dumped();
    for number in 8..=24 {
        flash.pages[number] = named_profile_page(first_bindings(), "x");
    }
    let (shared, handle, _flash) = g700(flash).await;

    let result = edit(&shared, vec![FlashProfileEdit::Duplicate(profile(1))]).await;

    assert!(matches!(result, Err(G700Error::NoFreePage)), "{result:?}");
    assert!(only_reads(&handle));
}

#[tokio::test]
async fn enabling_lists_a_stored_profile_again() {
    let mut flash = dumped();
    flash.pages[8] = named_profile_page(first_bindings(), "Old");
    let (shared, _handle, flash) = g700(flash).await;

    edit(&shared, vec![FlashProfileEdit::Enable { page: 8 }])
        .await
        .unwrap();

    assert_eq!(directory_pages(&flash), [2, 3, 4, 8]);
    assert_eq!(flash.lock().unwrap().current, [0x00, 0x02, 0x00]);
}

#[tokio::test]
async fn a_failed_edit_stops_the_job_and_keeps_the_rest_queued() {
    let (shared, handle, flash) = g700(dumped()).await;
    let mut job = G700Job {
        edits: VecDeque::from([
            FlashProfileEdit::Rename {
                profile: profile(1),
                name: "Работа".into(),
            },
            // Page 5 holds a profile, but outside the profile pages.
            FlashProfileEdit::Enable { page: 5 },
            FlashProfileEdit::Rename {
                profile: profile(2),
                name: "Игры".into(),
            },
        ]),
        ..G700Job::default()
    };

    let result = sync_g700_on(&shared, &mut job).await;

    assert!(
        matches!(result, Err(G700Error::NotAStoredProfile { page: 5 })),
        "{result:?}"
    );
    assert_eq!(job.edits.len(), 2);
    assert_eq!(page(&flash, 2).name().as_deref(), Some("Работа"));
    assert_eq!(page(&flash, 3).name().as_deref(), Some("Профиль 2"));
    assert_eq!(erased_pages(&handle), [2]);
}

#[tokio::test]
async fn a_rename_rewrites_only_the_name() {
    let (shared, _handle, flash) = g700(dumped()).await;
    let before = page(&flash, 3);

    edit(
        &shared,
        vec![FlashProfileEdit::Rename {
            profile: profile(2),
            name: "Игры".into(),
        }],
    )
    .await
    .unwrap();

    let after = page(&flash, 3);
    assert_eq!(after.name().as_deref(), Some("Игры"));
    assert_eq!(after.dpi_modes, before.dpi_modes);
    assert_eq!(after.bindings, before.bindings);
    assert!(after.to_page().has_valid_crc());
}

#[tokio::test]
async fn a_dpi_save_lands_in_the_mode_the_mouse_was_in_and_the_live_values_stay() {
    let (shared, _handle, flash) = g700(dumped()).await;
    let thousand = u16::from(raw_from_dpi(1000));
    {
        let mut flash = flash.lock().unwrap();
        // The slider moved the mouse from mode 1 (800 dpi) to 1000 dpi.
        flash.resolution = [thousand, thousand];
    }

    let synced = sync_g700_on(
        &shared,
        &mut G700Job {
            dpi: Some(DpiSave {
                from: dpi(800),
                to: dpi(1000),
            }),
            rate: Some(ReportRate::new(1000)),
            ..G700Job::default()
        },
    )
    .await
    .unwrap();

    assert!(synced.written);
    let played = page(&flash, 4);
    let raw: Vec<u8> = played.dpi_modes.iter().map(|mode| mode.x).collect();
    assert_eq!(raw, [0x11, raw_from_dpi(1000), 0x33, 0, 0]);
    assert_eq!(played.dpi_modes[1].y, raw_from_dpi(1000));
    assert_eq!(played.report_interval_millis, 1);
    assert_eq!(played.default_dpi_mode, 0, "the start mode stays");
    let flash = flash.lock().unwrap();
    assert_eq!(
        flash.resolution,
        [thousand, thousand],
        "the write reset the live DPI, which comes back"
    );
    assert_eq!(flash.interval, 1);
}

#[tokio::test]
async fn a_dpi_save_from_a_dpi_no_mode_holds_writes_nothing() {
    let (shared, handle, _flash) = g700(dumped()).await;

    let synced = sync_g700_on(
        &shared,
        &mut G700Job {
            dpi: Some(DpiSave {
                from: dpi(850),
                to: dpi(1000),
            }),
            ..G700Job::default()
        },
    )
    .await
    .unwrap();

    assert!(!synced.written);
    assert!(only_reads(&handle));
}

#[tokio::test]
async fn dpi_modes_replace_the_played_profile_modes() {
    let (shared, _handle, flash) = g700(dumped()).await;
    flash.lock().unwrap().pages[4][0x14] = 2;
    let before = page(&flash, 4);

    edit(
        &shared,
        vec![FlashProfileEdit::DpiModes(vec![dpi(800), dpi(1600)])],
    )
    .await
    .unwrap();

    let after = page(&flash, 4);
    let raw: Vec<u8> = after.dpi_modes.iter().map(|mode| mode.x).collect();
    assert_eq!(raw, [0x22, 0x44, 0, 0, 0]);
    assert_eq!(
        after.default_dpi_mode, 0,
        "mode 2 is gone, so the first starts"
    );
    for (a, b) in after.dpi_modes.iter().zip(&before.dpi_modes) {
        assert_eq!(a.leds, b.leds, "each mode keeps its LEDs");
    }
}

#[test]
fn edits_that_cannot_be_stored_are_refused_before_any_read() {
    let check = |edit| check_g700_edit(&edit);
    assert!(matches!(
        check(FlashProfileEdit::DpiModes(Vec::new())),
        Err(G700Error::DpiModeCount { count: 0 })
    ));
    assert!(matches!(
        check(FlashProfileEdit::DpiModes(vec![dpi(400); 6])),
        Err(G700Error::DpiModeCount { count: 6 })
    ));
    assert!(matches!(
        check(FlashProfileEdit::DpiModes(vec![dpi(1010)])),
        Err(G700Error::DpiNotInTable { .. })
    ));
    assert!(matches!(
        check(FlashProfileEdit::Rename {
            profile: profile(1),
            name: "x".repeat(24),
        }),
        Err(G700Error::InvalidName(_))
    ));
    check(FlashProfileEdit::DpiModes(vec![dpi(25), dpi(5700)])).unwrap();
}

#[tokio::test]
async fn activating_selects_an_enabled_profile_and_refuses_an_empty_slot() {
    let (shared, _handle, flash) = g700(dumped()).await;

    activate_g700_profile_on(&shared, profile(1)).await.unwrap();
    assert_eq!(flash.lock().unwrap().current, [0x00, 0x00, 0x00]);

    let result = activate_g700_profile_on(&shared, profile(4)).await;
    assert!(
        matches!(
            result,
            Err(G700Error::NoSuchProfile {
                index: 3,
                enabled: 3
            })
        ),
        "{result:?}"
    );
    assert_eq!(flash.lock().unwrap().current, [0x00, 0x00, 0x00]);
}
