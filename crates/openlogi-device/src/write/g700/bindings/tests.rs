use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hidpp::register::onboard_memory::{
    PAGE_SIZE, Page,
    g700::{Binding, ButtonMask, G700Profile, KeyModifiers, MacroPointer, Special},
};
use openlogi_core::binding::{
    ButtonId, FlashBinding, FlashFunction, G700, MouseButtonNumber, SlotIntent,
};

use super::{flash_binding, sync_g700_bindings_on, wire_binding};
use crate::SharedChannel;
use crate::channel::route::DeviceRoute;
use crate::channel::scripted::{ScriptedRawHidChannel, ScriptedRawHidHandle, scripted_channel};

const DEVICE: u8 = 1;

/// The flash of a G700 as far as a binding sync touches it: pages that
/// read, the current-profile register, the HOT upload into the RAM page and
/// the `0xA0` erase and copy. A copy can only clear bits, as on NOR flash.
struct Flash {
    pages: Vec<[u8; PAGE_SIZE]>,
    current: [u8; 3],
    upload: Option<(usize, usize)>,
}

impl Flash {
    fn respond(&mut self, report: &[u8]) -> Option<Vec<u8>> {
        match *report {
            [0x10, DEVICE, 0x81, 0x0f, ..] => {
                let [a, b, c] = self.current;
                Some(vec![0x10, DEVICE, 0x81, 0x0f, a, b, c])
            }
            [0x10, DEVICE, 0x80, 0x0f, a, b, c] => {
                self.current = [a, b, c];
                Some(vec![0x10, DEVICE, 0x80, 0x0f, 0, 0, 0])
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

fn button(n: u8) -> Binding {
    Binding::MouseButtons(ButtonMask(1 << (n - 1)))
}

/// A profile page holding `bindings`, with a valid CRC.
fn profile_page(bindings: [Binding; 13]) -> [u8; PAGE_SIZE] {
    let mut blank = Page::from_bytes([0xff; PAGE_SIZE]);
    blank.update_crc();
    let mut profile = G700Profile::parse(&blank);
    profile.bindings = bindings;
    let mut page = profile.to_page();
    page.update_crc();
    *page.as_bytes()
}

/// The user's three profiles as dumped on 2026-10-31, the third played.
fn dumped() -> Flash {
    let special = Binding::Special;
    let first = [
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
    ];
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
    pages[2] = profile_page(first);
    pages[3] = profile_page(first);
    pages[4] = profile_page(third);
    Flash {
        pages,
        current: [0x00, 0x02, 0x00],
        upload: None,
    }
}

async fn g700(flash: Flash) -> (SharedChannel, ScriptedRawHidHandle, Arc<Mutex<Flash>>) {
    let flash = Arc::new(Mutex::new(flash));
    let responder = Arc::clone(&flash);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        responder.lock().unwrap().respond(request)
    });
    let shared = SharedChannel::new(
        scripted_channel(raw.presenting_as(0xc531)).await,
        DeviceRoute::Unifying {
            receiver_uid: format!("g700-bindings-{}", std::process::id()),
            slot: 1,
        },
    );
    (shared, handle, flash)
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

    let sync = sync_g700_bindings_on(&shared, &intents(&[])).await.unwrap();

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

    let sync = sync_g700_bindings_on(
        &shared,
        &intents(&[
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
    let again = sync_g700_bindings_on(
        &shared,
        &intents(&[
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

    let sync = sync_g700_bindings_on(&shared, &intents(&[(ButtonId::G8, SlotIntent::Agent)]))
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

    let sync = sync_g700_bindings_on(&shared, &intents(&[(ButtonId::G6, SlotIntent::Agent)]))
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

    let result = sync_g700_bindings_on(&shared, &intents(&[])).await;

    assert!(
        matches!(
            result,
            Err(super::G700Error::Device(
                crate::write::WriteError::DeviceNotFound
            ))
        ),
        "{result:?}"
    );
    assert!(handle.written_reports().is_empty());
}

#[test]
fn every_profile_binding_converts_losslessly() {
    let bindings = [
        button(1),
        Binding::MouseButtons(ButtonMask(0x8003)),
        Binding::Key {
            modifiers: KeyModifiers::RIGHT_GUI | KeyModifiers::LEFT_SHIFT,
            usage: 0x2b,
        },
        Binding::Consumer(0x0192),
        Binding::Disabled,
        Binding::Macro(MacroPointer {
            page: 7,
            word_offset: 0x20,
        }),
        Binding::Unknown([0x85, 0x01, 0x02]),
        Binding::Unknown([0x8f, 0x01, 0x00]),
        Binding::Special(Special::Unknown(0x40)),
        Binding::Special(Special::ProfileCycleDown),
        Binding::Special(Special::ResolutionCycleDown),
        Binding::Special(Special::BatteryLevel),
    ];
    for binding in bindings {
        assert_eq!(
            wire_binding(flash_binding(binding)).to_bytes(),
            binding.to_bytes(),
            "{binding:?}"
        );
    }
    assert_eq!(
        flash_binding(Binding::Special(Special::Unknown(0x40))),
        FlashBinding::Function(FlashFunction::Other(0x40))
    );
}
