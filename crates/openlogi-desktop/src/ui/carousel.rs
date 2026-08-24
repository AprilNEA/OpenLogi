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
//! Carousel::new("devices", px(240.))
//!     .len(devices.len())
//!     .selected(current)
//!     .render_item(move |ix, selected, _w, cx| render_device(ix, selected, cx))
//!     .on_select(cx.listener(|this, ix: &usize, _, cx| this.select(*ix, cx)))
//! ```

use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    RenderOnce, ScrollHandle, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, IconName, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

type SelectHandler = Rc<dyn Fn(&usize, &mut Window, &mut App) + 'static>;
type ItemRenderer = Rc<dyn Fn(usize, bool, &mut Window, &mut App) -> AnyElement + 'static>;

/// Side padding of the scrolling row.
const ROW_PAD: f32 = 24.;

/// A controlled equal-size card carousel. See the module docs.
#[derive(IntoElement)]
pub struct Carousel {
    id: ElementId,
    card_w: Pixels,
    len: usize,
    selected: usize,
    render_item: Option<ItemRenderer>,
    gap: Pixels,
    on_select: Option<SelectHandler>,
}

impl Carousel {
    /// Create a carousel whose cards are `card_w` wide. `id` keys scroll state.
    pub fn new(id: impl Into<ElementId>, card_w: Pixels) -> Self {
        Self {
            id: id.into(),
            card_w,
            len: 0,
            selected: 0,
            render_item: None,
            gap: px(16.),
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

    /// Gap between items. Default 16px.
    #[must_use]
    pub fn gap(mut self, gap: Pixels) -> Self {
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
    fn render_row(self, window: &mut Window, cx: &mut App) -> AnyElement {
        let Self {
            id,
            card_w,
            len,
            selected,
            render_item,
            gap,
            on_select,
        } = self;
        let Some(render_item) = render_item.filter(|_| len > 0) else {
            return div().into_any_element();
        };
        let selected = selected.min(len - 1);
        let multi = len > 1;

        let scroll_state =
            window.use_keyed_state((id, "scroll"), cx, |_, _| (usize::MAX, ScrollHandle::new()));
        let previous = scroll_state.read(cx).0;
        let scroll_handle = scroll_state.read(cx).1.clone();
        if previous != selected {
            scroll_handle.scroll_to_item(selected + 1);
            scroll_state.update(cx, |state, _| state.0 = selected);
        }

        let mut items = Vec::with_capacity(len);
        for i in 0..len {
            items.push(
                div()
                    .w(card_w)
                    .flex_shrink_0()
                    .child(render_item(i, i == selected, window, cx))
                    .into_any_element(),
            );
        }

        // Flexible edge spacers centre short rows without putting overflowing
        // cards at a negative, unreachable offset. They are immediate children,
        // so the scroll handle targets cards at `selected + 1`.
        let edge_spacer = px((ROW_PAD - f32::from(gap)).max(0.));
        let row = h_flex()
            .id("carousel-row")
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
                    "carousel-prev",
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
                    "carousel-next",
                    IconName::ChevronRight,
                    (selected + 1).min(len - 1),
                    selected + 1 >= len,
                    Size::Large,
                    on_select.clone(),
                ))
            });

        v_flex().size_full().pb_6().child(stage).into_any_element()
    }
}

fn arrow(
    id: &'static str,
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
