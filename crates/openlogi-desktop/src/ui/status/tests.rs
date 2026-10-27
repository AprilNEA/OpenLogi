use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gpui::{
    Context, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke,
    Modifiers, Render, TestAppContext, Window, canvas,
};
use gpui_component::{Disableable as _, Theme, ThemeMode, v_flex};

use super::*;
use crate::ui::theme;

struct StyleHarness(Rc<RefCell<Vec<Hsla>>>);

impl Render for StyleHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Keep the default pointer at (0, 0) outside every button: compare
        // normal colors, not the first button's hover color against the rest.
        v_flex()
            .p_4()
            .children([false, true].into_iter().flat_map(|disabled| {
                [false, true].map(|stock| {
                    let colors = self.0.clone();
                    let id = format!("retry-{stock}-{disabled}");
                    let button = if stock {
                        Button::new(id).text().label("Retry")
                    } else {
                        retry_line(id, "Retry", |_| {})
                    };
                    button.disabled(disabled).child(
                        // Observe inherited text color during actual paint, inside
                        // the same content container as the production label.
                        canvas(
                            |_, _, _| (),
                            move |_, (), window, _| {
                                colors.borrow_mut().push(window.text_style().color);
                            },
                        )
                        .size_1(),
                    )
                })
            }))
    }
}

#[gpui::test]
fn retry_retains_button_disabled_foreground_in_both_themes(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(theme::register_builtin_themes);
    let colors = Rc::new(RefCell::new(Vec::new()));
    let (_, visual) = cx.add_window_view({
        let colors = colors.clone();
        move |_, _| StyleHarness(colors)
    });
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        visual.update(|window, cx| Theme::change(mode, Some(window), cx));
        colors.borrow_mut().clear();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let observed = colors.borrow();
        assert_eq!(observed.len(), 4);
        assert_eq!(
            observed[0], observed[1],
            "retry must retain stock enabled color"
        );
        assert_eq!(
            observed[2], observed[3],
            "retry must retain stock disabled color"
        );
        assert_ne!(
            observed[0], observed[2],
            "disabled retry must not look enabled"
        );
    }
}

struct InteractionHarness {
    activations: Rc<Cell<usize>>,
    disabled: bool,
}

impl Render for InteractionHarness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let activations = self.activations.clone();
        div().tab_group().size(px(200.)).child(
            retry_line("retry", "Retry", move |_| {
                activations.set(activations.get() + 1);
            })
            .disabled(self.disabled)
            .debug_selector(|| "retry-action".to_owned()),
        )
    }
}

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
        let keystroke = Keystroke::parse(key).unwrap();
        visual.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        visual.simulate_event(KeyUpEvent { keystroke });
        assert_eq!(activations.get(), index + 1);
    }
    // Disable the already-focused control, as an online-to-offline render does.
    view.update(visual, |view, cx| {
        view.disabled = true;
        cx.notify();
    });
    visual.update(|window, cx| window.draw(cx).clear(cx));
    for key in ["enter", "space"] {
        let keystroke = Keystroke::parse(key).unwrap();
        visual.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        visual.simulate_event(KeyUpEvent { keystroke });
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
