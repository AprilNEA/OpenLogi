//! A centre-stage ("coverflow") carousel.
//!
//! The selected item is rendered large and centred; its immediate neighbours
//! peek smaller on either side. Selecting a neighbour — by clicking it, an
//! arrow, or a dot — brings it to the centre with a grow animation. All sizing
//! is relative to the viewport, so the carousel scales with the window without
//! any measurement.
//!
//! Controlled, in the same spirit as [`gpui_component::tab::TabBar`]: the caller
//! owns the selected index ([`Carousel::selected`]) and item count
//! ([`Carousel::len`]), supplies items through [`Carousel::render_item`] (invoked
//! per visible slot with whether it is the focused/centre item), and reacts to
//! navigation through [`Carousel::on_select`].
//!
//! ```ignore
//! Carousel::new("devices")
//!     .len(devices.len())
//!     .selected(current)
//!     .render_item(move |ix, focused, _w, cx| render_device(ix, focused, cx))
//!     .on_select(cx.listener(|this, ix: &usize, _, cx| this.select(*ix, cx)))
//! ```

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, ease_in_out, prelude::FluentBuilder as _,
    px, relative,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

type SelectHandler = Rc<dyn Fn(&usize, &mut Window, &mut App) + 'static>;
type ItemRenderer = Rc<dyn Fn(usize, bool, &mut Window, &mut App) -> AnyElement + 'static>;

/// Side padding of the uniform-mode row (also used in its fits-the-viewport
/// check, so the two stay in step).
const UNIFORM_PAD: f32 = 24.;

/// A centre-stage carousel. See the module docs.
#[derive(IntoElement)]
pub struct Carousel {
    id: ElementId,
    len: usize,
    selected: usize,
    render_item: Option<ItemRenderer>,
    /// When set, switch from coverflow to an equal-size scrolling row whose
    /// cards are this wide; coverflow's magnify/peek options are then ignored.
    uniform: Option<Pixels>,
    focused_frac: f32,
    side_frac: f32,
    gap: Pixels,
    arrows: bool,
    indicators: bool,
    accent: Option<Hsla>,
    on_select: Option<SelectHandler>,
}

#[allow(
    dead_code,
    reason = "complete, reusable carousel API — not every builder option is exercised by the current device-list call site"
)]
impl Carousel {
    /// Create a carousel. `id` keys the per-transition grow animation.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            len: 0,
            selected: 0,
            render_item: None,
            uniform: None,
            focused_frac: 0.44,
            side_frac: 0.17,
            gap: px(16.),
            arrows: true,
            indicators: true,
            accent: None,
            on_select: None,
        }
    }

    /// Total number of items.
    #[must_use]
    pub fn len(mut self, len: usize) -> Self {
        self.len = len;
        self
    }

    /// The selected (centre) item, clamped to range when rendered.
    #[must_use]
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Item renderer, called per visible slot with `(index, focused)`. `focused`
    /// is `true` for the centre item. Reads live data each render, so it never
    /// goes stale.
    #[must_use]
    pub fn render_item(
        mut self,
        f: impl Fn(usize, bool, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        self.render_item = Some(Rc::new(f));
        self
    }

    /// Lay the items out as an equal-size, horizontally scrollable row (each
    /// card `card_w` wide) instead of the centre-stage coverflow. In this mode
    /// `render_item`'s `focused` flag marks the *active* item (so the caller can
    /// style it), clicks are wired by `render_item` itself, and the coverflow
    /// options (`focused_frac` / `side_frac` / arrows / dots) are ignored.
    #[must_use]
    pub fn uniform(mut self, card_w: Pixels) -> Self {
        self.uniform = Some(card_w);
        self
    }

    /// Width of the focused item as a fraction of the viewport. Default 0.44.
    #[must_use]
    pub fn focused_frac(mut self, frac: f32) -> Self {
        self.focused_frac = frac;
        self
    }

    /// Width of each side (peek) item as a fraction of the viewport. Default 0.17.
    #[must_use]
    pub fn side_frac(mut self, frac: f32) -> Self {
        self.side_frac = frac;
        self
    }

    /// Gap between items. Default 16px.
    #[must_use]
    pub fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
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

    /// Accent colour for the active indicator dot. Defaults to the theme primary.
    #[must_use]
    pub fn accent(mut self, accent: Hsla) -> Self {
        self.accent = Some(accent);
        self
    }

    /// Called with the new index when a neighbour, arrow, or dot is activated.
    #[must_use]
    pub fn on_select(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Carousel {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        match self.uniform {
            Some(card_w) => self.render_uniform(card_w, window, cx),
            None => self.render_coverflow(window, cx),
        }
    }
}

impl Carousel {
    /// Equal-size scrolling row — see [`Carousel::uniform`]. Every item renders
    /// at `card_w` in a horizontally scrollable row that centres while the cards
    /// fit the viewport and left-aligns (so the scroll reaches the first card)
    /// once they overflow. Prev/next arrows hug the screen edges and the page
    /// dots sit underneath; each card's click and active styling come from
    /// `render_item`. The coverflow magnify/peek and slide are the only parts
    /// dropped.
    fn render_uniform(self, card_w: Pixels, window: &mut Window, cx: &mut App) -> AnyElement {
        let Self {
            len,
            selected,
            render_item,
            gap,
            arrows,
            indicators,
            accent,
            on_select,
            ..
        } = self;
        let Some(render_item) = render_item.filter(|_| len > 0) else {
            return div().into_any_element();
        };
        let selected = selected.min(len - 1);
        let multi = len > 1;
        let accent = accent.unwrap_or(cx.theme().primary);
        let dot_idle = cx.theme().border;

        let count = u16::try_from(len).map_or(f32::MAX, f32::from);
        let content_w =
            count * f32::from(card_w) + (count - 1.).max(0.) * f32::from(gap) + 2. * UNIFORM_PAD;
        let centered = content_w <= f32::from(window.viewport_size().width);
        let mut items = Vec::with_capacity(len);
        for i in 0..len {
            items.push(render_item(i, i == selected, window, cx));
        }

        let row = h_flex()
            .id("carousel-uniform")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_x_scroll()
            .items_center()
            .gap(gap)
            .px(px(UNIFORM_PAD))
            .py_4()
            .map(|row| if centered { row.justify_center() } else { row })
            .children(items);

        // Prev/next arrows hug the left and right edges (vertically centred),
        // flanking the scrollable row; the page dots sit centred underneath.
        let stage = h_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .items_center()
            .px_4()
            .when(multi && arrows, |this| {
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
            .when(multi && arrows, |this| {
                this.child(arrow(
                    "carousel-next",
                    IconName::ChevronRight,
                    (selected + 1).min(len - 1),
                    selected + 1 >= len,
                    Size::Large,
                    on_select.clone(),
                ))
            });

        v_flex()
            .size_full()
            .gap_3()
            .pb_6()
            .child(stage)
            .when(multi && indicators, |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_center()
                        .gap_1p5()
                        .children(
                            (0..len).map(|i| {
                                dot(i, i == selected, accent, dot_idle, on_select.clone())
                            }),
                        ),
                )
            })
            .into_any_element()
    }

    /// Centre-stage ("coverflow") layout — the default. See the module docs.
    fn render_coverflow(self, window: &mut Window, cx: &mut App) -> AnyElement {
        let Self {
            id,
            len,
            selected,
            render_item,
            focused_frac,
            side_frac,
            gap,
            arrows,
            indicators,
            accent,
            on_select,
            ..
        } = self;

        let Some(render_item) = render_item.filter(|_| len > 0) else {
            return div().into_any_element();
        };

        let selected = selected.min(len - 1);
        let multi = len > 1;
        let accent = accent.unwrap_or(cx.theme().primary);
        let dot_idle = cx.theme().border;
        let has_prev = selected > 0;
        let has_next = selected + 1 < len;

        // Direction of the most recent selection change (+1 → moved to a later
        // device, -1 → earlier), persisted across renders so the focused card's
        // slide-in keeps a stable sign for the whole transition even if the view
        // repaints mid-glide. Updated — and so notifies — only when `selected`
        // actually changes, otherwise it would loop.
        let slide_dir = {
            let state =
                window.use_keyed_state(SharedString::from(format!("{id}-dir")), cx, |_, _| {
                    (selected, 0i8)
                });
            let (prev, last) = *state.read(cx);
            if selected == prev {
                f32::from(last)
            } else {
                let d: i8 = if selected > prev { 1 } else { -1 };
                state.update(cx, |s, _| *s = (selected, d));
                f32::from(d)
            }
        };

        // Render the visible slot items fresh (the callback reads live data).
        let prev_el = has_prev.then(|| render_item(selected - 1, false, window, cx));
        let next_el = has_next.then(|| render_item(selected + 1, false, window, cx));
        let focused_el = render_item(selected, true, window, cx);

        // The focused slot transitions in on each selection change: its relative
        // width/height ramp from a smaller fraction up to the full focused
        // fraction, it fades up, and it slides horizontally into place from the
        // side it was navigated from (a later device enters from the right, an
        // earlier one from the left). Keyed by `selected` so it re-fires per
        // change; `relative`-inset `left` translates it visually without
        // disturbing the neighbouring peek slots.
        let fw = focused_frac;
        let fh = 0.92_f32;
        let fw0 = focused_frac * 0.72;
        let fh0 = fh * 0.74;
        let focused_slot = div()
            .flex_shrink_0()
            .overflow_hidden()
            .child(focused_el)
            .with_animation(
                ElementId::NamedInteger(format!("{id}-focus").into(), selected as u64),
                Animation::new(Duration::from_millis(240)).with_easing(ease_in_out),
                move |this, delta| {
                    this.w(relative(fw0 + (fw - fw0) * delta))
                        .h(relative(fh0 + (fh - fh0) * delta))
                        .opacity(0.65 + 0.35 * delta)
                        .left(px(slide_dir * 72. * (1. - delta)))
                },
            );

        let stage = h_flex()
            .id("carousel-stage")
            .w_full()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap(gap)
            .overflow_hidden()
            .when(multi, |this| {
                this.child(side_slot(
                    prev_el,
                    selected.saturating_sub(1),
                    side_frac,
                    on_select.clone(),
                ))
            })
            .child(focused_slot)
            .when(multi, |this| {
                this.child(side_slot(
                    next_el,
                    selected + 1,
                    side_frac,
                    on_select.clone(),
                ))
            });

        v_flex()
            .size_full()
            .gap_3()
            .child(stage)
            .when(multi, |this| {
                this.child(controls(
                    len,
                    selected,
                    arrows,
                    indicators,
                    accent,
                    dot_idle,
                    on_select.as_ref(),
                ))
            })
            .into_any_element()
    }
}

/// The bottom control row: prev/next arrows flanking the page-indicator dots.
fn controls(
    len: usize,
    selected: usize,
    arrows: bool,
    indicators: bool,
    accent: Hsla,
    idle: Hsla,
    on_select: Option<&SelectHandler>,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .justify_center()
        .gap_3()
        .when(arrows, |t| {
            t.child(arrow(
                "carousel-prev",
                IconName::ChevronLeft,
                selected.saturating_sub(1),
                selected == 0,
                Size::XSmall,
                on_select.cloned(),
            ))
        })
        .when(indicators, |t| {
            t.child(h_flex().items_center().gap_1p5().children(
                (0..len).map(|i| dot(i, i == selected, accent, idle, on_select.cloned())),
            ))
        })
        .when(arrows, |t| {
            t.child(arrow(
                "carousel-next",
                IconName::ChevronRight,
                (selected + 1).min(len - 1),
                selected + 1 >= len,
                Size::XSmall,
                on_select.cloned(),
            ))
        })
}

/// A side (peek) slot: the smaller neighbour, clickable to bring it to centre,
/// or an empty spacer at the ends so the focused item stays centred.
fn side_slot(
    el: Option<AnyElement>,
    index: usize,
    frac: f32,
    on_select: Option<SelectHandler>,
) -> AnyElement {
    let base = div()
        .flex_shrink_0()
        .w(relative(frac))
        .h(relative(0.62))
        .flex()
        .items_center()
        .justify_center();
    match el {
        Some(el) => base
            .id(("carousel-peek", index))
            .opacity(0.6)
            .cursor_pointer()
            .hover(|s| s.opacity(0.85))
            .when_some(on_select, |this, handler| {
                this.on_click(move |_, window, cx| handler(&index, window, cx))
            })
            .child(el)
            .into_any_element(),
        None => base.into_any_element(),
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
