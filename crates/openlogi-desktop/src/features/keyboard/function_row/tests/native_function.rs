//! The Keys panel must not turn removing an assignment into suppression.

use super::*;
use crate::services::assets::AssetResolver;
use crate::services::ipc::Command;
use crate::state::{ConfigPersistence, DeviceKey, Sources, StateEvent};
use openlogi_core::binding::{Binding, LongPressBinding};
use openlogi_core::config::{Config, ConfigFile};

const KEY: &str = "unit:4f4c4402";
const CALCULATOR: ButtonId = ButtonId::control(0x000a);

mod selection;

/// Render the real selected-key inspector without image/depot downloads.
struct CalculatorInspector {
    view: Entity<FunctionRowView>,
}

impl Render for CalculatorInspector {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.view.update(cx, |view, cx| {
            view.config_panel(
                0,
                &[KeySlot {
                    idx: 0,
                    label: "Calculator".into(),
                    target: KeyTarget::Control(CALCULATOR),
                    x_frac: 0.5,
                    y_frac: 0.1,
                    binding: "Copy".into(),
                    binding_icon: None,
                    lcd_icon: None,
                }],
                &self.view,
                cx,
            )
        })
    }
}

#[gpui::test]
fn keyboard_native_row_click_removes_the_persisted_binding(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let (mut config, mut file) = ConfigFile::load_from_path(&path).unwrap();
    config.set_binding(KEY, CALCULATOR, Binding::Single(Action::Copy));
    file.save(&config).unwrap();
    let (state, mut receiver) = keyboard_state(config, ConfigPersistence::UserFile(file));
    cx.update(|cx| {
        let state = cx.new(|_| state);
        AppState::set_global(state, cx);
    });
    let (_, cx) = cx.add_window_view(|_, cx| CalculatorInspector {
        view: cx.new(FunctionRowView::new),
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let row = cx
        .debug_bounds("key-panel-native")
        .expect("native row is rendered");

    cx.simulate_click(row.center(), gpui::Modifiers::default());
    cx.update(|window, cx| window.draw(cx).clear(cx));

    assert!(
        !Config::load_from_path(&path)
            .unwrap()
            .stored_bindings(KEY)
            .contains_key(&CALCULATOR)
    );
    cx.read(|cx| {
        let state = AppState::try_read(cx).unwrap();
        assert_eq!(bound_action(state, &KeyTarget::Control(CALCULATOR)), None);
        assert!(selected_key_action(state, &KeyTarget::Control(CALCULATOR)).is_none());
    });
    let commands: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
    assert!(matches!(commands.as_slice(), [Command::ReloadConfig(_)]));
}

fn keyboard_state(
    config: Config,
    persistence: ConfigPersistence,
) -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let resolver = AssetResolver::new();
    let (commands, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut inventory = crate::state::tests::direct_inventory([0x4f, 0x4c, 0x44, 0x02]);
    inventory.paired[0].kind = DeviceKind::Keyboard;
    inventory.paired[0].codename = Some("Test keyboard".into());
    let state = AppState::new(Sources {
        inventories: &[inventory],
        persistence,
        ..Sources::in_memory(config, &resolver, commands)
    });
    while receiver.try_recv().is_ok() {}
    (state, receiver)
}

#[test]
fn keyboard_native_removes_the_saved_control_and_reloads_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let (config, file) = ConfigFile::load_from_path(&path).unwrap();
    let (mut state, mut receiver) = keyboard_state(config, ConfigPersistence::UserFile(file));
    let target = KeyTarget::Control(CALCULATOR);
    let _ = commit_key_action(&mut state, &target, Some(Action::Copy));
    assert_eq!(bound_action(&state, &target), Some(Action::Copy));
    assert_eq!(
        Config::load_from_path(&path)
            .unwrap()
            .stored_bindings(KEY)
            .get(&CALCULATOR),
        Some(&Binding::Single(Action::Copy))
    );
    while receiver.try_recv().is_ok() {}

    let events = commit_key_action(&mut state, &target, None);

    assert!(
        !Config::load_from_path(&path)
            .unwrap()
            .stored_bindings(KEY)
            .contains_key(&CALCULATOR),
        "native means an absent assignment, not an explicit suppress action"
    );
    assert_eq!(bound_action(&state, &target), None);
    assert!(selected_key_action(&state, &target).is_none());
    assert!(!state.button_bindings().contains_key(&CALCULATOR));
    assert_eq!(events, [StateEvent::BindingsChanged(DeviceKey::from(KEY))]);
    let commands: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
    assert!(matches!(commands.as_slice(), [Command::ReloadConfig(_)]));
}

#[test]
fn keyboard_native_does_not_disguise_explicit_suppression() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let (config, file) = ConfigFile::load_from_path(&path).unwrap();
    let (mut state, _receiver) = keyboard_state(config, ConfigPersistence::UserFile(file));
    let target = KeyTarget::Control(CALCULATOR);

    let _ = commit_key_action(&mut state, &target, Some(Action::None));

    assert_eq!(bound_action(&state, &target), Some(Action::None));
    assert!(selected_key_action(&state, &target).is_some());
    assert_eq!(
        Config::load_from_path(&path)
            .unwrap()
            .stored_bindings(KEY)
            .get(&CALCULATOR),
        Some(&Binding::Single(Action::None))
    );
    // Repair a suppression entry from either an intentional choice or the old GUI.
    let _ = commit_key_action(&mut state, &target, None);
    assert!(selected_key_action(&state, &target).is_none());
    assert!(
        !Config::load_from_path(&path)
            .unwrap()
            .stored_bindings(KEY)
            .contains_key(&CALCULATOR)
    );
}

#[test]
fn keyboard_native_reselecting_the_current_action_clears_it() {
    let (mut state, mut receiver) =
        keyboard_state(Config::ephemeral(), ConfigPersistence::MemoryOnly);
    let target = KeyTarget::Control(CALCULATOR);
    for action in [Action::Copy, Action::None] {
        let _ = commit_key_action(&mut state, &target, Some(action.clone()));
        let picked = binding_after_pick(bound_action(&state, &target).as_ref(), action);
        assert_eq!(picked, None);
        let _ = commit_key_action(&mut state, &target, picked);
        assert!(!state.button_bindings().contains_key(&CALCULATOR));
        assert!(selected_key_action(&state, &target).is_none());
    }
    assert_eq!(std::iter::from_fn(|| receiver.try_recv().ok()).count(), 4);
}

#[test]
fn keyboard_native_removes_the_whole_compound_binding_but_not_other_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let (mut config, mut file) = ConfigFile::load_from_path(&path).unwrap();
    config.set_binding(
        KEY,
        CALCULATOR,
        Binding::LongPress(LongPressBinding::new(Action::Copy, Action::Paste)),
    );
    let other_key = ButtonId::control(0x006f);
    config.set_binding(KEY, other_key, Binding::Single(Action::MuteVolume));
    config.set_binding("other-device", CALCULATOR, Binding::Single(Action::Undo));
    config.set_per_app_binding(KEY, "com.example.Editor", CALCULATOR, Some(Action::Paste));
    config.set_fn_lock(KEY, true);
    file.save(&config).unwrap();
    let (mut state, _receiver) = keyboard_state(config, ConfigPersistence::UserFile(file));

    let _ = commit_key_action(&mut state, &KeyTarget::Control(CALCULATOR), None);

    let saved = Config::load_from_path(&path).unwrap();
    assert!(!saved.stored_bindings(KEY).contains_key(&CALCULATOR));
    assert_eq!(
        saved.stored_bindings(KEY).get(&other_key),
        Some(&Binding::Single(Action::MuteVolume))
    );
    assert_eq!(
        saved.stored_bindings("other-device").get(&CALCULATOR),
        Some(&Binding::Single(Action::Undo))
    );
    assert_eq!(
        saved.per_app_overrides(KEY, "com.example.Editor").unwrap()[&CALCULATOR],
        Action::Paste
    );
    assert_eq!(saved.fn_lock(KEY), Some(true));
}

#[test]
fn keyboard_native_in_app_scope_inherits_default_without_clearing_other_scopes() {
    for default in [Some(Action::Copy), Some(Action::None), None] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let (mut config, mut file) = ConfigFile::load_from_path(&path).unwrap();
        if let Some(action) = &default {
            config.set_binding(KEY, CALCULATOR, Binding::Single(action.clone()));
        }
        config.set_per_app_binding(KEY, "com.example.Editor", CALCULATOR, Some(Action::Paste));
        config.set_per_app_binding(KEY, "com.example.Browser", CALCULATOR, Some(Action::Undo));
        file.save(&config).unwrap();
        let (mut state, mut receiver) = keyboard_state(config, ConfigPersistence::UserFile(file));
        let _ = state.set_editing_app(Some("com.example.Editor".into()));
        let target = KeyTarget::Control(CALCULATOR);
        assert_eq!(bound_action(&state, &target), Some(Action::Paste));
        assert!(selected_key_action(&state, &target).is_some());

        let events = commit_key_action(&mut state, &target, None);

        assert_eq!(state.editing_app(), Some("com.example.Editor"));
        assert_eq!(bound_action(&state, &target), default);
        assert!(
            selected_key_action(&state, &target).is_none(),
            "inheritance is selected even when the default is assigned"
        );
        let saved = Config::load_from_path(&path).unwrap();
        assert_eq!(
            saved.stored_bindings(KEY).get(&CALCULATOR).cloned(),
            default.map(Binding::Single)
        );
        assert!(saved.per_app_overrides(KEY, "com.example.Editor").is_none());
        assert_eq!(
            saved.per_app_overrides(KEY, "com.example.Browser").unwrap()[&CALCULATOR],
            Action::Undo
        );
        assert_eq!(events, [StateEvent::BindingsChanged(DeviceKey::from(KEY))]);
        let commands: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert!(matches!(commands.as_slice(), [Command::ReloadConfig(_)]));
    }
}

#[test]
fn keyboard_native_failed_save_keeps_assignment_and_does_not_reload() {
    for app in [None, Some("com.example.Editor")] {
        let mut config = Config::ephemeral();
        config.set_binding(KEY, CALCULATOR, Binding::Single(Action::Copy));
        if let Some(app) = app {
            config.set_per_app_binding(KEY, app, CALCULATOR, Some(Action::Paste));
        }
        let (mut state, mut receiver) =
            keyboard_state(config, ConfigPersistence::ReadOnly("read-only".into()));
        let _ = state.set_editing_app(app.map(str::to_string));
        let target = KeyTarget::Control(CALCULATOR);

        let events = commit_key_action(&mut state, &target, None);

        assert_eq!(
            bound_action(&state, &target),
            Some(if app.is_some() {
                Action::Paste
            } else {
                Action::Copy
            })
        );
        assert!(selected_key_action(&state, &target).is_some());
        assert_eq!(state.editing_app(), app);
        assert_eq!(state.config_issue(), Some("read-only"));
        assert_eq!(events, [StateEvent::BindingsChanged(DeviceKey::from(KEY))]);
        assert!(
            receiver.try_recv().is_err(),
            "failed saves must not reload the agent"
        );
    }
}

#[test]
fn keyboard_native_f_key_clear_stays_global_even_with_an_app_profile_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let (config, file) = ConfigFile::load_from_path(&path).unwrap();
    let (mut state, _receiver) = keyboard_state(config, ConfigPersistence::UserFile(file));
    let trigger = "f1".parse().unwrap();
    let target = KeyTarget::FunctionKey(trigger);
    let _ = state.set_editing_app(Some("com.example.Editor".into()));
    let _ = commit_key_action(&mut state, &target, Some(Action::Copy));
    assert_eq!(bound_action(&state, &target), Some(Action::Copy));
    assert_eq!(selected_key_action(&state, &target), Some(Action::Copy));

    let _ = commit_key_action(&mut state, &target, None);

    assert!(selected_key_action(&state, &target).is_none());
    assert!(state.keyboard_bindings().is_empty());
    assert!(
        Config::load_from_path(&path)
            .unwrap()
            .keyboard_bindings()
            .is_empty()
    );
}
