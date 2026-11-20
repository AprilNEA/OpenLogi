//! A horizontal track that holds several values on a logarithmic scale, each
//! one a dot that can be dragged, with a labelled ruler underneath.
//!
//! GPUI's slider holds one value, or a range of two; this track holds any
//! number. It owns no values: the owner hands it the dots on every render and
//! hears back through callbacks — a click on empty track to add a value, a
//! drag or an arrow key to move one, a press on a dot to select it, and a
//! right-click for the owner's menu of that dot. Where values land, which
//! ruler labels fit, and how value labels make room for each other is the
//! pure [`geometry`] the tests pin down.
//!
//! Dots are numbered by their place along the track. A drag keeps that place
//! as long as the owner keeps its values in order, which the owner decides.
//!
//! Each dot answers the pointer up to halfway to its neighbours, so dots a
//! pixel apart can still each be caught; that needs the width the track was
//! last painted at, which the track keeps between frames. A click adds a
//! value only on the band of the rail itself, and only when the pointer was
//! not dragged; a press on a dot's value label selects that dot.

pub(crate) mod geometry;

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, Div, DragMoveEvent, ElementId,
    Empty, Entity, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, Refineable as _, Render, RenderOnce, Role,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, TextAlign, TextStyle,
    Window, canvas, div, fill, point, prelude::FluentBuilder as _, px, relative, size,
};
use gpui_kit::component::{ActiveTheme as _, menu::ContextMenuExt as _, menu::PopupMenu};

pub(crate) use self::geometry::doubling_ticks;
use self::geometry::{Label, LogScale, hit_spans, label_at, ruler_labels, spread_labels};
use super::commit_slider::SliderUnit;
use super::theme::{self, Typography as _};

/// Height of the row of value labels above the dots.
const VALUE_ROW_H: f32 = 18.;
/// Distance from the top of the track to the centre of the rail and dots.
const RAIL_Y: f32 = 30.;
const RAIL_H: f32 = 4.;
/// The visible dot.
const DOT: f32 = 14.;
/// The square a dot answers the pointer in: larger than the dot, so a dot
/// is easy to catch.
const DOT_HIT: f32 = 24.;
/// The ring around a selected dot.
const RING: f32 = DOT + 8.;
/// The bar under the dot the device is using now, clear of the ring above
/// it and the ruler below it.
const MARK_W: f32 = 10.;
const MARK_H: f32 = 3.;
const MARK_TOP: f32 = RAIL_Y + RING / 2. + 2.;
const TICK_TOP: f32 = MARK_TOP + MARK_H + 4.;
const TICK_H: f32 = 5.;
const RULER_TOP: f32 = TICK_TOP + TICK_H + 1.;
const TRACK_H: f32 = RULER_TOP + 18.;
/// How far the pointer may move between press and release for a press on
/// empty rail to still add a value.
const CLICK_SLOP: f32 = 4.;
/// Room left and right of the rail for the dots and labels at its ends.
pub(crate) const TRACK_INSET: f32 = DOT_HIT / 2.;
/// The least space between two labels.
const LABEL_GAP: f32 = 6.;

/// Which way an arrow key moves the selected dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nudge {
    Lower,
    Higher,
}

/// One dot on the track.
#[derive(Debug, Clone)]
pub(crate) struct TrackThumb<T> {
    value: T,
    label: SharedString,
    filled: bool,
    selected: bool,
    marked: bool,
}

impl<T> TrackThumb<T> {
    /// An outlined dot at `value`, with `label` above it.
    pub(crate) fn new(value: T, label: impl Into<SharedString>) -> Self {
        Self {
            value,
            label: label.into(),
            filled: false,
            selected: false,
            marked: false,
        }
    }

    /// Fill the dot, setting it apart from the outlined ones.
    pub(crate) fn filled(mut self, filled: bool) -> Self {
        self.filled = filled;
        self
    }

    /// Ring the dot as the one the arrow keys move.
    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Put a small mark under the dot.
    pub(crate) fn marked(mut self, marked: bool) -> Self {
        self.marked = marked;
        self
    }
}

type ValueHandler<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;
type ThumbValueHandler<T> = Rc<dyn Fn(usize, T, &mut Window, &mut App)>;
type ThumbHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
type NudgeHandler = Rc<dyn Fn(usize, Nudge, &mut Window, &mut App)>;
type ThumbMenu = Rc<dyn Fn(usize, PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// A filled dot, as the track draws one: for a legend that explains it.
pub(crate) fn filled_dot(color: gpui::Hsla) -> Div {
    div().size(px(DOT)).rounded_full().bg(color)
}

/// The bar the track draws under a marked dot: for a legend that explains
/// it.
pub(crate) fn mark_swatch(color: gpui::Hsla) -> Div {
    div().w(px(MARK_W)).h(px(MARK_H)).rounded_full().bg(color)
}

/// What the track measured when it was last painted, for the pointer to
/// find the dots and labels on the next frame.
#[derive(Debug, Default)]
struct TrackMetrics {
    /// The rail, in window coordinates.
    rail: Bounds<Pixels>,
    /// Each dot's value label, as left and right pixels along the rail.
    value_labels: Vec<(f32, f32)>,
}

/// A multi-thumb track on a logarithmic scale.
#[derive(IntoElement)]
pub(crate) struct LogTrack<T: SliderUnit> {
    id: SharedString,
    scale: LogScale,
    ticks: Vec<(T, SharedString)>,
    thumbs: Vec<TrackThumb<T>>,
    focus: Option<FocusHandle>,
    disabled: bool,
    accessibility_label: Option<SharedString>,
    on_add: Option<ValueHandler<T>>,
    on_move: Option<ThumbValueHandler<T>>,
    on_select: Option<ThumbHandler>,
    on_nudge: Option<NudgeHandler>,
    thumb_menu: Option<ThumbMenu>,
}

impl<T: SliderUnit> LogTrack<T> {
    /// A track from `min` to `max`. `id` names the track's elements and its
    /// debug selectors: `{id}-rail` and `{id}-thumb-{index}`.
    pub(crate) fn new(id: impl Into<SharedString>, min: T, max: T) -> Self {
        Self {
            id: id.into(),
            scale: LogScale::new(min.to_slider(), max.to_slider()),
            ticks: Vec::new(),
            thumbs: Vec::new(),
            focus: None,
            disabled: false,
            accessibility_label: None,
            on_add: None,
            on_move: None,
            on_select: None,
            on_nudge: None,
            thumb_menu: None,
        }
    }

    /// The ruler: a tick with a label at each value. Labels that would
    /// overlap at the track's width are left out, never the ends.
    pub(crate) fn ticks(mut self, ticks: impl IntoIterator<Item = (T, SharedString)>) -> Self {
        self.ticks = ticks.into_iter().collect();
        self
    }

    /// The dots, in track order.
    pub(crate) fn thumbs(mut self, thumbs: impl IntoIterator<Item = TrackThumb<T>>) -> Self {
        self.thumbs = thumbs.into_iter().collect();
        self
    }

    /// The focus the track takes when a dot or the track is pressed, and
    /// that the arrow keys need. The track draws a ring while it has it;
    /// make the handle a tab stop for the keyboard to reach the track.
    pub(crate) fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    /// Take no pointer or key input, and draw the dots muted.
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(crate) fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// A click on the rail away from every dot, with the value under it.
    pub(crate) fn on_add(mut self, handler: impl Fn(T, &mut Window, &mut App) + 'static) -> Self {
        self.on_add = Some(Rc::new(handler));
        self
    }

    /// Every pointer move while a dot is dragged, with the value under the
    /// pointer.
    pub(crate) fn on_move(
        mut self,
        handler: impl Fn(usize, T, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_move = Some(Rc::new(handler));
        self
    }

    /// A press on a dot, with either button, or on its value label.
    pub(crate) fn on_select(
        mut self,
        handler: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// Left or Right while the track has focus, for the selected dot.
    pub(crate) fn on_nudge(
        mut self,
        handler: impl Fn(usize, Nudge, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_nudge = Some(Rc::new(handler));
        self
    }

    /// The menu a right-click on a dot opens.
    pub(crate) fn thumb_menu(
        mut self,
        menu: impl Fn(usize, PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        self.thumb_menu = Some(Rc::new(menu));
        self
    }
}

/// The payload of a dot being dragged: which track, and which dot on it.
#[derive(Clone)]
struct DraggedThumb {
    track: SharedString,
    index: usize,
}

impl Render for DraggedThumb {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl<T: SliderUnit> RenderOnce for LogTrack<T> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let accent = if self.disabled {
            pal.text_muted
        } else {
            cx.theme().primary
        };
        let metrics = window.use_keyed_state(
            ElementId::Name(format!("{}-metrics", self.id).into()),
            cx,
            |_, _| TrackMetrics::default(),
        );
        let width = f32::from(metrics.read(cx).rail.size.width);
        let focused = self
            .focus
            .as_ref()
            .is_some_and(|focus| focus.is_focused(window));
        let painter = self.painter(pal, window);
        let dots = self
            .thumbs
            .iter()
            .enumerate()
            .map(|(index, thumb)| self.dot(index, thumb, accent, pal));
        let id = self.id.clone();
        let rail_selector = format!("{id}-rail");
        let focus_selector = format!("{id}-focus-ring");
        let rail = div()
            .id(id)
            .debug_selector(move || rail_selector)
            .role(Role::Group)
            .when_some(self.accessibility_label.clone(), |this, label| {
                this.aria_label(label)
            })
            .relative()
            .w_full()
            .h(px(TRACK_H))
            .when_some(self.focus.clone(), |this, focus| this.track_focus(&focus))
            .when(focused && !self.disabled, |this| {
                this.child(
                    div()
                        .debug_selector(move || focus_selector)
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(-TRACK_INSET))
                        .right(px(-TRACK_INSET))
                        .rounded_md()
                        .border_2()
                        .border_color(pal.ring),
                )
            })
            .child(
                canvas(
                    {
                        let metrics = metrics.clone();
                        move |bounds, window, cx| painter.layout(bounds, &metrics, window, cx)
                    },
                    move |bounds, painted, window, cx| painted.paint(bounds, window, cx),
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .children(dots);
        let rail = if self.disabled {
            rail
        } else {
            let band = self.band(&metrics);
            let hits = self.hit_areas(width);
            self.listen(rail, &metrics).child(band).children(hits)
        };
        div().w_full().px(px(TRACK_INSET)).child(rail)
    }
}

impl<T: SliderUnit> LogTrack<T> {
    /// What the canvas paints, in this track's colours.
    fn painter(&self, pal: theme::Palette, window: &Window) -> TrackPainter {
        TrackPainter {
            scale: self.scale,
            ticks: self
                .ticks
                .iter()
                .map(|(value, label)| (value.to_slider(), label.clone()))
                .collect(),
            values: self
                .thumbs
                .iter()
                .map(|thumb| (thumb.value.to_slider(), thumb.label.clone()))
                .collect(),
            rail: pal.text_muted.opacity(0.3),
            tick: pal.text_muted.opacity(0.6),
            ruler_text: pal.text_muted,
            value_text: if self.disabled {
                pal.text_muted
            } else {
                pal.text_primary
            },
            caption: caption_style(window),
        }
    }

    /// The rail's own input: any press focuses the track, a press on a
    /// value label selects its dot, a dot's drag moves it, and the arrow
    /// keys nudge the selected dot.
    fn listen(&self, rail: Stateful<Div>, metrics: &Entity<TrackMetrics>) -> Stateful<Div> {
        let scale = self.scale;
        let selected = self.thumbs.iter().position(|thumb| thumb.selected);
        let focus = self.focus.clone();
        let on_select = self.on_select.clone();
        let on_move = self.on_move.clone();
        let on_nudge = self.on_nudge.clone();
        let track = self.id.clone();
        let metrics = metrics.clone();
        rail.on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, window, cx| {
                if let Some(focus) = &focus {
                    focus.focus(window, cx);
                }
                let measured = metrics.read(cx);
                let y = f32::from(event.position.y - measured.rail.top());
                if y >= VALUE_ROW_H {
                    return;
                }
                let x = f32::from(event.position.x - measured.rail.left());
                if let (Some(index), Some(on_select)) =
                    (label_at(x, &measured.value_labels), &on_select)
                {
                    on_select(index, window, cx);
                }
            },
        )
        .on_drag_move(move |event: &DragMoveEvent<DraggedThumb>, window, cx| {
            let dragged = event.drag(cx).clone();
            if dragged.track != track {
                return;
            }
            let x = f32::from(event.event.position.x - event.bounds.left());
            let value = scale.value_at_x(x, f32::from(event.bounds.size.width));
            if let Some(on_move) = &on_move {
                on_move(dragged.index, T::from_slider(value), window, cx);
            }
        })
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            let nudge = match event.keystroke.key.as_str() {
                "left" => Nudge::Lower,
                "right" => Nudge::Higher,
                _ => return,
            };
            if let (Some(index), Some(on_nudge)) = (selected, &on_nudge) {
                cx.stop_propagation();
                on_nudge(index, nudge, window, cx);
            }
        })
    }

    /// The band along the rail, level with the dots and under them, where a
    /// click adds the value under the press. A press that turns into a drag
    /// adds nothing.
    fn band(&self, metrics: &Entity<TrackMetrics>) -> Stateful<Div> {
        let scale = self.scale;
        let on_add = self.on_add.clone();
        let metrics = metrics.clone();
        div()
            .id(ElementId::Name(format!("{}-band", self.id).into()))
            .absolute()
            .top(px(RAIL_Y - DOT_HIT / 2.))
            .h(px(DOT_HIT))
            .left(px(-TRACK_INSET))
            .right(px(-TRACK_INSET))
            .on_click(move |event: &ClickEvent, window, cx| {
                let ClickEvent::Mouse(click) = event else {
                    return;
                };
                let (down, up) = (click.down.position, click.up.position);
                let moved = (f32::from(up.x - down.x).abs()).max(f32::from(up.y - down.y).abs());
                if moved > CLICK_SLOP {
                    return;
                }
                let rail = metrics.read(cx).rail;
                let x = f32::from(down.x - rail.left());
                let value = scale.value_at_x(x, f32::from(rail.size.width));
                if let Some(on_add) = &on_add {
                    on_add(T::from_slider(value), window, cx);
                }
            })
    }

    /// Dot `index` as it is drawn, centred on its value: the dot, its
    /// selection ring, and its mark. It takes no input itself.
    fn dot(
        &self,
        index: usize,
        thumb: &TrackThumb<T>,
        accent: gpui::Hsla,
        pal: theme::Palette,
    ) -> Div {
        let fraction = self.scale.fraction(thumb.value.to_slider());
        let visual = div()
            .size(px(DOT))
            .rounded_full()
            .border_2()
            .border_color(accent)
            .bg(if thumb.filled { accent } else { pal.panel });
        let ring = thumb.selected.then(|| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .size(px(RING))
                .rounded_full()
                .border_2()
                .border_color(pal.ring.opacity(0.6))
        });
        let mark_selector = format!("{}-thumb-{index}-mark", self.id);
        let mark = thumb.marked.then(|| {
            mark_swatch(accent)
                .debug_selector(move || mark_selector)
                .absolute()
                .top(px(MARK_TOP - (RAIL_Y - RING / 2.)))
                .left(px((RING - MARK_W) / 2.))
        });
        div()
            .absolute()
            .top(px(RAIL_Y - RING / 2.))
            .left(relative(fraction))
            .ml(px(-RING / 2.))
            .size(px(RING))
            .flex()
            .items_center()
            .justify_center()
            .children(ring)
            .child(visual)
            .children(mark)
    }

    /// Where each dot answers the pointer: up to [`DOT_HIT`] wide around
    /// it, and never past halfway to a neighbour, so every dot can be
    /// caught however close. Before the track has been painted its width
    /// is unknown, and each dot answers in the whole square around it.
    fn hit_areas(&self, width: f32) -> Vec<AnyElement> {
        let fractions: Vec<f32> = self
            .thumbs
            .iter()
            .map(|thumb| self.scale.fraction(thumb.value.to_slider()))
            .collect();
        let anchors: Vec<f32> = fractions.iter().map(|fraction| fraction * width).collect();
        let spans = hit_spans(&anchors, DOT_HIT / 2.);
        self.thumbs
            .iter()
            .enumerate()
            .map(|(index, thumb)| {
                let area = div()
                    .id((self.id.clone(), index))
                    .absolute()
                    .top(px(RAIL_Y - DOT_HIT / 2.))
                    .h(px(DOT_HIT));
                let area = if width > 0. {
                    let (left, right) = spans[index];
                    area.left(px(left)).w(px(right - left))
                } else {
                    area.left(relative(fractions[index]))
                        .ml(px(-DOT_HIT / 2.))
                        .w(px(DOT_HIT))
                };
                self.hit_area(index, thumb, area)
            })
            .collect()
    }

    /// The input of dot `index` on its hit `area`: a press selects it, a
    /// drag moves it, and a right-click opens its menu.
    fn hit_area(&self, index: usize, thumb: &TrackThumb<T>, area: Stateful<Div>) -> AnyElement {
        let selector = format!("{}-thumb-{index}", self.id);
        let focus = self.focus.clone();
        let on_select = self.on_select.clone();
        let select = move |window: &mut Window, cx: &mut App| {
            if let Some(focus) = &focus {
                focus.focus(window, cx);
            }
            if let Some(on_select) = &on_select {
                on_select(index, window, cx);
            }
        };
        let right_select = select.clone();
        let dragged = DraggedThumb {
            track: self.id.clone(),
            index,
        };
        let element = area
            .debug_selector(move || selector)
            .role(Role::Slider)
            .aria_label(thumb.label.clone())
            .aria_numeric_value(f64::from(thumb.value.to_slider()))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                // The band under the dot would take this press as an add.
                cx.stop_propagation();
                select(window, cx);
            })
            // The menu opens after this, from the same press.
            .on_mouse_down(MouseButton::Right, move |_, window, cx| {
                right_select(window, cx);
            })
            .on_drag(dragged, |dragged, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| dragged.clone())
            });
        match self.thumb_menu.clone() {
            Some(menu) => element
                .context_menu(move |popup, window, cx| menu(index, popup, window, cx))
                .into_any_element(),
            None => element.into_any_element(),
        }
    }
}

/// The caption text style the ruler and value labels are painted in.
fn caption_style(window: &Window) -> TextStyle {
    let mut style = window.text_style();
    style.refine(div().text_caption().text_style());
    style
}

/// What the track paints: the rail, the ruler, and the value labels.
struct TrackPainter {
    scale: LogScale,
    ticks: Vec<(f32, SharedString)>,
    values: Vec<(f32, SharedString)>,
    rail: gpui::Hsla,
    tick: gpui::Hsla,
    ruler_text: gpui::Hsla,
    value_text: gpui::Hsla,
    caption: TextStyle,
}

/// A label shaped and placed, ready to paint.
struct PlacedLabel {
    line: gpui::ShapedLine,
    left: f32,
    top: f32,
}

/// The track laid out at its painted width.
struct PaintedTrack {
    rail: gpui::Hsla,
    tick: gpui::Hsla,
    ticks: Vec<f32>,
    labels: Vec<PlacedLabel>,
    line_height: Pixels,
}

impl TrackPainter {
    /// Lay the track out at `bounds`, and keep what the pointer needs of
    /// it in `metrics`, redrawing once when the rail's width has changed.
    fn layout(
        self,
        bounds: Bounds<Pixels>,
        metrics: &Entity<TrackMetrics>,
        window: &mut Window,
        cx: &mut App,
    ) -> PaintedTrack {
        let width = f32::from(bounds.size.width);
        let rem = window.rem_size();
        let font_size = self.caption.font_size.to_pixels(rem);
        let line_height = self.caption.line_height_in_pixels(rem);
        let shape = |text: &SharedString, color: gpui::Hsla, window: &mut Window| {
            let mut style = self.caption.clone();
            style.color = color;
            window.text_system().shape_line(
                text.clone(),
                font_size,
                &[style.to_run(text.len())],
                None,
            )
        };
        let anchor = |value: f32| self.scale.fraction(value) * width;

        let ruler: Vec<gpui::ShapedLine> = self
            .ticks
            .iter()
            .map(|(_, label)| shape(label, self.ruler_text, window))
            .collect();
        let ruler_spans: Vec<Label> = self
            .ticks
            .iter()
            .zip(&ruler)
            .map(|((value, _), line)| Label {
                anchor: anchor(*value),
                width: f32::from(line.width),
            })
            .collect();
        let shown = ruler_labels(&ruler_spans, width, LABEL_GAP);
        let mut labels: Vec<PlacedLabel> = ruler
            .into_iter()
            .zip(&ruler_spans)
            .zip(shown)
            .filter(|(_, shown)| *shown)
            .map(|((line, span), _)| PlacedLabel {
                line,
                left: span.left(width),
                top: RULER_TOP,
            })
            .collect();

        let values: Vec<gpui::ShapedLine> = self
            .values
            .iter()
            .map(|(_, label)| shape(label, self.value_text, window))
            .collect();
        let value_spans: Vec<Label> = self
            .values
            .iter()
            .zip(&values)
            .map(|((value, _), line)| Label {
                anchor: anchor(*value),
                width: f32::from(line.width),
            })
            .collect();
        let lefts = spread_labels(&value_spans, width, LABEL_GAP);
        let value_labels: Vec<(f32, f32)> = lefts
            .iter()
            .zip(&value_spans)
            .map(|(left, span)| (*left, left + span.width))
            .collect();
        metrics.update(cx, |metrics, cx| {
            let resized = metrics.rail.size != bounds.size;
            metrics.rail = bounds;
            metrics.value_labels = value_labels;
            if resized {
                // The dots' hit areas were laid out for another width.
                cx.notify();
            }
        });
        labels.extend(
            values
                .into_iter()
                .zip(lefts)
                .map(|(line, left)| PlacedLabel {
                    line,
                    left,
                    top: (VALUE_ROW_H - f32::from(line_height)).max(0.) / 2.,
                }),
        );
        PaintedTrack {
            rail: self.rail,
            tick: self.tick,
            ticks: self.ticks.iter().map(|(value, _)| anchor(*value)).collect(),
            labels,
            line_height,
        }
    }
}

impl PaintedTrack {
    fn paint(self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let origin = bounds.origin;
        window.paint_quad(
            fill(
                Bounds::new(
                    point(origin.x, origin.y + px(RAIL_Y - RAIL_H / 2.)),
                    size(bounds.size.width, px(RAIL_H)),
                ),
                self.rail,
            )
            .corner_radii(px(RAIL_H / 2.)),
        );
        for x in self.ticks {
            window.paint_quad(fill(
                Bounds::new(
                    point(origin.x + px(x - 0.5), origin.y + px(TICK_TOP)),
                    size(px(1.), px(TICK_H)),
                ),
                self.tick,
            ));
        }
        for label in self.labels {
            let at = point(origin.x + px(label.left), origin.y + px(label.top));
            // A label that cannot be painted leaves a gap, nothing worse.
            label
                .line
                .paint(at, self.line_height, TextAlign::Left, None, window, cx)
                .ok();
        }
    }
}

#[cfg(test)]
mod tests;
