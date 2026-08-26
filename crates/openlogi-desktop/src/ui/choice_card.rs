//! Semantic selectable cards for bespoke pickers.

use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, Interactivity, IntoElement,
    ParentElement, RenderOnce, Role, SharedString, StatefulInteractiveElement, StyleRefinement,
    Styled, Toggled, Window, prelude::FluentBuilder as _,
};
use gpui_base::Button as BaseButton;
use gpui_component::{Disableable, Selectable};

type ClickHandler = std::rc::Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A controlled radio-card that keeps custom layout and styling while using
/// the shared semantic button primitive for focus, keyboard activation, and
/// accessibility state.
#[derive(IntoElement)]
pub(crate) struct ChoiceCard {
    base: BaseButton,
    selected: bool,
    disabled: bool,
    label: SharedString,
    children: Vec<AnyElement>,
    on_click: Option<ClickHandler>,
}

impl ChoiceCard {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        accessibility_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            base: BaseButton::new(id),
            selected: false,
            disabled: false,
            label: accessibility_label.into(),
            children: Vec::new(),
            on_click: None,
        }
    }

    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(std::rc::Rc::new(handler));
        self
    }
}

impl Selectable for ChoiceCard {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl Disableable for ChoiceCard {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Styled for ChoiceCard {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for ChoiceCard {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl InteractiveElement for ChoiceCard {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for ChoiceCard {}

impl RenderOnce for ChoiceCard {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let selected = self.selected;
        self.base
            .role(Role::RadioButton)
            .selected(selected)
            .disabled(self.disabled)
            .accessibility_label(self.label)
            .aria_toggled(if selected {
                Toggled::True
            } else {
                Toggled::False
            })
            .aria_selected(selected)
            .children(self.children)
            .when_some(self.on_click, |card, handler| {
                card.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        Context, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render, TestAppContext, div,
        point, px,
    };

    use super::*;

    struct ChoiceHarness {
        selected: bool,
        disabled: bool,
        activations: Rc<Cell<usize>>,
        parent_clicks: Rc<Cell<usize>>,
    }

    impl Render for ChoiceHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let activations = self.activations.clone();
            let parent_clicks = self.parent_clicks.clone();
            div()
                .id("choice-parent")
                .tab_group()
                .size(px(100.))
                .on_click(move |_, _, _| parent_clicks.set(parent_clicks.get() + 1))
                .child(
                    ChoiceCard::new("keyboard-choice", "Choice")
                        .selected(self.selected)
                        .disabled(self.disabled)
                        .size_full()
                        .child("Choice")
                        .on_click(move |_, _, _| activations.set(activations.get() + 1)),
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
    fn choice_card_is_focusable_and_activates_in_both_controlled_states(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let activations = Rc::new(Cell::new(0));
        let parent_clicks = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view({
            let activations = activations.clone();
            let parent_clicks = parent_clicks.clone();
            move |_, _| ChoiceHarness {
                selected: false,
                disabled: false,
                activations,
                parent_clicks,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(Window::focus_next);
        cx.update(|window, cx| assert!(window.focused(cx).is_some()));

        activate_key(cx, "enter");
        view.update(cx, |view, cx| {
            view.selected = true;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        activate_key(cx, "space");

        assert_eq!(activations.get(), 2);
        assert_eq!(parent_clicks.get(), 0);
    }

    #[gpui::test]
    fn disabled_choice_card_is_inert_and_blocks_its_parent(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let activations = Rc::new(Cell::new(0));
        let parent_clicks = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let activations = activations.clone();
            let parent_clicks = parent_clicks.clone();
            move |_, _| ChoiceHarness {
                selected: false,
                disabled: true,
                activations,
                parent_clicks,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        cx.update(Window::focus_next);
        cx.update(|window, cx| assert!(window.focused(cx).is_none()));
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        activate_key(cx, "enter");
        activate_key(cx, "space");

        assert_eq!(activations.get(), 0);
        assert_eq!(parent_clicks.get(), 0);
    }
}
