//! Exercise the real inspector's checkmarks and scoped action callbacks.

use super::*;

const APP: &str = "com.example.Editor";

fn inspector(cx: &mut gpui::TestAppContext) -> (&mut gpui::VisualTestContext, tempfile::TempDir) {
    cx.update(gpui_component::init);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let (mut config, mut file) = ConfigFile::load_from_path(&path).unwrap();
    config.set_binding(KEY, CALCULATOR, Binding::Single(Action::Copy));
    file.save(&config).unwrap();
    let (mut state, _) = keyboard_state(config, ConfigPersistence::UserFile(file));
    let _ = state.set_editing_app(Some(APP.into()));
    cx.update(|cx| {
        let state = cx.new(|_| state);
        AppState::set_global(state, cx);
    });
    let (_, cx) = cx.add_window_view(|_, cx| CalculatorInspector {
        view: cx.new(FunctionRowView::new),
    });
    redraw(cx);
    (cx, dir)
}

fn redraw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector).expect("action row is rendered");
    cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    redraw(cx);
}

fn assert_copy_selection(cx: &mut gpui::VisualTestContext, inherited: bool) {
    assert_eq!(
        cx.debug_bounds("key-panel-native-check").is_some(),
        inherited
    );
    assert_eq!(
        cx.debug_bounds("panel-action-Copy-check").is_some(),
        !inherited
    );
    cx.read(|cx| {
        let state = AppState::try_read(cx).unwrap();
        assert_eq!(
            bound_action(state, &KeyTarget::Control(CALCULATOR)),
            Some(Action::Copy)
        );
        assert_eq!(
            state
                .editing_app_overrides()
                .and_then(|rows| rows.get(&CALCULATOR)),
            (!inherited).then_some(&Action::Copy),
        );
    });
}

#[gpui::test]
fn keyboard_native_inherited_copy_has_only_the_default_checkmark(cx: &mut gpui::TestAppContext) {
    let (cx, _dir) = inspector(cx);
    assert_copy_selection(cx, true);
}

#[gpui::test]
fn keyboard_native_inherited_copy_can_be_explicitly_selected_and_cleared(
    cx: &mut gpui::TestAppContext,
) {
    let (cx, dir) = inspector(cx);
    click(cx, "panel-action-Copy");
    assert_copy_selection(cx, false);
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    assert_eq!(
        Config::load_from_path(&path)
            .unwrap()
            .per_app_overrides(KEY, APP)
            .unwrap()[&CALCULATOR],
        Action::Copy
    );
    click(cx, "key-panel-native");
    assert_copy_selection(cx, true);
    assert!(
        Config::load_from_path(&path)
            .unwrap()
            .per_app_overrides(KEY, APP)
            .is_none()
    );
    click(cx, "panel-action-Copy");
    assert_copy_selection(cx, false);
    click(cx, "panel-action-Copy");
    assert_copy_selection(cx, true);
    let saved = Config::load_from_path(&path).unwrap();
    assert!(saved.per_app_overrides(KEY, APP).is_none());
    assert_eq!(
        saved.stored_bindings(KEY)[&CALCULATOR],
        Binding::Single(Action::Copy)
    );
}

#[gpui::test]
fn keyboard_native_device_default_copy_is_selected_and_clears(cx: &mut gpui::TestAppContext) {
    let (cx, _dir) = inspector(cx);
    cx.update(|_, cx| AppState::apply(cx, |state| state.set_editing_app(None)));
    redraw(cx);
    assert!(cx.debug_bounds("panel-action-Copy-check").is_some());
    assert!(cx.debug_bounds("key-panel-native-check").is_none());
    click(cx, "panel-action-Copy");
    assert!(cx.debug_bounds("panel-action-Copy-check").is_none());
    assert!(cx.debug_bounds("key-panel-native-check").is_some());
    cx.read(|cx| {
        assert_eq!(
            bound_action(
                AppState::try_read(cx).unwrap(),
                &KeyTarget::Control(CALCULATOR)
            ),
            None
        );
    });
}

#[test]
fn keyboard_native_inspector_distinguishes_inherited_and_explicit_actions() {
    for action in [
        Action::Copy,
        Action::None,
        Action::TypeText("example".into()),
    ] {
        let mut config = Config::ephemeral();
        config.set_binding(KEY, CALCULATOR, Binding::Single(action.clone()));
        let (mut state, _) = keyboard_state(config, ConfigPersistence::MemoryOnly);
        let target = KeyTarget::Control(CALCULATOR);
        assert_eq!(selected_key_action(&state, &target), Some(action.clone()));
        let _ = state.set_editing_app(Some(APP.into()));
        assert_eq!(bound_action(&state, &target), Some(action.clone()));
        assert_eq!(selected_key_action(&state, &target), None);
        let _ = commit_key_action(&mut state, &target, Some(action.clone()));
        assert_eq!(selected_key_action(&state, &target), Some(action.clone()));
        let _ = commit_key_action(&mut state, &target, None);
        assert_eq!(selected_key_action(&state, &target), None);
        assert_eq!(bound_action(&state, &target), Some(action));
    }
}
