use gpui::{AppContext as _, Modifiers, TestAppContext, VisualTestContext, px, size};
use openlogi_core::config::{Config, ConfigFile};

use super::*;
use crate::{
    services::{assets::AssetResolver, i18n::LOCALE_LOCK},
    state::{ConfigPersistence, Sources},
    windows::settings::{SettingsPage, SettingsView},
};

#[gpui::test]
fn recovery_requires_preview_and_confirmation_and_can_be_cancelled(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let mut config = Config::default();
    config.app_settings.launch_at_login = false;
    std::fs::write(
        &path,
        format!(
            "schema_version = {}\n[app_settings]\nlaunch_at_login = false\n",
            openlogi_core::config::SCHEMA_VERSION
        ),
    )
    .unwrap();
    let (_, mut file) = ConfigFile::load_from_path(&path).unwrap();
    config.app_settings.launch_at_login = true;
    file.save(&config).unwrap();
    Config::load_from_path(&path).unwrap();
    assert_eq!(recovery_backups(&path).unwrap().len(), 1);
    let original = std::fs::read(&path).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::theme::register_builtin_themes(cx);
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let state = cx.new(|_| {
            let resolver = AssetResolver::new();
            let mut sources = Sources::in_memory(config, &resolver, commands);
            sources.persistence = ConfigPersistence::UserFile(file);
            AppState::new(sources)
        });
        AppState::set_global(state, cx);
    });
    let mut main_view = None;
    let main_handle = cx.open_window(size(px(920.), px(720.)), |window, cx| {
        let view = cx.new(|cx| crate::app::AppView::new(window, cx));
        main_view = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let mut main_visual = VisualTestContext::from_window(main_handle.into(), cx);
    draw(&mut main_visual);
    let main_notified = std::rc::Rc::new(std::cell::Cell::new(false));
    let _observer = cx.update(|cx| {
        let notified = main_notified.clone();
        cx.observe(&main_view.unwrap(), move |_, _| notified.set(true))
    });
    let handle = cx.open_window(size(px(920.), px(720.)), |window, cx| {
        let settings = cx.new(|cx| SettingsView::new(SettingsPage::Recovery, window, cx));
        gpui_component::Root::new(settings, window, cx)
    });
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    draw(&mut visual);
    click(&mut visual, "recovery-backup-0");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "preview must be read-only"
    );
    click(&mut visual, "recovery-advance");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "confirmation must not implicitly accept"
    );
    click(&mut visual, "recovery-cancel");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    click(&mut visual, "recovery-backup-0");
    click(&mut visual, "recovery-advance");
    click(&mut visual, "recovery-advance");
    cx.run_until_parked();
    assert!(
        main_notified.get(),
        "recovery must invalidate the already-open main window"
    );
    draw(&mut main_visual);
    assert!(main_visual.debug_bounds("recovery-complete").is_some());
    assert!(
        !Config::load_from_path(&path)
            .unwrap()
            .app_settings
            .launch_at_login
    );
    assert!(visual.debug_bounds("recovery-complete").is_some());
    assert!(cx.read(|cx| AppState::global(cx).read(cx).config_restored()));
    assert_eq!(
        std::fs::read(recovery_backups(&path).unwrap().first().unwrap()).unwrap(),
        original
    );
    visual.dispatch_action(crate::app::menu::CloseWindow);
    cx.run_until_parked();
    assert_eq!(
        cx.windows().len(),
        1,
        "restoring must preserve the window's close action"
    );
}

#[gpui::test]
fn recovery_writer_finishes_during_shutdown_after_last_window_closes(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
    let source = dir.path().join("restore.toml");
    Config::default().save_to_path(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    let (config, file) = ConfigFile::load_from_path(&path).unwrap();
    let mut candidate = config.clone();
    candidate.app_settings.launch_at_login = false;
    candidate.save_to_path(&source).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::theme::register_builtin_themes(cx);
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let state = cx.new(|_| {
            let resolver = AssetResolver::new();
            let mut sources = Sources::in_memory(config, &resolver, commands);
            sources.persistence = ConfigPersistence::UserFile(file);
            AppState::new(sources)
        });
        AppState::set_global(state, cx);
    });
    let view = cx.new(RecoveryView::new);
    let handle = cx.open_window(size(px(920.), px(720.)), |window, cx| {
        gpui_component::Root::new(view.clone(), window, cx)
    });
    cx.run_until_parked();
    view.update(cx, |view, cx| view.preview(&source, cx));
    cx.run_until_parked();
    view.update(cx, RecoveryView::continue_to_confirmation);
    view.update(cx, RecoveryView::restore);
    assert!(cx.read(|cx| AppState::global(cx).read(cx).config_recovering()));
    assert_eq!(std::fs::read(&path).unwrap(), original);

    // TestPlatform::quit is a no-op, so close the real last window and invoke
    // the same App::shutdown path that the native platform runs on quit.
    // Do not drain the executor: shutdown itself must finish the queued writer.
    cx.update_window(handle.into(), |_, window, _| window.remove_window())
        .unwrap();
    drop(view);
    assert!(cx.windows().is_empty());
    cx.quit();
    assert!(
        !Config::load_from_path(&path)
            .unwrap()
            .app_settings
            .launch_at_login,
        "application shutdown returned before the confirmed restore finished"
    );
    assert_eq!(
        std::fs::read(recovery_backups(&path).unwrap().first().unwrap()).unwrap(),
        original,
        "shutdown must wait for the recovery copy as well as replacement"
    );
    cx.run_until_parked();
    assert!(cx.read(|cx| AppState::global(cx).read(cx).config_restored()));
}

fn draw(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing recovery control: {selector}"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    draw(visual);
}

#[gpui::test]
fn recovery_error_details_are_optional_and_reset_for_a_new_preview(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
    let config = Config::default();
    std::fs::write(
        &path,
        format!(
            "schema_version = {}\n[app_settings]\nlaunch_at_login = false\n",
            openlogi_core::config::SCHEMA_VERSION
        ),
    )
    .unwrap();
    let (_, file) = ConfigFile::load_from_path(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::theme::register_builtin_themes(cx);
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let state = cx.new(|_| {
            let resolver = AssetResolver::new();
            let mut sources = Sources::in_memory(config, &resolver, commands);
            sources.persistence = ConfigPersistence::UserFile(file);
            AppState::new(sources)
        });
        AppState::set_global(state, cx);
    });
    let mut view = None;
    let handle = cx.open_window(size(px(920.), px(720.)), |window, cx| {
        let recovery = cx.new(RecoveryView::new);
        view = Some(recovery.clone());
        gpui_component::Root::new(recovery, window, cx)
    });
    let view = view.unwrap();
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    let missing = directory.path().join("missing.toml");
    view.update(cx, |view, cx| view.preview(&missing, cx));
    cx.run_until_parked();
    draw(&mut visual);
    assert!(visual.debug_bounds("recovery-details-toggle").is_some());
    assert!(visual.debug_bounds("recovery-error-details").is_none());
    click(&mut visual, "recovery-details-toggle");
    assert!(visual.debug_bounds("recovery-error-details").is_some());
    click(&mut visual, "recovery-details-toggle");
    assert!(visual.debug_bounds("recovery-error-details").is_none());
    click(&mut visual, "recovery-details-toggle");
    view.update(cx, |view, cx| view.preview(&path, cx));
    cx.run_until_parked();
    draw(&mut visual);
    assert!(visual.debug_bounds("recovery-details-toggle").is_none());
    view.update(cx, |view, cx| view.preview(&missing, cx));
    cx.run_until_parked();
    draw(&mut visual);
    assert!(visual.debug_bounds("recovery-error-details").is_none());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let backup = directory.path().join("valid-backup.toml");
    std::fs::write(&backup, &original).unwrap();
    std::fs::write(&path, "[broken").unwrap();
    view.update(cx, |view, cx| view.preview(&backup, cx));
    cx.run_until_parked();
    draw(&mut visual);
    assert!(visual.debug_bounds("recovery-details-toggle").is_some());
    assert!(visual.debug_bounds("recovery-error-details").is_none());
    click(&mut visual, "recovery-details-toggle");
    assert!(visual.debug_bounds("recovery-error-details").is_some());
    assert_eq!(std::fs::read(&path).unwrap(), b"[broken");
}

#[gpui::test]
fn recovery_pagination_and_confirmation_are_reachable_at_normal_window_height(
    cx: &mut TestAppContext,
) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
    let source = directory.path().join("backup.toml");
    let mut config = Config::default();
    for index in 0..10 {
        config
            .devices
            .entry(format!("unit:{index:08x}"))
            .or_default();
    }
    config.save_to_path(&path).unwrap();
    let (current, file) = ConfigFile::load_from_path(&path).unwrap();
    for device in config.devices.values_mut() {
        device.enabled = false;
    }
    config.save_to_path(&source).unwrap();
    let original = std::fs::read(&path).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::theme::register_builtin_themes(cx);
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let state = cx.new(|_| {
            let resolver = AssetResolver::new();
            let mut sources = Sources::in_memory(current, &resolver, commands);
            sources.persistence = ConfigPersistence::UserFile(file);
            AppState::new(sources)
        });
        AppState::set_global(state, cx);
    });
    let mut view = None;
    let handle = cx.open_window(size(px(920.), px(672.)), |window, cx| {
        let settings = cx.new(|cx| SettingsView::new(SettingsPage::Recovery, window, cx));
        view = Some(settings.read(cx).recovery.clone());
        gpui_component::Root::new(settings, window, cx)
    });
    let view = view.unwrap();
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    view.update(&mut visual, |view, cx| view.preview(&source, cx));
    draw(&mut visual);
    assert!(visual.debug_bounds("recovery-change-8").is_none());
    scroll_recovery_to_bottom(&mut visual);
    let pages = visual.debug_bounds("recovery-change-pages").unwrap();
    assert!(pages.center().y > px(60.) && pages.center().y < px(672.));
    visual.simulate_click(
        gpui::point(pages.right() - px(20.), pages.center().y),
        Modifiers::default(),
    );
    draw(&mut visual);
    assert!(visual.debug_bounds("recovery-change-8").is_some());
    scroll_recovery_to_bottom(&mut visual);
    let advance = visual.debug_bounds("recovery-advance").unwrap();
    assert!(advance.center().y > px(60.) && advance.center().y < px(672.));
    click(&mut visual, "recovery-advance");
    scroll_recovery_to_bottom(&mut visual);
    let confirm = visual.debug_bounds("recovery-advance").unwrap();
    assert!(confirm.center().y > px(60.) && confirm.center().y < px(672.));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    click(&mut visual, "recovery-advance");
    assert!(visual.debug_bounds("recovery-complete").is_some());
    assert!(
        Config::load_from_path(&path)
            .unwrap()
            .devices
            .values()
            .all(|device| !device.enabled)
    );
}

fn scroll_recovery_to_bottom(visual: &mut VisualTestContext) {
    visual.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                position: gpui::point(px(700.), px(500.)),
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-5000.))),
                ..Default::default()
            }),
            cx,
        );
    });
    draw(visual);
}

#[gpui::test]
fn recovery_keyboard_focus_reveals_actions_and_confirmation(cx: &mut TestAppContext) {
    let _locale = LOCALE_LOCK.lock().unwrap();
    rust_i18n::set_locale("en");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(openlogi_core::paths::CONFIG_FILE);
    let source = directory.path().join("backup.toml");
    let mut config = Config::default();
    for index in 0..3 {
        config
            .devices
            .entry(format!("unit:{index:08x}"))
            .or_default();
    }
    config.save_to_path(&path).unwrap();
    let (current, file) = ConfigFile::load_from_path(&path).unwrap();
    for device in config.devices.values_mut() {
        device.enabled = false;
    }
    config.save_to_path(&source).unwrap();
    let original = std::fs::read(&path).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::ui::theme::register_builtin_themes(cx);
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let state = cx.new(|_| {
            let resolver = AssetResolver::new();
            let mut sources = Sources::in_memory(current, &resolver, commands);
            sources.persistence = ConfigPersistence::UserFile(file);
            AppState::new(sources)
        });
        AppState::set_global(state, cx);
    });
    let mut view = None;
    let handle = cx.open_window(size(px(920.), px(672.)), |window, cx| {
        let settings = cx.new(|cx| SettingsView::new(SettingsPage::Recovery, window, cx));
        view = Some(settings.read(cx).recovery.clone());
        gpui_component::Root::new(settings, window, cx)
    });
    let view = view.unwrap();
    let mut visual = VisualTestContext::from_window(handle.into(), cx);
    view.update(&mut visual, |view, cx| view.preview(&source, cx));
    draw(&mut visual);
    let before = visual.debug_bounds("recovery-advance").unwrap();
    assert!(
        before.center().y > px(672.),
        "fixture must overflow the window"
    );
    visual.update(|window, cx| {
        window.blur(cx);
        window.focus_next(cx);
    });
    // Search, Appearance, technical details, Cancel, Continue.
    for _ in 0..4 {
        visual.simulate_keystrokes("tab");
        draw(&mut visual);
    }
    let advance = visual.debug_bounds("recovery-advance").unwrap();
    assert!(advance.center().y > px(60.) && advance.bottom() <= px(672.));
    let keystroke = gpui::Keystroke::parse("enter").unwrap();
    visual.simulate_event(gpui::KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    visual.simulate_event(gpui::KeyUpEvent { keystroke });
    draw(&mut visual);
    assert!(view.read_with(&visual, |view, _| matches!(
        view.stage,
        RecoveryStage::Confirm(_)
    )));
    let confirm = visual.debug_bounds("recovery-advance").unwrap();
    assert!(confirm.center().y > px(60.) && confirm.bottom() <= px(672.));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    click(&mut visual, "recovery-cancel");
    assert_eq!(std::fs::read(&path).unwrap(), original);
}
