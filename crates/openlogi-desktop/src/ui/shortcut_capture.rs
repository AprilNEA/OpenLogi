//! Keystroke recorder for custom keyboard shortcuts.
//!
//! A text field cannot tell Home from a cursor-motion, or AZERTY `-` (the 6
//! key) from the minus key or the numpad. This control intercepts GPUI
//! keystrokes before keymap bindings and maps them to [`KeyCombo`] HID usages.
//! Numpad digits are distinguished from main-row digits by a vendored GPUI
//! patch (`vendor/gpui/`) that keeps `kp*` key names.

use gpui::{
    App, Context, ElementId, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    Keystroke, KeystrokeEvent, ParentElement as _, Render, Role, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div, px, rgb,
};
use openlogi_core::binding::{CapturedKeystroke, KeyCombo};

use super::theme::{self, ACCENT_BLUE, CONTROL_H, Typography as _};

/// Focusable control that records one keyboard chord.
pub(crate) struct ShortcutCapture {
    focus_handle: FocusHandle,
    id: ElementId,
    placeholder: SharedString,
    combo: Option<KeyCombo>,
    _intercept: Subscription,
}

impl ShortcutCapture {
    pub(crate) fn new(id: impl Into<ElementId>, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let listener = cx.listener(|this, event: &KeystrokeEvent, window, cx| {
            if !this.focus_handle.is_focused(window) {
                return;
            }
            this.record(&event.keystroke, window, cx);
        });
        let intercept = cx.intercept_keystrokes(listener);
        Self {
            focus_handle,
            id: id.into(),
            placeholder: SharedString::default(),
            combo: None,
            _intercept: intercept,
        }
    }

    pub(crate) fn set_placeholder(&mut self, placeholder: SharedString, cx: &mut Context<Self>) {
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    fn record(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.prevent_default();
        let Some(combo) = KeyCombo::from_captured(
            CapturedKeystroke::new(&keystroke.key, cx.keyboard_layout().name())
                .command(keystroke.modifiers.platform)
                .shift(keystroke.modifiers.shift)
                .control(keystroke.modifiers.control)
                .option(keystroke.modifiers.alt),
        ) else {
            return;
        };
        self.combo = Some(combo.clone());
        cx.emit(combo);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn combo(&self) -> Option<&KeyCombo> {
        self.combo.as_ref()
    }
}

impl EventEmitter<KeyCombo> for ShortcutCapture {}

impl Focusable for ShortcutCapture {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ShortcutCapture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let focused = self.focus_handle.is_focused(window);
        let (label, muted) = self.combo.as_ref().map_or_else(
            || (self.placeholder.to_string(), true),
            |combo| (combo.rendered_label(), false),
        );
        div()
            .id(self.id.clone())
            .track_focus(&self.focus_handle)
            .tab_index(0)
            .role(Role::TextInput)
            .flex()
            .w_full()
            .min_w_0()
            .h(px(CONTROL_H))
            .min_h(px(CONTROL_H))
            .px_2()
            .items_center()
            .rounded(pal.control_radius)
            .border_1()
            .border_color(if focused {
                rgb(ACCENT_BLUE).into()
            } else {
                pal.border
            })
            .bg(pal.control)
            .hover(move |style| style.bg(pal.control_hover))
            .on_click(cx.listener(|this, _, window, cx| {
                this.focus_handle.focus(window, cx);
            }))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .text_body()
                    .text_color(if muted {
                        pal.text_muted
                    } else {
                        pal.text_primary
                    })
                    .child(label),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, KeyDownEvent, Keystroke, Render, TestAppContext, Window, div, px,
    };

    use super::*;

    struct Harness {
        capture: gpui::Entity<ShortcutCapture>,
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("shortcut-capture-harness")
                .tab_group()
                .size(px(200.))
                .child(self.capture.clone())
        }
    }

    #[gpui::test]
    fn home_and_keypad_minus_are_recorded_as_named_keys(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, cx| Harness {
            capture: cx.new(|cx| ShortcutCapture::new("test-shortcut-capture", cx)),
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.update(|window, cx| {
            view.read(cx).capture.focus_handle(cx).focus(window, cx);
        });
        cx.update(|window, cx| {
            assert!(
                window.focused(cx).is_some(),
                "shortcut capture must be focused to intercept keys"
            );
        });

        let home = Keystroke::parse("home").expect("GPUI names Home");
        cx.simulate_event(KeyDownEvent {
            keystroke: home,
            is_held: false,
            prefer_character_input: false,
        });
        view.update(cx, |view, cx| {
            assert_eq!(
                view.capture.read(cx).combo().map(KeyCombo::rendered_label),
                Some("Home".to_string())
            );
        });

        let subtract = Keystroke::parse("subtract").expect("GPUI names keypad minus");
        cx.simulate_event(KeyDownEvent {
            keystroke: subtract,
            is_held: false,
            prefer_character_input: false,
        });
        view.update(cx, |view, cx| {
            assert_eq!(
                view.capture.read(cx).combo().map(KeyCombo::rendered_label),
                Some("KpMinus".to_string())
            );
        });

        drop(view);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
    }
}
