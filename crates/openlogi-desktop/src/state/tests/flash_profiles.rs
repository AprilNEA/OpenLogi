//! Which onboard profile of a flash mouse (the G700) the Buttons panel
//! edits, and what a profile copy takes along.

use openlogi_core::binding::FlashWrite;
use openlogi_core::hid::{
    DisabledProfile, FlashProfiles, OnboardMode, OnboardProfile, OnboardState, ProfileIndex,
};

use super::super::onboard::PendingCopy;
use super::*;

fn g700_inventory() -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "USB Receiver".into(),
            vendor_id: 0x046d,
            product_id: 0xc531,
            unique_id: Some("C531-UID".into()),
        },
        paired: vec![PairedDevice {
            slot: 1,
            codename: Some("G700".into()),
            wpid: Some(0x1023),
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: None,
            capabilities: Some(Capabilities {
                pointer: true,
                report_rate: true,
                flash_bindings: true,
                ..Capabilities::default()
            }),
            settings_revision: 0,
            hidpp10: true,
        }],
    }
}

fn g700_state(config: Config) -> (AppState, DeviceKey, String) {
    let resolver = AssetResolver::new();
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[g700_inventory()],
        ..Sources::in_memory(config, &resolver, commands)
    });
    let record = state.current_record().expect("the G700").clone();
    let config_key = record
        .persistent_config_key()
        .expect("the G700 stores settings")
        .to_owned();
    (state, record.device_key(), config_key)
}

fn profile(n: u8) -> ProfileIndex {
    ProfileIndex::new(n).expect("a profile number")
}

fn listing(pages: &[u8], disabled: &[u8], write: FlashWrite) -> FlashProfiles {
    FlashProfiles {
        onboard: OnboardState {
            mode: OnboardMode::Onboard,
            active_profile: Some(profile(1)),
            profiles: (1..=5)
                .map(|n| OnboardProfile {
                    index: profile(n),
                    name: None,
                    enabled: usize::from(n) <= pages.len(),
                })
                .collect(),
        },
        active: None,
        disabled: disabled
            .iter()
            .map(|&page| DisabledProfile { page, name: None })
            .collect(),
        can_create: true,
        write,
        pages: pages.to_vec(),
    }
}

fn g6(state: &AppState, key: &str, page: u8) -> Option<Binding> {
    state
        .config
        .flash_profile_config(key, page)
        .devices
        .get(key)?
        .bindings
        .get(&ButtonId::G6)
        .cloned()
}

#[test]
fn the_buttons_panel_edits_the_profile_the_mouse_plays() {
    let (mut state, key, config_key) = g700_state(Config::ephemeral());
    assert!(state.follow_flash_profile(&key, 4));
    let _ = state.commit_binding(ButtonId::G6, Action::Copy);
    assert!(!state.follow_flash_profile(&key, 4), "already edited");

    // The mouse switched to the profile on page 2: its buttons are its own.
    assert!(state.follow_flash_profile(&key, 2));
    assert_ne!(
        state.button_bindings().get(&ButtonId::G6),
        Some(&Action::Copy),
        "page 2 has no binding of its own"
    );
    assert_eq!(g6(&state, &config_key, 2), None);
    let _ = state.commit_binding(ButtonId::G6, Action::Paste);
    assert_eq!(
        g6(&state, &config_key, 4),
        Some(Binding::Single(Action::Copy))
    );
    assert_eq!(
        g6(&state, &config_key, 2),
        Some(Binding::Single(Action::Paste))
    );

    // And back: page 4's binding is where it was left.
    assert!(state.follow_flash_profile(&key, 4));
    assert_eq!(
        state.button_bindings().get(&ButtonId::G6),
        Some(&Action::Copy)
    );
}

#[test]
fn a_landed_copy_takes_its_sources_bindings() {
    let (mut state, key, config_key) = g700_state(Config::ephemeral());
    state.follow_flash_profile(&key, 3);
    let _ = state.commit_binding(ButtonId::G6, Action::Copy);
    let before = listing(&[2, 3, 4], &[5], FlashWrite::Idle);
    let copy = PendingCopy::of(&before, profile(2)).expect("profile 2 is listed");
    state.pointer.flash_copies.insert(key.clone(), copy);

    // Still writing: nothing yet.
    state.settle_flash_copy(&key, &listing(&[2, 3, 4], &[5], FlashWrite::Writing));
    assert_eq!(g6(&state, &config_key, 8), None);
    // A stale reading from before the copy changes nothing either.
    state.settle_flash_copy(&key, &before);
    assert!(state.pointer.flash_copies.contains_key(&key));

    state.settle_flash_copy(&key, &listing(&[2, 3, 4, 8], &[5], FlashWrite::Idle));
    assert_eq!(
        g6(&state, &config_key, 8),
        Some(Binding::Single(Action::Copy))
    );
    assert!(!state.pointer.flash_copies.contains_key(&key));
}

#[test]
fn enabling_a_disabled_profile_is_no_copy_and_a_failed_copy_is_forgotten() {
    let (mut state, key, config_key) = g700_state(Config::ephemeral());
    state.follow_flash_profile(&key, 3);
    let _ = state.commit_binding(ButtonId::G6, Action::Copy);
    let before = listing(&[2, 3, 4], &[5, 9], FlashWrite::Idle);
    let copy = PendingCopy::of(&before, profile(2)).expect("listed");
    state.pointer.flash_copies.insert(key.clone(), copy.clone());

    // Page 9 was stored before: it was enabled, not copied to.
    state.settle_flash_copy(&key, &listing(&[2, 3, 4, 9], &[5], FlashWrite::Idle));
    assert_eq!(g6(&state, &config_key, 9), None);

    state.settle_flash_copy(
        &key,
        &listing(&[2, 3, 4], &[5, 9], FlashWrite::Failed("gone".into())),
    );
    assert!(!state.pointer.flash_copies.contains_key(&key));
}

#[test]
fn only_a_listed_profile_can_be_copied_from() {
    let profiles = listing(&[2, 3, 4], &[], FlashWrite::Idle);
    assert!(PendingCopy::of(&profiles, profile(3)).is_some());
    assert!(
        PendingCopy::of(&profiles, profile(5)).is_none(),
        "an empty slot"
    );
}
