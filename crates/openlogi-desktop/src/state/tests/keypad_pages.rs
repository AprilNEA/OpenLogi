//! Keypad binding pages: the page count is a per-device config fact the
//! Keys view grows and shrinks, and shrinking takes the removed page's grid
//! bindings with it.

use super::*;
use crate::services::ipc::Command;
use openlogi_core::binding::{Cid, GridKey, GridSlot, KeypadPage};
use openlogi_core::config::DeviceIdentity;
use openlogi_core::device_order::PhysicalDeviceKey;

pub(crate) fn keypad_state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    numpad_state(
        ReceiverInfo {
            name: "MX Creative Keypad".to_string(),
            vendor_id: 0x046d,
            product_id: 0xc354,
            unique_id: None,
        },
        openlogi_core::hid::DIRECT_DEVICE_INDEX,
    )
}

/// A state whose one device is a number pad reached through `receiver` at
/// `slot` — the MX Creative Keypad's own direct node, or any other.
fn numpad_state(
    receiver: ReceiverInfo,
    slot: u8,
) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let inventory = DeviceInventory {
        receiver,
        paired: vec![PairedDevice {
            slot,
            codename: Some("MX Creative Keypad".to_string()),
            wpid: None,
            kind: DeviceKind::Numpad,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 4,
                serial_number: Some("KEYPAD-0001".to_string()),
                unit_id: [0x4f, 0x4c, 0x44, 0x03],
                transports: DeviceTransports::default(),
                model_ids: [0xc354, 0, 0],
                extended_model_id: 0,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Numpad)),
            settings_revision: 0,
        }],
    };
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

fn page(number: u8) -> KeypadPage {
    KeypadPage::new(number).unwrap()
}

fn slot(number: u8, key: GridKey) -> ButtonId {
    ButtonId::Grid(GridSlot {
        page: page(number),
        key,
    })
}

/// The keypad's grid and paging keys are its identity, not every number
/// pad's: one paired to a receiver reports the same kind but is not it.
#[test]
fn only_the_known_keypad_is_a_keypad() {
    let (keypad, _commands) = keypad_state();
    assert_eq!(keypad.current_record().unwrap().kind, DeviceKind::Numpad);
    assert_eq!(
        keypad.current_keypad().map(|keypad| keypad.name),
        Some("MX Creative Keypad")
    );

    let (numpad, _commands) = numpad_state(
        ReceiverInfo {
            name: "Bolt Receiver".to_string(),
            vendor_id: 0x046d,
            product_id: 0xc548,
            unique_id: Some("BOLT-0001".to_string()),
        },
        1,
    );
    assert_eq!(numpad.current_record().unwrap().kind, DeviceKind::Numpad);
    assert_eq!(numpad.current_keypad(), None);
}

/// A state whose one device is an unplugged number pad, saved under its
/// serial with the route it was last reached on.
fn offline_numpad_state(route_key: &str) -> AppState {
    let mut config = Config::ephemeral();
    config.set_device_identity(
        "serial:keypad-0001",
        DeviceIdentity {
            display_name: "MX Creative Keypad".to_string(),
            kind: DeviceKind::Numpad,
            capabilities: Capabilities::presumed_from_kind(DeviceKind::Numpad),
            light_capabilities: None,
            model_info: None,
            codename: None,
            driver_id: None,
            registry_model_id: None,
        },
    );
    let canonical = PhysicalDeviceKey::parse("serial:keypad-0001").unwrap();
    assert!(config.adopt_route(&canonical, route_key, None));
    let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
    AppState::new(Sources::in_memory(config, &AssetResolver::new(), commands))
}

/// Unplugged, the keypad has no live route, but its saved entry still says
/// it was reached as that keypad: it keeps its keys and app profiles rather
/// than falling back to a keyboard's F-row.
#[test]
fn an_unplugged_keypad_is_still_the_keypad() {
    let state = offline_numpad_state("direct:046d:c354");
    let record = state.current_record().unwrap();
    assert!(!record.online);
    assert_eq!(record.route, None);
    assert_eq!(
        state.current_keypad().map(|keypad| keypad.name),
        Some("MX Creative Keypad")
    );

    let other = offline_numpad_state("direct:046d:c548");
    assert_eq!(other.current_keypad(), None);
}

#[test]
fn adding_pages_persists_and_seeds_the_paging_buttons() {
    let (mut state, mut receiver) = keypad_state();
    assert_eq!(state.current_keypad_pages(), page(1));
    let right = ButtonId::Control(Cid::RIGHT_PAGING);
    assert!(!state.button_bindings().contains_key(&right));

    let events = state.commit_keypad_pages(page(3));

    let key = state.current_record().unwrap().device_key();
    assert_eq!(events, [StateEvent::BindingsChanged(key)]);
    assert_eq!(state.current_keypad_pages(), page(3));
    // More than one page: the paging buttons now page, visibly.
    assert_eq!(
        state.button_bindings().get(&right),
        Some(&Action::NextKeypadPage)
    );
    assert!(
        std::iter::from_fn(|| receiver.try_recv().ok())
            .any(|command| matches!(command, Command::ReloadConfig(_))),
        "the agent reloads the new page count"
    );
}

#[test]
fn removing_a_page_drops_its_bindings_and_keeps_the_rest() {
    let (mut state, _receiver) = keypad_state();
    let _ = state.commit_keypad_pages(page(2));
    let _ = state.commit_binding(slot(1, GridKey::Middle), Action::Copy);
    let _ = state.commit_binding(slot(2, GridKey::Middle), Action::Paste);

    let _ = state.commit_keypad_pages(page(1));

    assert_eq!(state.current_keypad_pages(), page(1));
    assert_eq!(
        state.button_bindings().get(&slot(1, GridKey::Middle)),
        Some(&Action::Copy)
    );
    assert!(
        !state
            .button_bindings()
            .contains_key(&slot(2, GridKey::Middle))
    );
    // Back to one page: nothing to page through, the buttons go native.
    assert!(
        !state
            .button_bindings()
            .contains_key(&ButtonId::Control(Cid::LEFT_PAGING))
    );
}

#[test]
fn an_app_profile_binds_keypad_keys_over_default() {
    let (mut state, _receiver) = keypad_state();
    assert!(state.current_device_is_persistent());
    let _ = state.commit_keypad_pages(page(2));
    let _ = state.commit_binding(slot(2, GridKey::Middle), Action::Copy);

    let _ = state.set_editing_app(Some("com.adobe.Photoshop".into()));
    assert_eq!(
        state.button_bindings().get(&slot(2, GridKey::Middle)),
        Some(&Action::Copy),
        "an app profile inherits Default's keys"
    );
    let _ = state.commit_binding(slot(2, GridKey::Middle), Action::Paste);
    assert_eq!(
        state.button_bindings().get(&slot(2, GridKey::Middle)),
        Some(&Action::Paste)
    );
    assert_eq!(state.app_profiles().count(), 1);

    let _ = state.set_editing_app(None);
    assert_eq!(
        state.button_bindings().get(&slot(2, GridKey::Middle)),
        Some(&Action::Copy),
        "Default keeps its own key"
    );
}

#[test]
fn a_page_bound_only_in_an_app_profile_still_counts_as_bound() {
    let (mut state, _receiver) = keypad_state();
    let _ = state.commit_keypad_pages(page(2));
    assert!(!state.current_keypad_page_binds(page(2)));

    let _ = state.set_editing_app(Some("com.adobe.Photoshop".into()));
    let _ = state.commit_binding(slot(2, GridKey::TopLeft), Action::Cut);
    let _ = state.set_editing_app(None);

    // Default binds nothing there, yet removing page 2 would drop the app's key.
    assert!(state.current_keypad_page_binds(page(2)));
}
