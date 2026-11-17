//! Key modules on the orchestrator's side: the keys each owns, the faces it
//! draws, and where its presses go.

use std::collections::BTreeMap;

use openlogi_core::binding::{ActionRingIcon, GridSlot, LongPressBinding};
use openlogi_core::module::ModuleId;

use super::device_settings::{on_page, page, wired_keypad};
use super::*;
use crate::modules::{ModuleEvent, ModuleKey, ModulePress};
use crate::runtime::PageTurn;
use crate::watchers::key_display::{KeyFace, KeyImage};

fn spotify() -> ModuleId {
    ModuleId::try_new("com.example.spotify".to_owned()).unwrap()
}

fn weather() -> ModuleId {
    ModuleId::try_new("com.example.weather".to_owned()).unwrap()
}

fn slot(number: u8, key: GridKey) -> GridSlot {
    GridSlot {
        page: page(number),
        key,
    }
}

/// A two-page keypad: Spotify on page 1's middle key and page 2's top-left,
/// Copy on page 1's top-left, weather on page 2's middle; Spotify turned on.
fn module_keypad() -> Orchestrator {
    let mut config = Config::default();
    config.set_keypad_pages("keypad", page(2));
    for (number, key, action) in [
        (1, GridKey::Middle, Action::Module(spotify())),
        (1, GridKey::TopLeft, Action::Copy),
        (2, GridKey::TopLeft, Action::Module(spotify())),
        (2, GridKey::Middle, Action::Module(weather())),
    ] {
        config.set_binding("keypad", on_page(number, key), Binding::Single(action));
    }
    config.set_module_enabled(&spotify(), true);
    let mut orch = orchestrator(config.clone());
    orch.devices = vec![wired_keypad("keypad", true)];
    orch.reload_config(config);
    orch
}

fn published_face(orch: &Orchestrator, key: GridKey) -> KeyFace {
    let specs = orch.shared.key_display_specs.borrow();
    let [spec] = specs.as_slice() else {
        panic!("one keypad publishes one spec: {specs:?}");
    };
    spec.faces[&key].clone()
}

fn published_keys(orch: &Orchestrator) -> BTreeMap<ModuleId, Vec<ModuleKey>> {
    orch.shared.module_plan.borrow().keys.clone()
}

fn owned(module: ModuleId, slots: &[GridSlot]) -> (ModuleId, Vec<ModuleKey>) {
    let size = openlogi_hid::key_display_for(&super::device_settings::keypad_route())
        .expect("the keypad's keys are displays")
        .key_size;
    (
        module,
        slots
            .iter()
            .map(|slot| ModuleKey {
                device: "keypad".into(),
                slot: *slot,
                size,
            })
            .collect(),
    )
}

/// The plan names the keys of the current page whose binding is a module,
/// and follows a page turn and a rebind; it carries the `[modules]` table.
#[test]
fn the_keys_a_module_owns_follow_page_turns_and_rebinds() {
    let mut orch = module_keypad();
    assert_eq!(
        published_keys(&orch),
        BTreeMap::from([owned(spotify(), &[slot(1, GridKey::Middle)])])
    );
    assert!(orch.shared.module_plan.borrow().config[&spotify()].enabled);

    orch.turn_keypad_page(Some("keypad"), PageTurn::Next);
    assert_eq!(
        published_keys(&orch),
        BTreeMap::from([
            owned(spotify(), &[slot(2, GridKey::TopLeft)]),
            owned(weather(), &[slot(2, GridKey::Middle)]),
        ])
    );

    let mut config = orch.config.clone();
    config.set_binding(
        "keypad",
        on_page(2, GridKey::TopLeft),
        Binding::Single(Action::Paste),
    );
    orch.reload_config(config);
    assert_eq!(
        published_keys(&orch),
        BTreeMap::from([owned(weather(), &[slot(2, GridKey::Middle)])])
    );
}

/// A module's key shows the module glyph until the module draws it, then
/// its image, and the glyph again once the module stops.
#[test]
fn a_module_key_shows_the_glyph_then_its_image_then_the_glyph_again() {
    let mut orch = module_keypad();
    let glyph = KeyFace::Icon(ActionRingIcon::Module);
    assert_eq!(published_face(&orch, GridKey::Middle), glyph);
    assert_eq!(
        published_face(&orch, GridKey::TopLeft),
        KeyFace::Icon(ActionRingIcon::Copy)
    );

    let art = KeyImage::solid(8, [30, 215, 96]);
    orch.apply_module_event(ModuleEvent::Image {
        module: spotify(),
        device: "keypad".into(),
        slot: slot(1, GridKey::Middle),
        image: art.clone(),
    });
    assert_eq!(published_face(&orch, GridKey::Middle), KeyFace::Image(art));

    orch.apply_module_event(ModuleEvent::Stopped { module: spotify() });
    assert_eq!(published_face(&orch, GridKey::Middle), glyph);
    assert!(orch.module_faces.is_empty());
}

/// An image counts only for a key the module owns as it arrives: one for
/// a key on another page, another keypad, or another module's key is
/// dropped, and a clear brings the glyph back.
#[test]
fn an_image_for_a_key_the_module_does_not_own_is_dropped() {
    let mut orch = module_keypad();
    let art = KeyImage::solid(8, [255, 0, 0]);
    for (module, device, at) in [
        (spotify(), "keypad", slot(2, GridKey::TopLeft)),
        (spotify(), "other", slot(1, GridKey::Middle)),
        (weather(), "keypad", slot(1, GridKey::Middle)),
        (spotify(), "keypad", slot(1, GridKey::TopLeft)),
    ] {
        orch.apply_module_event(ModuleEvent::Image {
            module,
            device: device.into(),
            slot: at,
            image: art.clone(),
        });
    }
    assert!(orch.module_faces.is_empty());

    orch.apply_module_event(ModuleEvent::Image {
        module: spotify(),
        device: "keypad".into(),
        slot: slot(1, GridKey::Middle),
        image: art.clone(),
    });
    orch.apply_module_event(ModuleEvent::Clear {
        module: spotify(),
        device: "keypad".into(),
        slot: slot(1, GridKey::Middle),
    });
    assert_eq!(
        published_face(&orch, GridKey::Middle),
        KeyFace::Icon(ActionRingIcon::Module)
    );
}

/// `image_all` covers the keys the module owns when it arrives; a key it
/// comes to own later shows the glyph until the module sends again, and a
/// key it owns again shows the image it last sent for it.
#[test]
fn image_all_covers_the_keys_owned_when_it_arrives() {
    let mut orch = module_keypad();
    let art = KeyImage::solid(8, [0, 0, 255]);
    orch.apply_module_event(ModuleEvent::ImageAll {
        module: spotify(),
        image: art.clone(),
    });
    assert_eq!(
        published_face(&orch, GridKey::Middle),
        KeyFace::Image(art.clone())
    );

    orch.turn_keypad_page(Some("keypad"), PageTurn::Next);
    assert_eq!(
        published_face(&orch, GridKey::TopLeft),
        KeyFace::Icon(ActionRingIcon::Module),
        "owned only after the image arrived"
    );
    let fresh = KeyImage::solid(8, [9, 9, 9]);
    orch.apply_module_event(ModuleEvent::ImageAll {
        module: spotify(),
        image: fresh.clone(),
    });
    assert_eq!(
        published_face(&orch, GridKey::TopLeft),
        KeyFace::Image(fresh)
    );

    orch.turn_keypad_page(Some("keypad"), PageTurn::Previous);
    assert_eq!(
        published_face(&orch, GridKey::Middle),
        KeyFace::Image(art),
        "the page 1 key shows its last image again"
    );
}

/// A key shows a module's image only while that module owns it: bound to
/// something else by the app in front, or rebound to another module, it
/// shows that binding's face, and the image again once the module owns it
/// again.
#[test]
fn a_key_the_module_no_longer_owns_shows_its_own_binding() {
    let mut orch = module_keypad();
    let mut config = orch.config.clone();
    config.set_per_app_binding(
        "keypad",
        "com.example.editor",
        on_page(1, GridKey::Middle),
        Some(Action::Paste),
    );
    orch.reload_config(config);
    let art = KeyImage::solid(8, [30, 215, 96]);
    orch.apply_module_event(ModuleEvent::Image {
        module: spotify(),
        device: "keypad".into(),
        slot: slot(1, GridKey::Middle),
        image: art.clone(),
    });
    assert_eq!(
        published_face(&orch, GridKey::Middle),
        KeyFace::Image(art.clone())
    );

    orch.set_current_app(Some(ForegroundApp::unnamed("com.example.editor".into())));
    assert_eq!(
        published_face(&orch, GridKey::Middle),
        KeyFace::Icon(ActionRingIcon::Paste),
        "the editor's own binding"
    );
    orch.set_current_app(Some(ForegroundApp::unnamed("com.example.finder".into())));
    assert_eq!(published_face(&orch, GridKey::Middle), KeyFace::Image(art));

    let mut config = orch.config.clone();
    config.set_binding(
        "keypad",
        on_page(1, GridKey::Middle),
        Binding::Single(Action::Module(weather())),
    );
    orch.reload_config(config);
    assert_eq!(
        published_face(&orch, GridKey::Middle),
        KeyFace::Icon(ActionRingIcon::Module),
        "weather has drawn nothing"
    );
}

/// Each reload that turns a module off counts once in the plan, so the
/// host sees the toggle even when the next reload turns it on again
/// before it looks.
#[test]
fn turning_a_module_off_counts_in_the_plan() {
    let mut orch = module_keypad();
    let turned_off = |orch: &Orchestrator| {
        orch.shared
            .module_plan
            .borrow()
            .turned_off
            .get(&spotify())
            .copied()
    };
    assert_eq!(turned_off(&orch), None);

    let mut config = orch.config.clone();
    config.set_module_enabled(&spotify(), false);
    orch.reload_config(config.clone());
    orch.reload_config(config.clone());
    assert_eq!(turned_off(&orch), Some(1), "staying off counts nothing");

    config.set_module_enabled(&spotify(), true);
    orch.reload_config(config.clone());
    assert_eq!(turned_off(&orch), Some(1), "turning it on counts nothing");
    config.set_module_enabled(&spotify(), false);
    orch.reload_config(config);
    assert_eq!(turned_off(&orch), Some(2));
}

/// A press goes to the module the key is bound to on the keypad's current
/// page, at that page's slot; a key rebound since goes nowhere.
#[test]
fn a_module_press_goes_to_the_key_on_the_current_page() {
    let mut orch = module_keypad();
    assert_eq!(
        orch.module_press(Some("keypad"), &spotify(), GridKey::Middle),
        Some(ModulePress {
            module: spotify(),
            device: "keypad".into(),
            slot: slot(1, GridKey::Middle),
        })
    );
    assert_eq!(
        orch.module_press(Some("keypad"), &weather(), GridKey::Middle),
        None
    );
    assert_eq!(orch.module_press(None, &spotify(), GridKey::Middle), None);

    orch.turn_keypad_page(Some("keypad"), PageTurn::Next);
    assert_eq!(
        orch.module_press(Some("keypad"), &spotify(), GridKey::Middle),
        None,
        "page 2's middle key is weather's"
    );
    assert_eq!(
        orch.module_press(Some("keypad"), &spotify(), GridKey::TopLeft),
        Some(ModulePress {
            module: spotify(),
            device: "keypad".into(),
            slot: slot(2, GridKey::TopLeft),
        })
    );
}

/// A key that runs a module on a long press and something else on a short
/// one belongs to the module: the module is told it owns the key, draws on
/// it, and hears its long presses, which go nowhere else.
#[test]
fn a_module_on_a_long_press_owns_its_key() {
    let mut orch = module_keypad();
    let mut config = orch.config.clone();
    config.set_binding(
        "keypad",
        on_page(1, GridKey::TopLeft),
        Binding::LongPress(LongPressBinding::new(
            Action::Copy,
            Action::Module(spotify()),
        )),
    );
    orch.reload_config(config);
    assert_eq!(
        published_keys(&orch),
        BTreeMap::from([owned(
            spotify(),
            &[slot(1, GridKey::TopLeft), slot(1, GridKey::Middle)]
        )])
    );
    assert_eq!(
        orch.module_press(Some("keypad"), &spotify(), GridKey::TopLeft),
        Some(ModulePress {
            module: spotify(),
            device: "keypad".into(),
            slot: slot(1, GridKey::TopLeft),
        })
    );
    let art = KeyImage::solid(4, [0, 0, 255]);
    orch.apply_module_event(ModuleEvent::Image {
        module: spotify(),
        device: "keypad".into(),
        slot: slot(1, GridKey::TopLeft),
        image: art.clone(),
    });
    assert_eq!(published_face(&orch, GridKey::TopLeft), KeyFace::Image(art));
}
