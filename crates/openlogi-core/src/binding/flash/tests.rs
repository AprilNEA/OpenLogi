use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::binding::{G700, GamingButtons, LongPressBinding};
use crate::device::Capabilities;

/// A host whose hook sees the agent buttons and whose Command is Control.
const LINUX: FlashHost = FlashHost {
    modifiers: ModifierHost::Other,
    hook: AgentButtonHook::Sees,
};

fn button(n: u8) -> MouseButtonNumber {
    MouseButtonNumber::try_new(n).unwrap()
}

fn chord(text: &str) -> Action {
    Action::CustomShortcut(text.parse().unwrap())
}

/// Every action the mouse can run, with the binding it is stored as.
fn native_actions() -> Vec<(Action, FlashBinding)> {
    let mut actions = vec![
        (Action::LeftClick, FlashBinding::MouseButtons(0x0001)),
        (Action::RightClick, FlashBinding::MouseButtons(0x0002)),
        (Action::MiddleClick, FlashBinding::MouseButtons(0x0004)),
        (Action::MouseBack, FlashBinding::MouseButtons(0x0008)),
        (Action::MouseForward, FlashBinding::MouseButtons(0x0010)),
        (Action::PlayPause, FlashBinding::Consumer(0x00cd)),
        (Action::NextTrack, FlashBinding::Consumer(0x00b5)),
        (Action::PrevTrack, FlashBinding::Consumer(0x00b6)),
        (Action::VolumeUp, FlashBinding::Consumer(0x00e9)),
        (Action::VolumeDown, FlashBinding::Consumer(0x00ea)),
        (Action::MuteVolume, FlashBinding::Consumer(0x00e2)),
        (
            Action::NextDpiPreset,
            FlashBinding::Function(FlashFunction::ResolutionUp),
        ),
        (
            Action::PreviousDpiPreset,
            FlashBinding::Function(FlashFunction::ResolutionDown),
        ),
        (
            Action::CycleDpiPresets,
            FlashBinding::Function(FlashFunction::ResolutionCycleUp),
        ),
        (
            Action::NextOnboardProfile,
            FlashBinding::Function(FlashFunction::ProfileUp),
        ),
        (
            Action::PreviousOnboardProfile,
            FlashBinding::Function(FlashFunction::ProfileDown),
        ),
        (
            Action::CycleOnboardProfiles,
            FlashBinding::Function(FlashFunction::ProfileCycleUp),
        ),
        (
            Action::ShowBatteryLevel,
            FlashBinding::Function(FlashFunction::BatteryLevel),
        ),
        (
            Action::HorizontalScrollLeft,
            FlashBinding::Function(FlashFunction::WheelLeft),
        ),
        (
            Action::HorizontalScrollRight,
            FlashBinding::Function(FlashFunction::WheelRight),
        ),
        (Action::None, FlashBinding::Disabled),
        // Modifier flags are set: libratbag's writer drops them.
        (
            chord("Ctrl+Shift+F5"),
            FlashBinding::Key {
                modifiers: 0x03,
                usage: 0x3e,
            },
        ),
        (
            chord("Alt+Tab"),
            FlashBinding::Key {
                modifiers: 0x04,
                usage: 0x2b,
            },
        ),
        (
            chord("Super+L"),
            FlashBinding::Key {
                modifiers: 0x08,
                usage: 0x0f,
            },
        ),
        (
            chord("Escape"),
            FlashBinding::Key {
                modifiers: 0,
                usage: 0x29,
            },
        ),
    ];
    actions.extend((6..=16).map(|n| {
        (
            Action::MouseButton(button(n)),
            FlashBinding::MouseButtons(1 << (n - 1)),
        )
    }));
    actions
}

#[test]
fn every_native_action_round_trips_through_its_flash_binding() {
    for (action, binding) in native_actions() {
        assert_eq!(
            FlashBinding::of_action(&action, ModifierHost::Other),
            Some(binding),
            "{action:?}"
        );
        assert_eq!(
            binding.action(ModifierHost::Other),
            Some(action.clone()),
            "{binding:?}"
        );
    }
}

#[test]
fn every_flash_binding_an_action_names_writes_back_unchanged() {
    let mut bindings: Vec<FlashBinding> = native_actions().into_iter().map(|(_, b)| b).collect();
    bindings.push(FlashBinding::Key {
        modifiers: 0x11,
        usage: 0x06,
    });
    bindings.push(FlashBinding::Key {
        modifiers: 0x88,
        usage: 0x06,
    });
    for host in [ModifierHost::MacOs, ModifierHost::Other] {
        for binding in &bindings {
            let action = binding.action(host).unwrap();
            let back = FlashBinding::of_action(&action, host).unwrap();
            // Either key of a modifier pair reads as the pair; it writes back
            // as the left key.
            let expected = match *binding {
                FlashBinding::Key { modifiers, usage } => FlashBinding::Key {
                    modifiers: (modifiers | (modifiers >> 4)) & 0x0f,
                    usage,
                },
                other => other,
            };
            assert_eq!(back, expected, "{binding:?} on {host:?}");
        }
    }
}

#[test]
fn command_is_the_hosts_primary_modifier() {
    let copy = chord("Cmd+C");
    let on_linux = FlashBinding::of_action(&copy, ModifierHost::Other).unwrap();
    let on_mac = FlashBinding::of_action(&copy, ModifierHost::MacOs).unwrap();
    assert_eq!(
        on_linux,
        FlashBinding::Key {
            modifiers: 0x01,
            usage: 0x06
        }
    );
    assert_eq!(
        on_mac,
        FlashBinding::Key {
            modifiers: 0x08,
            usage: 0x06
        }
    );
    assert_eq!(on_mac.action(ModifierHost::MacOs), Some(copy));
    assert_eq!(
        on_linux.action(ModifierHost::Other),
        Some(chord("Ctrl+C")),
        "Linux reads Control back as Control"
    );
    assert_eq!(on_mac.action(ModifierHost::Other), Some(chord("Super+C")));
}

#[test]
fn actions_only_the_host_runs_have_no_flash_binding() {
    let host_only = [
        Action::Copy,
        Action::BrowserBack,
        Action::ShowActionsRing,
        Action::MissionControl,
        Action::SetDpiPreset(1),
        Action::ToggleSmartShift,
        Action::DpiShift,
        Action::ScrollUp,
        Action::HoldShortcut("Ctrl+A".parse().unwrap()),
        Action::TypeText("hi".into()),
        Action::RunShellCommand("true".into()),
    ];
    for action in host_only {
        assert_eq!(
            FlashBinding::of_action(&action, ModifierHost::Other),
            None,
            "{action:?}"
        );
    }
}

#[test]
fn bindings_no_action_stands_for_are_kept_as_read() {
    let unnamed = [
        FlashBinding::Macro {
            page: 7,
            word_offset: 0x10,
        },
        FlashBinding::Raw([0x85, 0x01, 0x02]),
        FlashBinding::MouseButtons(0x0003),
        FlashBinding::MouseButtons(0),
        FlashBinding::Consumer(0x0192),
        FlashBinding::Function(FlashFunction::ResolutionCycleDown),
        FlashBinding::Function(FlashFunction::ProfileCycleDown),
        FlashBinding::Function(FlashFunction::Other(0x40)),
        // A modifier on its own: no shortcut holds no key.
        FlashBinding::Key {
            modifiers: 0x02,
            usage: 0,
        },
    ];
    for binding in unnamed {
        assert_eq!(binding.action(ModifierHost::Other), None, "{binding:?}");
    }
}

#[test]
fn mouse_button_numbers_are_the_sixteen_report_bits() {
    MouseButtonNumber::try_new(0).expect_err("buttons count from one");
    MouseButtonNumber::try_new(17).expect_err("a report has sixteen buttons");
    assert_eq!(button(16).mask(), 0x8000);
    assert_eq!(
        MouseButtonNumber::in_mask(0x8201).collect::<Vec<_>>(),
        [button(1), button(10), button(16)]
    );
    let toml = toml::to_string(&Action::MouseButton(button(6)).wrap()).unwrap();
    assert!(toml.contains("MouseButton = 6"), "{toml}");
    let bad: Result<Wrapped, _> = toml::from_str("action = { MouseButton = 17 }");
    assert!(bad.is_err(), "a button past 16 is rejected");
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Wrapped {
    action: Action,
}

trait Wrap {
    fn wrap(self) -> Wrapped;
}

impl Wrap for Action {
    fn wrap(self) -> Wrapped {
        Wrapped { action: self }
    }
}

/// The user's third profile as dumped on 2026-10-31: slots 8 and 9 press
/// buttons 11 and 10, slots 3 to 6 buttons 7, 6, 4 and 5.
fn dumped_profile() -> Vec<FlashBinding> {
    let b = |n: u8| FlashBinding::MouseButtons(1 << (n - 1));
    let f = FlashBinding::Function;
    vec![
        b(1),
        b(2),
        b(3),
        b(7),
        b(6),
        b(4),
        b(5),
        f(FlashFunction::BatteryLevel),
        b(11),
        b(10),
        f(FlashFunction::ProfileCycleUp),
        f(FlashFunction::WheelLeft),
        f(FlashFunction::WheelRight),
    ]
}

/// Lay out `intents` for writing, with nothing owned.
fn lay_out(
    layout: &GamingLayout,
    current: &[FlashBinding],
    reserved: &BTreeSet<MouseButtonNumber>,
    intents: &[SlotIntent],
) -> FlashPlan {
    plan_flash_bindings(
        layout,
        current,
        reserved,
        &ProfileRequest::write(intents.to_vec()),
    )
}

fn intents(changes: &[(usize, SlotIntent)]) -> Vec<SlotIntent> {
    let mut intents = vec![SlotIntent::Keep; 13];
    for (slot, intent) in changes {
        intents[*slot] = intent.clone();
    }
    intents
}

#[test]
fn keeping_every_slot_changes_nothing() {
    let current = dumped_profile();
    let plan = lay_out(&G700, &current, &BTreeSet::new(), &intents(&[]));
    assert_eq!(plan.bindings, current);
    assert!(plan.routed.is_empty() && plan.unrouted.is_empty());
}

#[test]
fn agent_slots_take_the_highest_free_buttons_the_profile_leaves() {
    let current = dumped_profile();
    let plan = lay_out(
        &G700,
        &current,
        &BTreeSet::new(),
        &intents(&[
            (5, SlotIntent::Agent),
            (6, SlotIntent::Agent),
            (7, SlotIntent::Agent),
        ]),
    );
    assert_eq!(
        plan.routed,
        BTreeMap::from([
            (button(16), ButtonId::G6),
            (button(15), ButtonId::G7),
            (button(14), ButtonId::G8),
        ])
    );
    assert_eq!(plan.bindings[5], FlashBinding::MouseButtons(0x8000));
    assert_eq!(plan.bindings[7], FlashBinding::MouseButtons(0x2000));
    assert_eq!(plan.bindings[8..], current[8..], "the rest is untouched");
}

#[test]
fn agent_buttons_never_collide_with_buttons_any_profile_presses() {
    let mut current = dumped_profile();
    // Slots 8 and 9 press 16 and 15 natively here; another profile presses 14.
    current[8] = FlashBinding::MouseButtons(0x8000);
    current[9] = FlashBinding::MouseButtons(0x4000);
    let reserved = BTreeSet::from([button(14)]);
    let all_agent: Vec<(usize, SlotIntent)> = [0, 1, 2, 3, 4, 5, 6, 7, 10, 11, 12]
        .into_iter()
        .map(|slot| (slot, SlotIntent::Agent))
        .collect();
    let plan = lay_out(&G700, &current, &reserved, &intents(&all_agent));
    let natively_pressed: BTreeSet<MouseButtonNumber> = [button(16), button(15), button(14)].into();
    assert_eq!(plan.routed.len(), 5, "9 to 13 are all that is free");
    assert!(plan.routed.keys().all(|b| !natively_pressed.contains(b)));
    let mut seen = BTreeSet::new();
    for binding in &plan.bindings {
        for pressed in binding.buttons() {
            assert!(
                seen.insert(pressed) || !is_agent_button(pressed),
                "{pressed} is pressed twice"
            );
        }
    }
    assert_eq!(plan.unrouted.len(), 6);
    for slot in [7, 10, 11, 12] {
        if plan.unrouted.contains(&G700.slots[slot].button) {
            assert_eq!(
                plan.bindings[slot], current[slot],
                "an unrouted slot is kept"
            );
        }
    }
}

#[test]
fn a_slot_keeps_the_agent_button_it_already_presses() {
    let mut current = dumped_profile();
    current[6] = FlashBinding::MouseButtons(button(12).mask());
    let plan = lay_out(
        &G700,
        &current,
        &BTreeSet::new(),
        &intents(&[(5, SlotIntent::Agent), (6, SlotIntent::Agent)]),
    );
    assert_eq!(plan.routed.get(&button(12)), Some(&ButtonId::G7));
    assert_eq!(plan.routed.get(&button(16)), Some(&ButtonId::G6));
    assert_eq!(plan.bindings[6], current[6]);
}

#[test]
fn a_native_agent_button_elsewhere_takes_it_from_the_slot_that_held_it() {
    let mut current = dumped_profile();
    current[5] = FlashBinding::MouseButtons(button(16).mask());
    // Slot 6 now presses 16 natively; slot 5 must move.
    let plan = lay_out(
        &G700,
        &current,
        &BTreeSet::new(),
        &intents(&[
            (5, SlotIntent::Agent),
            (6, SlotIntent::Native(FlashBinding::MouseButtons(0x8000))),
        ]),
    );
    assert_eq!(plan.bindings[6], FlashBinding::MouseButtons(0x8000));
    assert_eq!(plan.routed.get(&button(15)), Some(&ButtonId::G6));
}

#[test]
fn a_macro_slot_is_kept_unless_the_configuration_names_the_slot() {
    let mut current = dumped_profile();
    let macro_binding = FlashBinding::Macro {
        page: 7,
        word_offset: 0,
    };
    current[5] = macro_binding;
    current[6] = FlashBinding::Raw([0x85, 0, 1]);
    let plan = lay_out(&G700, &current, &BTreeSet::new(), &intents(&[]));
    assert_eq!(plan.bindings[5], macro_binding);
    assert_eq!(plan.bindings[6], FlashBinding::Raw([0x85, 0, 1]));
    let plan = lay_out(
        &G700,
        &current,
        &BTreeSet::new(),
        &intents(&[(5, SlotIntent::Native(FlashBinding::Disabled))]),
    );
    assert_eq!(plan.bindings[5], FlashBinding::Disabled);
    assert_eq!(plan.bindings[6], FlashBinding::Raw([0x85, 0, 1]));
}

#[test]
fn intents_follow_the_stored_bindings() {
    let key = "receiver:abc:slot:1";
    let mut config = Config::default();
    config.set_binding(key, ButtonId::G6, Binding::Single(chord("Ctrl+C")));
    config.set_binding(key, ButtonId::G7, Binding::Single(Action::Copy));
    config.set_binding(key, ButtonId::G8, Binding::Single(Action::MouseBack));
    config.set_binding(
        key,
        ButtonId::G9,
        Binding::LongPress(LongPressBinding::new(Action::MouseBack, Action::Copy)),
    );
    config.set_binding(
        key,
        ButtonId::G10,
        Binding::Single(Action::ShowBatteryLevel),
    );
    config.set_binding(key, ButtonId::Back, Binding::Single(Action::MouseBack));
    config.set_per_app_binding(key, "org.mozilla.firefox", ButtonId::G8, Some(Action::Copy));
    config.set_per_app_binding(
        key,
        "org.mozilla.firefox",
        ButtonId::G10,
        Some(Action::Copy),
    );
    config.set_per_app_binding(
        key,
        "org.mozilla.firefox",
        ButtonId::G11,
        Some(Action::Copy),
    );

    let intents = flash_intents(&config, key, &G700, LINUX);

    let at = |button: ButtonId| &intents[usize::from(G700.slot_of(button).unwrap())];
    assert_eq!(
        at(ButtonId::G6),
        &SlotIntent::Native(FlashBinding::Key {
            modifiers: 0x01,
            usage: 0x06
        })
    );
    assert_eq!(at(ButtonId::G7), &SlotIntent::Agent, "Copy is the host's");
    assert_eq!(at(ButtonId::G8), &SlotIntent::Agent, "an app overrides it");
    assert_eq!(
        at(ButtonId::G9),
        &SlotIntent::Agent,
        "a long press is timed"
    );
    assert_eq!(
        at(ButtonId::G10),
        &SlotIntent::Native(FlashBinding::Function(FlashFunction::BatteryLevel)),
        "only the mouse runs it, so the app override is not applied"
    );
    assert_eq!(
        at(ButtonId::G11),
        &SlotIntent::Keep,
        "nothing stored: the override has nothing to fall back to"
    );
    assert_eq!(
        at(ButtonId::Back),
        &SlotIntent::Native(FlashBinding::MouseButtons(0x0008))
    );
    assert_eq!(at(ButtonId::LeftClick), &SlotIntent::Keep);
    assert_eq!(intents.len(), G700.slots.len());
    assert!(
        flash_intents(&config, "unknown", &G700, LINUX)
            .iter()
            .all(|intent| *intent == SlotIntent::Keep)
    );
}

#[test]
fn the_g700_layout_matches_its_profile_binding_table() {
    assert_eq!(G700.slots.len(), 13);
    assert_eq!(G700.flash_defaults.len(), G700.slots.len());
    assert_eq!(G700.slot_of(ButtonId::Back), Some(3));
    assert_eq!(G700.slot_of(ButtonId::G11), Some(10));
    assert_eq!(G700.slot_of(ButtonId::WheelTiltRight), Some(12));
    let default_action = |button| {
        G700.flash_default(button)
            .and_then(|binding| binding.action(ModifierHost::Other))
    };
    assert_eq!(
        default_action(ButtonId::G6),
        Some(Action::MouseButton(button(6)))
    );
    assert_eq!(default_action(ButtonId::G8), Some(Action::ShowBatteryLevel));
    assert_eq!(
        default_action(ButtonId::G11),
        Some(Action::CycleOnboardProfiles)
    );
    assert_eq!(
        default_action(ButtonId::WheelTiltLeft),
        Some(Action::HorizontalScrollLeft)
    );
    assert!(
        G700.flash_defaults
            .iter()
            .all(|binding| binding.action(ModifierHost::Other).is_some()),
        "every default is an action the picker can show"
    );
}

#[test]
fn the_flash_capability_names_the_g700_layout() {
    let g700 = Capabilities {
        pointer: true,
        report_rate: true,
        flash_bindings: true,
        ..Capabilities::default()
    };
    let verdict = GamingButtons::for_device("wpid1023", Some(&g700));
    assert_eq!(verdict, Some(GamingButtons::Flash(&G700)));
    assert_eq!(verdict.and_then(GamingButtons::flash_layout), Some(&G700));
    assert_eq!(
        GamingButtons::for_device("wpid1023", Some(&Capabilities::default())),
        None
    );
    assert_eq!(
        GamingButtons::for_device("0407f", Some(&g700)).and_then(GamingButtons::flash_layout),
        None,
        "a dumped model keeps its own layout"
    );
}

#[test]
fn the_state_answers_per_slot() {
    let state = FlashBindingsState {
        profile: Some(2),
        slots: dumped_profile(),
        routed: BTreeMap::from([(button(16), ButtonId::G6)]),
        ..FlashBindingsState::default()
    };
    assert_eq!(
        state.slot(&G700, ButtonId::G10),
        Some(&FlashBinding::MouseButtons(button(10).mask()))
    );
    assert!(state.routed_to_agent(ButtonId::G6));
    assert!(!state.routed_to_agent(ButtonId::G7));
    assert_eq!(state.slot(&G700, ButtonId::DpiToggle), None);
}

#[test]
fn only_g700_input_is_flash_input() {
    assert!(is_flash_bindings_input(0x046d, 0xc531));
    assert!(!is_flash_bindings_input(0x046d, 0xc548));
}

#[test]
fn a_numbered_button_round_trips_through_its_config_name() {
    let numbered = ButtonId::Numbered(button(12));
    assert_eq!(numbered.config_name(), "Button12");
    assert_eq!("Button12".parse::<ButtonId>(), Ok(numbered));
    "Button17".parse::<ButtonId>().expect_err("past sixteen");
    "Button0".parse::<ButtonId>().expect_err("before one");
    assert_eq!(numbered.to_string(), "Button 12");
    assert!(!numbered.is_os_hook_button(), "the hook passes it through");
}

#[test]
fn a_primary_button_is_never_handed_to_the_agent() {
    let key = "receiver:C531:slot:1";
    let mut config = Config::default();
    config.set_binding(key, ButtonId::LeftClick, Binding::Single(Action::Copy));
    config.set_binding(
        key,
        ButtonId::RightClick,
        Binding::Single(Action::MiddleClick),
    );
    config.set_per_app_binding(
        key,
        "org.mozilla.firefox",
        ButtonId::RightClick,
        Some(Action::Copy),
    );

    let intents = flash_intents(&config, key, &G700, LINUX);

    assert_eq!(
        intents[0],
        SlotIntent::Refused(RefusedSlot::PrimaryButton),
        "a host action on the left button is refused"
    );
    assert_eq!(
        intents[1],
        SlotIntent::Native(FlashBinding::MouseButtons(0x0004)),
        "a native action stays native even with an app override"
    );
    // A profile whose left slot presses an agent button (a hand-edited
    // configuration of an earlier build) gets its left click back.
    let mut held = dumped_profile();
    held[0] = FlashBinding::MouseButtons(button(16).mask());
    let plan = lay_out(&G700, &held, &BTreeSet::new(), &intents);
    assert_eq!(plan.bindings[0], G700.flash_defaults[0]);
    assert!(plan.routed.is_empty());
}

#[test]
fn a_hook_that_cannot_see_agent_buttons_leaves_agent_slots_as_they_are() {
    let key = "receiver:C531:slot:1";
    let mut config = Config::default();
    config.set_binding(key, ButtonId::G7, Binding::Single(Action::Copy));
    config.set_binding(key, ButtonId::G6, Binding::Single(Action::MiddleClick));
    let windows = FlashHost {
        modifiers: ModifierHost::Other,
        hook: AgentButtonHook::Blind,
    };

    let intents = flash_intents(&config, key, &G700, windows);

    let g7 = usize::from(G700.slot_of(ButtonId::G7).unwrap());
    let g6 = usize::from(G700.slot_of(ButtonId::G6).unwrap());
    assert_eq!(intents[g7], SlotIntent::Refused(RefusedSlot::HookBlind));
    assert_eq!(
        intents[g6],
        SlotIntent::Native(FlashBinding::MouseButtons(0x0004)),
        "native actions still go to the flash"
    );
    let held = dumped_profile();
    let plan = lay_out(&G700, &held, &BTreeSet::new(), &intents);
    assert_eq!(plan.bindings[g7], held[g7], "the slot keeps its binding");
    assert!(plan.routed.is_empty() && plan.unrouted.is_empty());
    assert_eq!(
        flash_intents(&config, key, &G700, LINUX)[g7],
        SlotIntent::Agent
    );
}

#[test]
fn a_cleared_slot_goes_back_to_its_default_only_on_the_button_the_agent_gave_it() {
    let g7 = usize::from(G700.slot_of(ButtonId::G7).unwrap());
    let g8 = usize::from(G700.slot_of(ButtonId::G8).unwrap());
    let g9 = usize::from(G700.slot_of(ButtonId::G9).unwrap());
    let mut held = dumped_profile();
    held[g7] = FlashBinding::MouseButtons(button(16).mask());
    held[g8] = FlashBinding::MouseButtons(button(15).mask());
    // Another tool made G9 press 14 on purpose.
    held[g9] = FlashBinding::MouseButtons(button(14).mask());
    let mut wanted = vec![SlotIntent::Keep; G700.slots.len()];
    wanted[g8] = SlotIntent::Agent;
    let request = ProfileRequest {
        intents: wanted,
        owned: BTreeMap::from([(button(16), ButtonId::G7), (button(15), ButtonId::G8)]),
        sync: FlashSync::Write,
    };

    let plan = plan_flash_bindings(&G700, &held, &BTreeSet::new(), &request);

    assert_eq!(plan.bindings[g7], G700.flash_defaults[g7], "released");
    assert_eq!(plan.bindings[g8], held[g8], "still configured, keeps 15");
    assert_eq!(plan.bindings[g9], held[g9], "not the agent's: kept");
    assert_eq!(plan.routed, BTreeMap::from([(button(15), ButtonId::G8)]));

    let unowned = ProfileRequest {
        owned: BTreeMap::new(),
        ..request
    };
    let plan = plan_flash_bindings(&G700, &held, &BTreeSet::new(), &unowned);
    assert_eq!(
        plan.bindings[g7], held[g7],
        "without a record nothing is released"
    );
}

#[test]
fn an_observed_profile_keeps_its_flash_and_routes_what_it_holds() {
    let g6 = usize::from(G700.slot_of(ButtonId::G6).unwrap());
    let g7 = usize::from(G700.slot_of(ButtonId::G7).unwrap());
    let mut held = dumped_profile();
    held[g6] = FlashBinding::MouseButtons(button(16).mask());
    let mut wanted = vec![SlotIntent::Keep; G700.slots.len()];
    wanted[g6] = SlotIntent::Agent;
    wanted[g7] = SlotIntent::Agent;
    wanted[0] = SlotIntent::Native(FlashBinding::MouseButtons(0x0002));
    let request = ProfileRequest {
        intents: wanted,
        owned: BTreeMap::new(),
        sync: FlashSync::Observe,
    };

    let plan = plan_flash_bindings(&G700, &held, &BTreeSet::from([button(16)]), &request);

    assert_eq!(plan.bindings, held, "nothing changes");
    assert_eq!(plan.routed, BTreeMap::from([(button(16), ButtonId::G6)]));
    assert!(plan.unrouted.is_empty());
}

#[test]
fn an_owned_button_stays_with_its_profile_where_a_copy_presses_it_too() {
    let g6 = usize::from(G700.slot_of(ButtonId::G6).unwrap());
    let mut held = dumped_profile();
    held[g6] = FlashBinding::MouseButtons(button(16).mask());
    let mut wanted = vec![SlotIntent::Keep; G700.slots.len()];
    wanted[g6] = SlotIntent::Agent;
    // The copy on another page presses 16 as well.
    let reserved = BTreeSet::from([button(16)]);
    let owner = ProfileRequest {
        intents: wanted.clone(),
        owned: BTreeMap::from([(button(16), ButtonId::G6)]),
        sync: FlashSync::Write,
    };
    let plan = plan_flash_bindings(&G700, &held, &reserved, &owner);
    assert_eq!(plan.bindings[g6], held[g6]);
    assert_eq!(plan.routed, BTreeMap::from([(button(16), ButtonId::G6)]));

    let copy = ProfileRequest::write(wanted);
    let plan = plan_flash_bindings(&G700, &held, &reserved, &copy);
    assert_eq!(
        plan.routed,
        BTreeMap::from([(button(15), ButtonId::G6)]),
        "the copy moves"
    );
}
