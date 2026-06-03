//! A horizontal carousel of items.
//!
//! A *controlled* component, in the same spirit as [`gpui_component::tab::TabBar`]:
//! the caller owns the selected index and passes it in via [`Carousel::selected`],
//! reacting to changes through [`Carousel::on_select`]. Previous/next arrows and
//! clickable page-indicator dots drive `on_select`; the carousel animates its
//! scroll position to the selected item. Scroll state is persisted across frames
//! keyed by the carousel's `id`, so the caller holds nothing but the index.
//!
//! ```ignore
//! Carousel::new("devices")
//!     .selected(current)
//!     .item_width(px(220.))
//!     .children(cards)
//!     .on_select(cx.listener(|this, ix: &usize, _, cx| this.select(*ix, cx)))
//! ```

use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, RenderOnce, ScrollHandle, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

type SelectHandler = Rc<dyn Fn(&usize, &mut Window, &mut App) + 'static>;

/// A horizontal, controlled carousel. See the module docs.
#[derive(IntoElement)]
pub struct Carousel {
    id: ElementId,
    children: Vec<AnyElement>,
    selected: usize,
    gap: Pixels,
    item_width: Option<Pixels>,
    arrows: bool,
    indicators: bool,
    loop_around: bool,
    accent: Option<Hsla>,
    on_select: Option<SelectHandler>,
}

#[allow(
    dead_code,
    reason = "complete, reusable carousel API — not every builder option is exercised by the current device-list call site"
)]
impl Carousel {
    /// Create a carousel. `id` must be stable — it keys the persisted scroll
    /// position across frames.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            children: Vec::new(),
            selected: 0,
            gap: px(12.),
            item_width: None,
            arrows: true,
            indicators: true,
            loop_around: false,
            accent: None,
            on_select: None,
        }
    }

    /// Append one item.
    #[must_use]
    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_any_element());
        self
    }

    /// Append many items.
    #[must_use]
    pub fn children(mut self, children: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.children
            .extend(children.into_iter().map(IntoElement::into_any_element));
        self
    }

    /// The currently selected item (clamped to range when rendered).
    #[must_use]
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Gap between items. Default 12px.
    #[must_use]
    pub fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
        self
    }

    /// Give every item a fixed width, so paging is uniform. Without it items
    /// keep their natural width.
    #[must_use]
    pub fn item_width(mut self, width: Pixels) -> Self {
        self.item_width = Some(width);
        self
    }

    /// Show the prev/next arrows. Default `true`.
    #[must_use]
    pub fn arrows(mut self, show: bool) -> Self {
        self.arrows = show;
        self
    }

    /// Show the page-indicator dots. Default `true`.
    #[must_use]
    pub fn indicators(mut self, show: bool) -> Self {
        self.indicators = show;
        self
    }

    /// Wrap the arrows around the ends instead of stopping. Default `false`.
    #[must_use]
    pub fn loop_around(mut self, loop_around: bool) -> Self {
        self.loop_around = loop_around;
        self
    }

    /// Accent colour for the active indicator dot. Defaults to the theme's
    /// primary colour.
    #[must_use]
    pub fn accent(mut self, accent: Hsla) -> Self {
        self.accent = Some(accent);
        self
    }

    /// Called with the new index when an arrow or indicator is activated.
    #[must_use]
    pub fn on_select(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Carousel {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            children,
            selected,
            gap,
            item_width,
            arrows,
            indicators,
            loop_around,
            accent,
            on_select,
        } = self;

        let len = children.len();
        let selected = selected.min(len.saturating_sub(1));
        let multi = len > 1;

        // Scroll position persisted by id, plus the index we last scrolled to so
        // a manual scroll isn't yanked back to the selection every frame — only
        // a *change* in the selected index re-scrolls.
        let scroll = window
            .use_keyed_state(format!("{id}-carousel-scroll"), cx, |_, _| {
                ScrollHandle::new()
            })
            .read(cx)
            .clone();
        let last_sel = window.use_keyed_state(format!("{id}-carousel-sel"), cx, |_, _| usize::MAX);
        if multi && *last_sel.read(cx) != selected {
            scroll.scroll_to_item(selected);
            last_sel.update(cx, |v, _| *v = selected);
        }

        let accent = accent.unwrap_or(cx.theme().primary);
        let dot_idle = cx.theme().border;

        let items = children.into_iter().map(move |child| match item_width {
            Some(w) => div().flex_shrink_0().w(w).child(child).into_any_element(),
            None => child,
        });

        let track = h_flex()
            .id("carousel-track")
            .flex_1()
            .min_w_0()
            .justify_center()
            .overflow_x_scroll()
            .track_scroll(&scroll)
            .gap(gap)
            .children(items);

        let prev_target = selected.checked_sub(1).unwrap_or(len.saturating_sub(1));
        let next_target = if selected + 1 >= len { 0 } else { selected + 1 };
        let prev_disabled = !loop_around && selected == 0;
        let next_disabled = !loop_around && selected + 1 >= len;
        let on_prev = on_select.clone();
        let on_next = on_select.clone();
        let on_dot = on_select;

        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .when(arrows && multi, |this| {
                        this.child(arrow(
                            "carousel-prev",
                            IconName::ChevronLeft,
                            prev_target,
                            prev_disabled,
                            on_prev,
                        ))
                    })
                    .child(track)
                    .when(arrows && multi, |this| {
                        this.child(arrow(
                            "carousel-next",
                            IconName::ChevronRight,
                            next_target,
                            next_disabled,
                            on_next,
                        ))
                    }),
            )
            .when(indicators && multi, |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_center()
                        .gap_1p5()
                        .children(
                            (0..len)
                                .map(|i| dot(i, i == selected, accent, dot_idle, on_dot.clone())),
                        ),
                )
            })
    }
}

fn arrow(
    id: &'static str,
    icon: IconName,
    target: usize,
    disabled: bool,
    on_select: Option<SelectHandler>,
) -> impl IntoElement {
    Button::new(id)
        .icon(icon)
        .ghost()
        .xsmall()
        .disabled(disabled)
        .when_some(on_select.filter(|_| !disabled), |this, handler| {
            this.on_click(move |_, window, cx| handler(&target, window, cx))
        })
}

fn dot(
    index: usize,
    active: bool,
    accent: Hsla,
    idle: Hsla,
    on_select: Option<SelectHandler>,
) -> impl IntoElement {
    let size = if active { px(8.) } else { px(6.) };
    div()
        .id(("carousel-dot", index))
        .w(size)
        .h(size)
        .rounded_full()
        .bg(if active { accent } else { idle })
        .cursor_pointer()
        .when_some(on_select, |this, handler| {
            this.on_click(move |_, window, cx| handler(&index, window, cx))
        })
}
