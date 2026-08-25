//! A centered, horizontally scrolling carousel of equal-size cards.
//!
//! The row centres while its cards fit the viewport and becomes scrollable when
//! they overflow. Selecting an item scrolls it into view; arrows navigate the
//! controlled selection.
//!
//! Controlled, in the same spirit as [`gpui_component::tab::TabBar`]: the caller
//! owns the selected index ([`Carousel::selected`]) and item count
//! ([`Carousel::len`]), supplies items through [`Carousel::render_item`] (invoked
//! per slot with whether it is selected), and reacts to
//! navigation through [`Carousel::on_select`].
//!
//! ```ignore
//! Carousel::new("devices", rems(15.))
//!     .len(devices.len())
//!     .selected(current)
//!     .render_item(move |ix, selected, _w, cx| render_device(ix, selected, cx))
//!     .on_select(cx.listener(|this, ix: &usize, _, cx| this.select(*ix, cx)))
//! ```

use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Rems,
    RenderOnce, ScrollHandle, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, rems,
};
use gpui_component::{
    Disableable as _, IconName, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

type SelectHandler = Rc<dyn Fn(&usize, &mut Window, &mut App) + 'static>;
type ItemRenderer = Rc<dyn Fn(usize, bool, &mut Window, &mut App) -> AnyElement + 'static>;

/// Side padding of the scrolling row.
const ROW_PAD: Rems = rems(1.5);

/// A controlled equal-size card carousel. See the module docs.
#[derive(IntoElement)]
pub struct Carousel {
    id: ElementId,
    card_w: Rems,
    len: usize,
    selected: usize,
    render_item: Option<ItemRenderer>,
    gap: Rems,
    on_select: Option<SelectHandler>,
}

impl Carousel {
    /// Create a carousel whose cards are `card_w` wide. `id` keys scroll state.
    pub fn new(id: impl Into<ElementId>, card_w: Rems) -> Self {
        Self {
            id: id.into(),
            card_w,
            len: 0,
            selected: 0,
            render_item: None,
            gap: rems(1.),
            on_select: None,
        }
    }

    /// Total number of items.
    #[must_use]
    pub fn len(mut self, len: usize) -> Self {
        self.len = len;
        self
    }

    /// The selected item, clamped to range when rendered.
    #[must_use]
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Item renderer, called per slot with `(index, selected)`. Reads live data
    /// each render, so it never goes stale.
    #[must_use]
    pub fn render_item(
        mut self,
        f: impl Fn(usize, bool, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        self.render_item = Some(Rc::new(f));
        self
    }

    /// Gap between items. Default 1rem.
    #[must_use]
    pub fn gap(mut self, gap: Rems) -> Self {
        self.gap = gap;
        self
    }

    /// Called with the new index when an arrow is activated.
    #[must_use]
    pub fn on_select(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Carousel {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.render_row(window, cx)
    }
}

impl Carousel {
    /// Every item renders at `card_w` in a horizontally scrollable row that
    /// centres while the cards fit the viewport and left-aligns (so the scroll
    /// reaches the first card) once they overflow. Prev/next arrows hug the
    /// screen edges; each card's click and selected styling come from
    /// `render_item`.
    fn render_row(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            card_w,
            len,
            selected,
            render_item,
            gap,
            on_select,
        } = self;
        v_flex().when_some(render_item.filter(|_| len > 0), |this, render_item| {
            let selected = selected.min(len - 1);
            let multi = len > 1;

            let scroll_state = window.use_keyed_state((id.clone(), "scroll"), cx, |_, _| {
                (usize::MAX, ScrollHandle::new())
            });
            let previous = scroll_state.read(cx).0;
            let scroll_handle = scroll_state.read(cx).1.clone();
            if previous != selected {
                scroll_handle.scroll_to_item(selected + 1);
                scroll_state.update(cx, |state, _| state.0 = selected);
            }

            let mut items = Vec::with_capacity(len);
            for i in 0..len {
                items.push(div().w(card_w).flex_shrink_0().child(render_item(
                    i,
                    i == selected,
                    window,
                    cx,
                )));
            }

            // Flexible edge spacers centre short rows without putting overflowing
            // cards at a negative, unreachable offset. They are immediate children,
            // so the scroll handle targets cards at `selected + 1`.
            let edge_spacer = rems((ROW_PAD.0 - gap.0).max(0.));
            let row = h_flex()
                .id((id.clone(), "row"))
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_x_scroll()
                .track_scroll(&scroll_handle)
                .items_center()
                .gap(gap)
                .py_4()
                .child(div().flex_1().min_w(edge_spacer))
                .children(items)
                .child(div().flex_1().min_w(edge_spacer));

            // Prev/next arrows hug the left and right edges, flanking the row.
            let stage = h_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .items_center()
                .px_4()
                .when(multi, |this| {
                    this.child(arrow(
                        (id.clone(), "previous").into(),
                        IconName::ChevronLeft,
                        selected.saturating_sub(1),
                        selected == 0,
                        Size::Large,
                        on_select.clone(),
                    ))
                })
                .child(row)
                .when(multi, |this| {
                    this.child(arrow(
                        (id.clone(), "next").into(),
                        IconName::ChevronRight,
                        (selected + 1).min(len - 1),
                        selected + 1 >= len,
                        Size::Large,
                        on_select.clone(),
                    ))
                });

            this.size_full().pb_6().child(stage)
        })
    }
}

fn arrow(
    id: ElementId,
    icon: IconName,
    target: usize,
    disabled: bool,
    size: Size,
    on_select: Option<SelectHandler>,
) -> impl IntoElement {
    Button::new(id)
        .icon(icon)
        .ghost()
        .with_size(size)
        .disabled(disabled)
        .when_some(on_select.filter(|_| !disabled), |this, handler| {
            this.on_click(move |_, window, cx| handler(&target, window, cx))
        })
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{Context, KeyDownEvent, KeyUpEvent, Keystroke, Render, TestAppContext, div, px};

    use super::*;

    struct CarouselHarness {
        selected: usize,
        instances: usize,
        selections: Rc<RefCell<Vec<(usize, usize)>>>,
    }

    impl Render for CarouselHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let mut row = h_flex().tab_group().size(px(600.));
            for instance in 0..self.instances {
                let selections = self.selections.clone();
                row = row.child(
                    Carousel::new(("test-carousel", instance), rems(7.5))
                        .len(3)
                        .selected(self.selected)
                        .render_item(|index, _, _, _| {
                            div().child(format!("Item {index}")).into_any_element()
                        })
                        .on_select(move |index, _, _| {
                            selections.borrow_mut().push((instance, *index));
                        }),
                );
            }
            row
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
    fn carousel_arrows_are_controlled_and_instance_local(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let selections = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let selections = selections.clone();
            move |_, _| CarouselHarness {
                selected: 1,
                instances: 2,
                selections,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        for _ in 0..3 {
            cx.update(Window::focus_next);
            activate_key(cx, "enter");
        }

        assert_eq!(&*selections.borrow(), &[(0, 0), (0, 2), (1, 0)]);
    }

    #[gpui::test]
    fn carousel_clamps_selection_before_navigating(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let selections = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let selections = selections.clone();
            move |_, _| CarouselHarness {
                selected: usize::MAX,
                instances: 1,
                selections,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        cx.update(Window::focus_next);
        activate_key(cx, "space");

        assert_eq!(&*selections.borrow(), &[(0, 1)]);
    }
}
