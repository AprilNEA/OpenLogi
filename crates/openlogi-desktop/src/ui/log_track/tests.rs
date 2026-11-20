//! The track in a window: presses, drags, and keys reach the owner as the
//! value under the pointer and the dot they were for.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, FocusHandle, IntoElement, Modifiers, MouseButton, ParentElement as _, Pixels, Point,
    Render, Styled as _, TestAppContext, VisualTestContext, Window, div, point, px,
};

use super::geometry::LogScale;
use super::{LogTrack, Nudge, TRACK_INSET, TrackThumb};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Heard {
    Add(u16),
    Move(usize, u16),
    Select(usize),
    Nudge(usize, Nudge),
}

struct Harness {
    values: Vec<u16>,
    selected: Option<usize>,
    disabled: bool,
    focus: FocusHandle,
    heard: Rc<RefCell<Vec<Heard>>>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let log = |heard: &Rc<RefCell<Vec<Heard>>>| heard.clone();
        let (add, moved, select, nudge) = (
            log(&self.heard),
            log(&self.heard),
            log(&self.heard),
            log(&self.heard),
        );
        div().w(px(WIDTH)).p(px(0.)).child(
            LogTrack::new("test-track", 200_u16, 8200)
                .thumbs(self.values.iter().enumerate().map(|(index, value)| {
                    TrackThumb::new(*value, value.to_string())
                        .selected(self.selected == Some(index))
                }))
                .track_focus(&self.focus)
                .disabled(self.disabled)
                .on_add(move |value, _, _| add.borrow_mut().push(Heard::Add(value)))
                .on_move(move |index, value, _, _| {
                    moved.borrow_mut().push(Heard::Move(index, value));
                })
                .on_select(move |index, _, _| select.borrow_mut().push(Heard::Select(index)))
                .on_nudge(move |index, way, _, _| {
                    nudge.borrow_mut().push(Heard::Nudge(index, way));
                }),
        )
    }
}

const WIDTH: f32 = 524.;

fn open(
    cx: &mut TestAppContext,
    values: Vec<u16>,
    selected: Option<usize>,
    disabled: bool,
) -> (Rc<RefCell<Vec<Heard>>>, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let heard = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view({
        let heard = heard.clone();
        move |_, cx| Harness {
            values,
            selected,
            disabled,
            focus: cx.focus_handle().tab_stop(true),
            heard,
        }
    });
    redraw(cx);
    (heard, cx)
}

/// Draw the window, and again once the track has measured itself.
fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

/// The window point over `value` on the rail, at the height of the dots.
fn over(cx: &mut VisualTestContext, value: f32) -> Point<Pixels> {
    let rail = cx
        .debug_bounds("test-track-rail")
        .expect("the rail is drawn");
    let width = f32::from(rail.size.width);
    assert!(
        (width - (WIDTH - 2. * TRACK_INSET)).abs() < 0.5,
        "the rail spans the track between its insets"
    );
    let fraction = LogScale::new(200., 8200.).fraction(value);
    point(
        rail.left() + px(fraction * width),
        rail.top() + px(super::RAIL_Y),
    )
}

fn near(heard: Heard, expected: Heard) -> bool {
    match (heard, expected) {
        (Heard::Add(a), Heard::Add(b)) => a.abs_diff(b) <= 20,
        (Heard::Move(i, a), Heard::Move(j, b)) => i == j && a.abs_diff(b) <= 20,
        _ => heard == expected,
    }
}

#[gpui::test]
fn a_press_on_empty_track_adds_the_value_under_it(cx: &mut TestAppContext) {
    let (heard, cx) = open(cx, vec![400], None, false);

    let at = over(cx, 1600.);

    cx.simulate_click(at, Modifiers::none());

    let heard = heard.borrow();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(near(heard[0], Heard::Add(1600)), "{heard:?}");
}

#[gpui::test]
fn a_press_on_a_dot_selects_it_instead_of_adding(cx: &mut TestAppContext) {
    let (heard, cx) = open(cx, vec![400, 3200], None, false);
    let dot = cx
        .debug_bounds("test-track-thumb-1")
        .expect("the second dot is drawn");

    // Off its centre, but inside the area it answers in.
    cx.simulate_click(dot.center() + point(px(8.), px(0.)), Modifiers::none());

    assert_eq!(*heard.borrow(), [Heard::Select(1)]);
}

#[gpui::test]
fn dragging_a_dot_reports_the_value_under_the_pointer(cx: &mut TestAppContext) {
    let (heard, cx) = open(cx, vec![400, 3200], None, false);
    let from = over(cx, 400.);

    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for value in [500., 800.] {
        let at = over(cx, value);
        cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
    }
    let at = over(cx, 800.);
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());

    let heard = heard.borrow();
    assert_eq!(heard.first(), Some(&Heard::Select(0)), "{heard:?}");
    let moves: Vec<Heard> = heard[1..].to_vec();
    assert!(
        moves
            .last()
            .is_some_and(|last| near(*last, Heard::Move(0, 800))),
        "{moves:?}"
    );
    assert!(
        moves.iter().all(|heard| matches!(heard, Heard::Move(0, _))),
        "only the dragged dot moves, and nothing is added: {moves:?}"
    );
}

#[gpui::test]
fn the_arrow_keys_nudge_the_selected_dot_once_the_track_has_focus(cx: &mut TestAppContext) {
    let (heard, cx) = open(cx, vec![400, 3200], Some(1), false);
    let dot = cx.debug_bounds("test-track-thumb-1").expect("a dot");
    cx.simulate_click(dot.center(), Modifiers::none());

    cx.simulate_keystrokes("right left");

    assert_eq!(
        *heard.borrow(),
        [
            Heard::Select(1),
            Heard::Nudge(1, Nudge::Higher),
            Heard::Nudge(1, Nudge::Lower)
        ]
    );
}

#[gpui::test]
fn a_disabled_track_takes_no_input(cx: &mut TestAppContext) {
    let (heard, cx) = open(cx, vec![400], Some(0), true);

    let at = over(cx, 1600.);

    cx.simulate_click(at, Modifiers::none());
    let at = over(cx, 400.);
    cx.simulate_click(at, Modifiers::none());
    cx.simulate_keystrokes("right");

    assert!(heard.borrow().is_empty(), "{:?}", heard.borrow());
}

#[gpui::test]
fn dots_a_pixel_apart_can_each_be_pressed(cx: &mut TestAppContext) {
    // 8150 and 8200 sit about 1.4 px apart; each dot's own square would
    // cover the other entirely.
    let (heard, cx) = open(cx, vec![8150, 8200], None, false);
    let lower = over(cx, 8150.);
    let upper = over(cx, 8200.);
    assert!(f32::from(upper.x - lower.x) < 2., "{lower:?} {upper:?}");

    cx.simulate_click(lower - point(px(6.), px(0.)), Modifiers::none());
    cx.simulate_click(upper + point(px(6.), px(0.)), Modifiers::none());

    assert_eq!(*heard.borrow(), [Heard::Select(0), Heard::Select(1)]);
}

#[gpui::test]
fn a_press_on_a_value_label_selects_its_dot(cx: &mut TestAppContext) {
    // 850's label is pushed right of its dot, clear of 800's.
    let (heard, cx) = open(cx, vec![800, 850], None, false);
    let rail = cx.debug_bounds("test-track-rail").expect("the rail");
    let label_row = rail.top() + px(super::VALUE_ROW_H / 2.);

    let under_label = point(over(cx, 850.).x + px(16.), label_row);
    cx.simulate_click(under_label, Modifiers::none());
    // Off every label, the row does nothing.
    let clear = point(over(cx, 3200.).x, label_row);
    cx.simulate_click(clear, Modifiers::none());

    assert_eq!(*heard.borrow(), [Heard::Select(1)]);
}

#[gpui::test]
fn only_a_click_on_the_rail_adds_a_value(cx: &mut TestAppContext) {
    let (heard, cx) = open(cx, vec![400], None, false);
    let rail = cx.debug_bounds("test-track-rail").expect("the rail");
    let at = over(cx, 1600.);

    // On the ruler under the rail.
    let ruler = point(at.x, rail.top() + px(super::RULER_TOP + 6.));
    cx.simulate_click(ruler, Modifiers::none());
    // A press on the rail that is dragged away.
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    let away = at + point(px(40.), px(0.));
    cx.simulate_mouse_move(away, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(away, MouseButton::Left, Modifiers::none());
    assert!(heard.borrow().is_empty(), "{:?}", heard.borrow());

    cx.simulate_click(at, Modifiers::none());
    let heard = heard.borrow();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(near(heard[0], Heard::Add(1600)), "{heard:?}");
}

#[gpui::test]
fn the_keyboard_reaches_the_track_and_sees_it_focused(cx: &mut TestAppContext) {
    let (heard, cx) = open(cx, vec![400, 3200], Some(0), false);
    assert!(cx.debug_bounds("test-track-focus-ring").is_none());

    cx.update(Window::focus_next);
    redraw(cx);
    assert!(
        cx.debug_bounds("test-track-focus-ring").is_some(),
        "the focused track shows it"
    );
    cx.simulate_keystrokes("right");
    assert_eq!(*heard.borrow(), [Heard::Nudge(0, Nudge::Higher)]);
}

#[gpui::test]
fn a_pressed_dot_leaves_the_track_focused_and_showing_it(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, vec![400, 3200], Some(0), false);
    let dot = cx.debug_bounds("test-track-thumb-1").expect("a dot");

    cx.simulate_click(dot.center(), Modifiers::none());
    redraw(cx);

    assert!(cx.debug_bounds("test-track-focus-ring").is_some());
}
