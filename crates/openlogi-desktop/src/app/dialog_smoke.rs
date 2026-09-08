//! Native, hidden-window regression check for the main window's dialog host.
//! Run the debug binary with `OPENLOGI_DIALOG_SMOKE=1`; no agent or user config is used.

use std::{cell::Cell, rc::Rc};

use gpui::{
    AppContext as _, Keystroke, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement as _,
    PlatformInput, WindowOptions, point, px,
};
use gpui_component::{Root, WindowExt as _};
use openlogi_core::config::Config;

use crate::{
    services::assets::AssetResolver,
    state::{AppState, ConfigPersistence},
    ui::theme,
};

#[expect(
    clippy::expect_used,
    reason = "native regression failures must fail the diagnostic process"
)]
pub(crate) fn run() {
    gpui_platform::application()
        .with_assets(crate::app_assets::AppAssets)
        .run(|cx| {
            gpui_component::init(cx);
            theme::register_builtin_themes(cx);
            let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let state = cx.new(|_| {
                AppState::with_runtime(
                    Config::ephemeral(),
                    &[],
                    &[],
                    &AssetResolver::new(),
                    &[],
                    ConfigPersistence::MemoryOnly,
                    commands,
                )
            });
            AppState::set_global(state, cx);
            let handle = cx
                .open_window(
                    WindowOptions {
                        show: false,
                        focus: false,
                        ..WindowOptions::default()
                    },
                    |window, cx| {
                        let view = cx.new(|cx| super::AppView::new(&[], window, cx));
                        cx.new(|cx| Root::new(view, window, cx))
                    },
                )
                .expect("create a hidden native window");
            cx.update_window(handle.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                let rendered = Rc::new(Cell::new(false));
                let observed = rendered.clone();
                window.open_dialog(cx, move |dialog, _, _| {
                    observed.set(true);
                    dialog
                        .title("Dialog rendering regression")
                        .child("Visible dialog body")
                });
                assert!(window.has_active_dialog(cx), "dialog must be registered");
                window.refresh();
                window.draw(cx).clear(cx);
                assert!(
                    rendered.get(),
                    "registered dialog must be rendered by the actual AppView"
                );
                window.close_all_dialogs(cx);
                window.refresh();
                window.draw(cx).clear(cx);
                assert!(!window.has_active_dialog(cx));
                super::home::open_rename_dialog(
                    window, cx, "smoke-device".into(), String::new(), "Test Mouse".into(),
                );
                window.refresh();
                window.draw(cx).clear(cx);
                assert!(window.has_active_dialog(cx));
                assert!(window.has_focused_input(cx), "the real rename dialog must render its input");
                check_input_events(window, cx);
                window.close_all_dialogs(cx);
                window.refresh();
                window.draw(cx).clear(cx);
                super::home::open_delete_confirmation(window, cx, "smoke-device".into(), "Test Mouse".into());
                window.refresh();
                window.draw(cx).clear(cx);
                assert!(window.has_active_dialog(cx));
                window.close_all_dialogs(cx);
                eprintln!("PASS: actual AppView renders and closes a registered dialog");
                eprintln!("PASS: actual rename input renders and receives focus; delete confirmation opens");
                super::home::open_rename_dialog(window, cx, "smoke-device".into(), String::new(), "Test Mouse".into());
            })
            .expect("exercise main-window dialog rendering");
            cx.spawn(async move |cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
                cx.update_window(handle.into(), |_, window, cx| {
                    window.refresh();
                    window.draw(cx).clear(cx);
                    let input = window.focused_input(cx).expect("input after event-loop turn")
                        .as_input().expect("rename input").clone();
                    window.dispatch_keystroke(Keystroke::parse("b").expect("typing key"), cx);
                    window.refresh();
                    window.draw(cx).clear(cx);
                    assert_eq!(input.read(cx).value().as_str(), "b", "input must work after native event-loop processing");
                    eprintln!("PASS: rename input accepts typing after native event-loop processing");
                    cx.quit();
                }).expect("exercise delayed native input");
            }).detach();
        });
}

#[expect(
    clippy::expect_used,
    reason = "native regression failures must fail the diagnostic process"
)]
fn check_input_events(window: &mut gpui::Window, cx: &mut gpui::App) {
    let input = window
        .focused_input(cx)
        .expect("focused rename input")
        .as_input()
        .expect("single-line rename input")
        .clone();
    window.dispatch_keystroke(Keystroke::parse("a").expect("typing key"), cx);
    window.refresh();
    window.draw(cx).clear(cx);
    assert_eq!(
        input.read(cx).value().as_str(),
        "a",
        "typing must update the rename input"
    );
    window.dispatch_keystroke(Keystroke::parse("left").expect("cursor key"), cx);
    window.refresh();
    window.draw(cx).clear(cx);
    assert_eq!(
        input.read(cx).cursor(),
        0,
        "left arrow must move the rename cursor"
    );
    let bounds = input.read(cx).text_bounds().expect("rendered input bounds");
    let position = point(bounds.right() - px(2.), bounds.center().y);
    window.dispatch_event(
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            click_count: 1,
            ..Default::default()
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            click_count: 1,
            ..Default::default()
        }),
        cx,
    );
    window.refresh();
    window.draw(cx).clear(cx);
    assert_eq!(
        input.read(cx).cursor(),
        1,
        "mouse click must move the rename cursor"
    );
}
