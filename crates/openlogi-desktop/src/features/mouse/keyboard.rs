//! Shared keyboard-chord editor for tap, hold, and double-click bindings.

use std::collections::BTreeSet;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, Keystroke,
    Modifiers, ParentElement, Render, SharedString, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, IndexPath, WindowExt as _,
    button::ButtonVariants as _,
    checkbox::Checkbox,
    dialog::{Cancel, Confirm, DialogFooter},
    h_flex,
    kbd::Kbd,
    select::{SelectEvent, SelectItem, SelectState},
    v_flex,
};
use openlogi_core::binding::{Action, KeyCombo, KeyComboParseError, KeyboardUsage};

use super::picker::{ActionActivation, PickFn};
use crate::ui::components::{control_button, control_select};
use crate::ui::theme::{self, Typography as _};

type SaveKeys = Rc<dyn Fn(KeyCombo, &mut Window, &mut App)>;

#[derive(Clone, Copy)]
enum KeyboardActivation {
    Tap,
    Hold,
}

impl KeyboardActivation {
    fn action(self, keys: KeyCombo) -> Action {
        match self {
            Self::Tap => Action::CustomShortcut(keys),
            Self::Hold => Action::HoldShortcut(keys),
        }
    }
}

pub(super) fn keyboard_actions(
    id: &'static str,
    activation: ActionActivation,
    current: Option<&Action>,
    on_pick: &PickFn,
) -> impl IntoElement {
    let initial = match current {
        Some(Action::CustomShortcut(keys) | Action::HoldShortcut(keys)) => keys.rendered_label(),
        Some(Action::HoldGlobeKey) => "Fn".to_string(),
        _ => "T".to_string(),
    };
    let modes = match activation {
        ActionActivation::PhysicalPress => &[KeyboardActivation::Tap, KeyboardActivation::Hold][..],
        ActionActivation::Deferred => &[KeyboardActivation::Tap][..],
    };
    v_flex().gap_1().children(modes.iter().map(|mode| {
        let mode = *mode;
        let (suffix, label) = match mode {
            KeyboardActivation::Tap => ("tap", tr!("actions.press_keyboard_keys")),
            KeyboardActivation::Hold => ("hold", tr!("actions.hold_keyboard_keys")),
        };
        let title = label.clone();
        let initial = initial.clone();
        let on_pick = on_pick.clone();
        control_button(SharedString::from(format!("{id}-{suffix}")))
            .w_full()
            .label(label)
            .on_click(move |_, window, cx| {
                let on_pick = on_pick.clone();
                edit_keys(
                    &initial,
                    title.clone(),
                    Rc::new(move |keys, window, cx| on_pick(mode.action(keys), window, cx)),
                    window,
                    cx,
                );
            })
    }))
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Modifier {
    Control,
    Option,
    Shift,
    Command,
    Fn,
}

impl Modifier {
    const ALL: [Self; 5] = [
        Self::Control,
        Self::Option,
        Self::Shift,
        Self::Command,
        Self::Fn,
    ];

    fn token(self) -> &'static str {
        match self {
            Self::Control => "Ctrl",
            Self::Option => "Alt",
            Self::Shift => "Shift",
            Self::Command => "Cmd",
            Self::Fn => "Fn",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Control => "Control",
            Self::Option => "Option / Alt",
            Self::Shift => "Shift",
            Self::Command => "Command / Meta",
            Self::Fn => "Fn / Globe",
        }
    }

    fn selected(self, keys: &KeyCombo) -> bool {
        match self {
            Self::Control => keys.has_control(),
            Self::Option => keys.has_option(),
            Self::Shift => keys.has_shift(),
            Self::Command => keys.has_command(),
            Self::Fn => keys.has_fn(),
        }
    }
}

#[derive(Clone)]
struct KeyChoice(Option<KeyboardUsage>);

impl SelectItem for KeyChoice {
    type Value = Option<KeyboardUsage>;

    fn title(&self) -> SharedString {
        self.0
            .map_or_else(|| tr!("actions.modifiers_only"), |key| key.label().into())
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

struct KeyboardEditor {
    modifiers: BTreeSet<Modifier>,
    key: Entity<SelectState<Vec<KeyChoice>>>,
    _selection: Subscription,
}

impl KeyboardEditor {
    fn new(initial: Option<&KeyCombo>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let modifiers = Modifier::ALL
            .into_iter()
            .filter(|modifier| initial.is_some_and(|keys| modifier.selected(keys)))
            .collect();
        let choices: Vec<_> = std::iter::once(KeyChoice(None))
            .chain((4u8..=0x6f).filter_map(|code| {
                KeyboardUsage::try_from(code)
                    .ok()
                    .map(|key| KeyChoice(Some(key)))
            }))
            .collect();
        let selected = choices
            .iter()
            .position(|choice| choice.0 == initial.and_then(KeyCombo::key))
            .unwrap_or(0);
        let key = cx.new(|cx| {
            SelectState::new(choices, Some(IndexPath::new(selected)), window, cx).searchable(true)
        });
        let selection = cx.subscribe(&key, |_, _, _: &SelectEvent<Vec<KeyChoice>>, cx| {
            cx.notify();
        });
        Self {
            modifiers,
            key,
            _selection: selection,
        }
    }

    fn keys(&self, cx: &App) -> Result<KeyCombo, KeyComboParseError> {
        let mut parts: Vec<_> = self
            .modifiers
            .iter()
            .map(|modifier| modifier.token().to_string())
            .collect();
        if let Some(key) = self.key.read(cx).selected_value().copied().flatten() {
            parts.push(key.label());
        }
        parts.join("+").parse()
    }
}

pub(super) fn shortcut_keycaps(keys: &KeyCombo) -> impl IntoElement {
    let stroke = Keystroke {
        modifiers: Modifiers {
            control: keys.has_control(),
            alt: keys.has_option(),
            shift: keys.has_shift(),
            platform: keys.has_command(),
            ..Modifiers::default()
        },
        key: keys
            .key()
            .map_or_else(String::new, |key| key.label().to_lowercase()),
        key_char: None,
    };
    h_flex()
        .gap_1()
        .when(keys.has_fn(), |row| {
            row.child(
                Kbd::new(Keystroke {
                    key: "Fn".into(),
                    modifiers: Modifiers::default(),
                    key_char: None,
                })
                .text_body(),
            )
        })
        .when(
            keys.key().is_some()
                || keys.has_control()
                || keys.has_option()
                || keys.has_shift()
                || keys.has_command(),
            |row| row.child(Kbd::new(stroke).text_body()),
        )
}

impl Render for KeyboardEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let preview = self.keys(cx).map_or_else(
            |_| {
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("actions.choose_keyboard_keys"))
                    .into_any_element()
            },
            |keys| shortcut_keycaps(&keys).into_any_element(),
        );
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_caption()
                    .text_color(pal.text_muted)
                    .child(tr!("actions.keyboard_keys_help")),
            )
            .child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap_3()
                    .children(Modifier::ALL.into_iter().map(|modifier| {
                        Checkbox::new(SharedString::from(format!(
                            "key-modifier-{}",
                            modifier.token()
                        )))
                        .debug_selector(move || format!("keyboard-modifier-{}", modifier.token()))
                        .label(modifier.label())
                        .checked(self.modifiers.contains(&modifier))
                        .disabled(modifier == Modifier::Fn && !cfg!(target_os = "macos"))
                        .on_change(cx.listener(
                            move |this, checked, _, cx| {
                                if *checked {
                                    this.modifiers.insert(modifier);
                                } else {
                                    this.modifiers.remove(&modifier);
                                }
                                cx.notify();
                            },
                        ))
                    })),
            )
            .child(div().text_body().child(tr!("actions.ordinary_key")))
            .child(control_select(&self.key).accessibility_label(tr!("actions.ordinary_key")))
            .child(
                h_flex()
                    .min_h_8()
                    .gap_3()
                    .justify_between()
                    .child(
                        div()
                            .text_caption()
                            .text_color(pal.text_muted)
                            .child(tr!("actions.shortcut_preview")),
                    )
                    .child(preview),
            )
    }
}

pub(super) fn edit_keys(
    initial: &str,
    title: SharedString,
    on_save: SaveKeys,
    window: &mut Window,
    cx: &mut App,
) {
    let initial = initial.parse::<KeyCombo>().ok();
    let editor = cx.new(|cx| KeyboardEditor::new(initial.as_ref(), window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        let submit = editor.clone();
        let on_save = on_save.clone();
        dialog
            .title(title.clone())
            .child(editor.clone())
            .footer(
                DialogFooter::new()
                    .child(
                        control_button("keyboard-cancel")
                            .debug_selector(|| "keyboard-cancel".into())
                            .label(tr!("common.cancel"))
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
                    )
                    .child(
                        control_button("keyboard-save")
                            .debug_selector(|| "keyboard-save".into())
                            .label(tr!("common.confirm"))
                            .primary()
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(Confirm { secondary: false }), cx);
                            }),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let Ok(keys) = submit.read(cx).keys(cx) else {
                    return false;
                };
                on_save(keys, window, cx);
                true
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::i18n::LOCALE_LOCK;
    use gpui::{Modifiers, TestAppContext, px, size};

    #[gpui::test]
    fn modifier_checkboxes_build_and_clear_the_requested_shortcut(cx: &mut TestAppContext) {
        // Keep translated layout stable while other tests switch the process locale.
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(gpui_component::init);
        let (editor, visual) = cx.add_window_view(|window, cx| {
            KeyboardEditor::new(Some(&"T".parse().unwrap()), window, cx)
        });
        visual.simulate_resize(size(px(480.), px(300.)));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        for selector in [
            "keyboard-modifier-Ctrl",
            "keyboard-modifier-Alt",
            "keyboard-modifier-Shift",
        ] {
            let bounds = visual.debug_bounds(selector).unwrap();
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.update(|window, cx| window.draw(cx).clear(cx));
        }
        editor.read_with(visual, |editor, cx| {
            assert_eq!(editor.keys(cx).unwrap(), "⌃⌥⇧T".parse().unwrap());
        });
        let bounds = visual.debug_bounds("keyboard-modifier-Shift").unwrap();
        visual.simulate_click(bounds.center(), Modifiers::default());
        editor.read_with(visual, |editor, cx| {
            assert_eq!(editor.keys(cx).unwrap().rendered_label(), "Ctrl+Alt+T");
        });
        drop(editor);
        visual.update(|window, _| window.remove_window());
        visual.run_until_parked();
    }

    #[gpui::test]
    fn fn_checkbox_works_without_an_ordinary_key(cx: &mut TestAppContext) {
        // Keep translated layout stable while other tests switch the process locale.
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(gpui_component::init);
        let (editor, visual) =
            cx.add_window_view(|window, cx| KeyboardEditor::new(None, window, cx));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        editor.read_with(visual, |editor, cx| {
            assert_eq!(editor.keys(cx).unwrap_err(), KeyComboParseError::Empty);
        });
        let bounds = visual.debug_bounds("keyboard-modifier-Fn").unwrap();
        visual.simulate_click(bounds.center(), Modifiers::default());
        editor.read_with(visual, |editor, cx| {
            if cfg!(target_os = "macos") {
                assert_eq!(editor.keys(cx).unwrap(), KeyCombo::FN);
            } else {
                assert_eq!(editor.keys(cx).unwrap_err(), KeyComboParseError::Empty);
            }
        });
        drop(editor);
        visual.update(|window, _| window.remove_window());
        visual.run_until_parked();
    }
    #[gpui::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one real dialog flow verifies commit, cancellation, and invalid submission against the same owner"
    )]
    fn keyboard_dialog_confirms_cancels_and_rejects_empty_keys(cx: &mut TestAppContext) {
        use crate::services::assets::AssetResolver;
        use crate::state::{AgentLink, AppState, ConfigPersistence};
        use gpui_component::Root;
        use openlogi_core::config::Config;
        use openlogi_ipc::{AgentStatus, InventoryHealth, PROTOCOL_VERSION};
        use std::cell::RefCell;
        let _locale = LOCALE_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        cx.update(|cx| {
            gpui_component::init(cx);
            let (commands, _) = tokio::sync::mpsc::unbounded_channel();
            let mut state = AppState::with_runtime(
                Config::ephemeral(),
                &[],
                &[],
                &AssetResolver::new(),
                &[],
                ConfigPersistence::MemoryOnly,
                commands,
            );
            state.set_agent_link(AgentLink::Ready(AgentStatus {
                accessibility_granted: true,
                hook_installed: true,
                launch_at_login: false,
                inventory: InventoryHealth::Ready,
                protocol_version: PROTOCOL_VERSION,
                agent_version: "keyboard-dialog-test".into(),
                input_monitoring_granted: true,
                hid_open_failures: false,
            }));
            let state = cx.new(|_| state);
            AppState::set_global(state, cx);
        });
        let (root, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| crate::app::AppView::new(&[], window, cx));
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(1000.), px(800.)));
        let saved = Rc::new(RefCell::new(Vec::new()));
        let output = saved.clone();
        let save: SaveKeys = Rc::new(move |keys, _, _| output.borrow_mut().push(keys));
        visual.update(|window, cx| {
            edit_keys("T", "Keyboard shortcut".into(), save.clone(), window, cx);
            window.draw(cx).clear(cx);
        });
        // The upstream dialog slides using wall-clock animation (250 ms).
        std::thread::sleep(std::time::Duration::from_millis(300));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        // Actual footer buttons must be present and separate; Enter alone isn't enough.
        let cancel = visual.debug_bounds("keyboard-cancel").unwrap();
        let confirm = visual.debug_bounds("keyboard-save").unwrap();
        assert!(cancel.right() <= confirm.left());
        for selector in [
            "keyboard-modifier-Ctrl",
            "keyboard-modifier-Alt",
            "keyboard-modifier-Shift",
            "keyboard-save",
        ] {
            let bounds = visual.debug_bounds(selector).unwrap();
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        }
        assert_eq!(
            *saved.borrow(),
            ["Ctrl+Alt+Shift+T".parse::<KeyCombo>().unwrap()]
        );
        assert!(visual.debug_bounds("dialog-layer").is_none());
        // Changes in a canceled dialog must never reach the owner.
        visual.update(|window, cx| {
            edit_keys(
                "Ctrl+T",
                "Keyboard shortcut".into(),
                save.clone(),
                window,
                cx,
            );
            window.draw(cx).clear(cx);
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        for selector in ["keyboard-modifier-Shift", "keyboard-cancel"] {
            let bounds = visual.debug_bounds(selector).unwrap();
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        }
        assert_eq!(saved.borrow().len(), 1);
        assert!(visual.debug_bounds("dialog-layer").is_none());
        visual.update(|window, cx| {
            edit_keys("", "Keyboard shortcut".into(), save.clone(), window, cx);
            window.draw(cx).clear(cx);
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let confirm = visual.debug_bounds("keyboard-save").unwrap();
        visual.simulate_click(confirm.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(saved.borrow().len(), 1);
        assert!(visual.debug_bounds("dialog-layer").is_some());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("dialog-layer").is_none());
        drop(root);
        visual.update(|window, _| window.remove_window());
        visual.run_until_parked();
    }
}
