use std::{cell::Cell, rc::Rc};

use gpui::{
    AppContext as _, Context, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render,
    TestAppContext, point,
};

use super::*;

struct ToggleHarness {
    selected: bool,
    disabled: bool,
    changes: Rc<Cell<Option<bool>>>,
    parent_clicks: Rc<Cell<usize>>,
}

impl Render for ToggleHarness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let changes = self.changes.clone();
        let parent_clicks = self.parent_clicks.clone();
        div()
            .id("toggle-parent")
            .tab_group()
            .size(px(100.))
            .on_click(move |_, _, _| parent_clicks.set(parent_clicks.get() + 1))
            .child(
                Toggle::new("keyboard-toggle")
                    .selected(self.selected)
                    .disabled(self.disabled)
                    .on_change(move |selected, _, _| changes.set(Some(*selected))),
            )
    }
}

struct MenuRowHarness {
    activations: Rc<Cell<usize>>,
}

impl Render for MenuRowHarness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let activations = self.activations.clone();
        div().tab_group().size(px(100.)).child(
            MenuRow::new("keyboard-menu-row")
                .role(Role::MenuItem)
                .child("Action")
                .on_click(move |_, _, _| activations.set(activations.get() + 1)),
        )
    }
}

struct PresetChipHarness {
    applications: Rc<Cell<usize>>,
    removals: Rc<Cell<usize>>,
}

impl Render for PresetChipHarness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let applications = self.applications.clone();
        let removals = self.removals.clone();
        div().tab_group().size(px(100.)).child(
            PresetChip::new("keyboard-preset-chip")
                .selected(true)
                .child(
                    BaseButton::new("keyboard-preset-apply")
                        .child("800")
                        .on_click(move |_, _, _| {
                            applications.set(applications.get() + 1);
                        }),
                )
                .child(
                    BaseButton::new("keyboard-preset-remove")
                        .child(Icon::new(IconName::Close).size_3())
                        .on_click(move |_, _, _| removals.set(removals.get() + 1)),
                ),
        )
    }
}

struct ProfileTabHarness {
    applications: Rc<Cell<usize>>,
    deletions: Rc<Cell<usize>>,
}

impl Render for ProfileTabHarness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let applications = self.applications.clone();
        let deletions = self.deletions.clone();
        div().tab_group().size(px(100.)).child(
            ProfileTab::new("keyboard-profile-tab", "Custom")
                .on_click(move |_, _, _| applications.set(applications.get() + 1))
                .on_delete("keyboard-profile-delete", move |_, _, _| {
                    deletions.set(deletions.get() + 1);
                }),
        )
    }
}

fn activate_key(cx: &mut gpui::VisualTestContext, key: &str) {
    let keystroke = Keystroke::parse(key).unwrap();
    cx.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(KeyUpEvent { keystroke });
}

#[gpui::test]
fn toggle_is_tab_focusable_and_reports_controlled_next_state(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let changes = Rc::new(Cell::new(None));
    let parent_clicks = Rc::new(Cell::new(0));
    let (view, cx) = cx.add_window_view({
        let changes = changes.clone();
        let parent_clicks = parent_clicks.clone();
        move |_, _| ToggleHarness {
            selected: false,
            disabled: false,
            changes,
            parent_clicks,
        }
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.update(Window::focus_next);
    cx.update(|window, cx| assert!(window.focused(cx).is_some()));

    activate_key(cx, "enter");
    assert_eq!(changes.get(), Some(true));

    view.update(cx, |view, cx| {
        view.selected = true;
        cx.notify();
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    activate_key(cx, "space");
    assert_eq!(changes.get(), Some(false));
}

#[gpui::test]
fn disabled_toggle_is_inert_and_blocks_its_parent(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let changes = Rc::new(Cell::new(None));
    let parent_clicks = Rc::new(Cell::new(0));
    let (_, cx) = cx.add_window_view({
        let changes = changes.clone();
        let parent_clicks = parent_clicks.clone();
        move |_, _| ToggleHarness {
            selected: false,
            disabled: true,
            changes,
            parent_clicks,
        }
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });

    cx.update(Window::focus_next);
    cx.update(|window, cx| assert!(window.focused(cx).is_none()));
    cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
    activate_key(cx, "enter");
    activate_key(cx, "space");

    assert_eq!(changes.get(), None);
    assert_eq!(parent_clicks.get(), 0);
}

#[gpui::test]
fn menu_row_is_tab_focusable_and_keyboard_activatable(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let activations = Rc::new(Cell::new(0));
    let (_, cx) = cx.add_window_view({
        let activations = activations.clone();
        move |_, _| MenuRowHarness { activations }
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.update(Window::focus_next);
    cx.update(|window, cx| assert!(window.focused(cx).is_some()));

    activate_key(cx, "enter");
    activate_key(cx, "space");

    assert_eq!(activations.get(), 2);
}

/// The guard is the point: `set_placeholder` notifies unconditionally, so
/// an unguarded per-render restamp would re-render forever. Same text must
/// not notify; a changed text must land.
#[gpui::test]
fn localize_placeholder_restamps_only_on_change(cx: &mut TestAppContext) {
    struct Blank;
    impl Render for Blank {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            gpui::Empty
        }
    }

    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|_, _| Blank);
    let input = cx.update(|window, cx| cx.new(|cx| InputState::new(window, cx).placeholder("old")));
    let notifies = Rc::new(Cell::new(0_usize));
    let _obs = cx.update(|_, cx| {
        cx.observe(&input, {
            let notifies = notifies.clone();
            move |_, _| notifies.set(notifies.get() + 1)
        })
    });

    cx.update(|window, cx| localize_placeholder(&input, "old".into(), window, cx));
    assert_eq!(notifies.get(), 0, "unchanged text must not notify");

    cx.update(|window, cx| localize_placeholder(&input, "new".into(), window, cx));
    assert_eq!(notifies.get(), 1);
    cx.update(|_, cx| {
        assert_eq!(*input.read(cx).presentation().placeholder(), "new");
    });
}

#[gpui::test]
fn profile_tab_and_delete_are_separate_keyboard_targets(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let applications = Rc::new(Cell::new(0));
    let deletions = Rc::new(Cell::new(0));
    let (_, cx) = cx.add_window_view({
        let applications = applications.clone();
        let deletions = deletions.clone();
        move |_, _| ProfileTabHarness {
            applications,
            deletions,
        }
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });

    cx.update(Window::focus_next);
    activate_key(cx, "enter");
    activate_key(cx, "space");
    assert_eq!(applications.get(), 2);
    assert_eq!(deletions.get(), 0);

    cx.update(Window::focus_next);
    activate_key(cx, "enter");
    activate_key(cx, "space");
    assert_eq!(applications.get(), 2);
    assert_eq!(deletions.get(), 2);
}

#[gpui::test]
fn preset_chip_children_are_separate_keyboard_targets(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let applications = Rc::new(Cell::new(0));
    let removals = Rc::new(Cell::new(0));
    let (_, cx) = cx.add_window_view({
        let applications = applications.clone();
        let removals = removals.clone();
        move |_, _| PresetChipHarness {
            applications,
            removals,
        }
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });

    cx.update(Window::focus_next);
    activate_key(cx, "enter");
    activate_key(cx, "space");
    assert_eq!(applications.get(), 2);
    assert_eq!(removals.get(), 0);

    cx.update(Window::focus_next);
    activate_key(cx, "enter");
    activate_key(cx, "space");
    assert_eq!(applications.get(), 2);
    assert_eq!(removals.get(), 2);
}
