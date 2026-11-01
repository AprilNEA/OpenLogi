use std::{cell::Cell, rc::Rc};

use super::*;
use gpui::{
    Context, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render, TestAppContext,
    Window,
};

struct InteractionHarness {
    activations: Rc<Cell<usize>>,
    disabled: bool,
}

impl Render for InteractionHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let activations = self.activations.clone();
        div().tab_group().size(px(400.)).child(
            retry_line("retry", "Couldn't read DPI.", move |_| {
                activations.set(activations.get() + 1);
            })
            .disabled(self.disabled)
            .debug_selector(|| "retry-action".to_owned()),
        )
    }
}

fn press(visual: &mut gpui::VisualTestContext, key: &str) {
    let keystroke = Keystroke::parse(key).unwrap();
    visual.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    visual.simulate_event(KeyUpEvent { keystroke });
}

/// The message is plain text and the button is the one focus stop, so Tab
/// reaches the retry and Enter / Space run it; disabling the row (the device
/// went offline) must make both keyboard and pointer activation inert.
#[gpui::test]
fn retry_activates_once_per_enter_or_space_and_is_inert_when_disabled(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let activations = Rc::new(Cell::new(0));
    let (view, visual) = cx.add_window_view({
        let activations = activations.clone();
        move |_, _| InteractionHarness {
            activations,
            disabled: false,
        }
    });
    visual.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.focus_next(cx);
        assert!(window.focused(cx).is_some());
        window.draw(cx).clear(cx);
    });
    for (index, key) in ["enter", "space"].into_iter().enumerate() {
        press(visual, key);
        assert_eq!(activations.get(), index + 1);
    }
    // Disable the already-focused control, as an online-to-offline render does.
    view.update(visual, |view, cx| {
        view.disabled = true;
        cx.notify();
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    for key in ["enter", "space"] {
        press(visual, key);
        assert_eq!(
            activations.get(),
            2,
            "disabled keyboard activation must be inert"
        );
    }
    let button = visual.debug_bounds("retry-action").unwrap();
    visual.simulate_click(button.center(), Modifiers::default());
    assert_eq!(
        activations.get(),
        2,
        "disabled pointer activation must be inert"
    );
}
