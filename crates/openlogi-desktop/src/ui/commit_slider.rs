//! The write-on-release slider the settings panels share.
//!
//! A [`SliderState`] reports a value on every drag frame, which is far more
//! often than a device or `config.toml` should be written. So a panel shows
//! the live value while the thumb moves, commits once on release, and re-seats
//! the thumb when the committed value changes underneath it — never mid-drag.
//! [`CommitSlider`] is that behaviour and the state it needs; the panel still
//! renders the stock `Slider` from [`CommitSlider::slider`].

use std::cell::Cell;
use std::rc::Rc;

use gpui::{App, AppContext as _, Context, Entity, Subscription, Window};
use gpui_component::slider::{SliderEvent, SliderState};
use openlogi_core::config::{ThumbwheelSensitivity, VerticalScrollSensitivity};
use openlogi_core::hid::{Dpi, SmartShiftThreshold};

/// A value a slider thumb can rest on.
pub(crate) trait SliderUnit: Copy + PartialEq + 'static {
    /// The value in slider units.
    fn to_slider(self) -> f32;

    /// The value nearest to a raw thumb position.
    fn from_slider(raw: f32) -> Self;
}

impl SliderUnit for u8 {
    fn to_slider(self) -> f32 {
        f32::from(self)
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is rounded and clamped into the u8 range before the cast"
    )]
    fn from_slider(raw: f32) -> Self {
        raw.clamp(0., f32::from(Self::MAX)).round() as Self
    }
}

impl SliderUnit for u16 {
    fn to_slider(self) -> f32 {
        f32::from(self)
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is rounded and clamped into the u16 range before the cast"
    )]
    fn from_slider(raw: f32) -> Self {
        raw.clamp(0., f32::from(Self::MAX)).round() as Self
    }
}

impl SliderUnit for i32 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a UVC control range is far below f32's exact integer range"
    )]
    fn to_slider(self) -> f32 {
        self as f32
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the thumb stays inside a range that was built from i32 bounds"
    )]
    fn from_slider(raw: f32) -> Self {
        raw.round() as Self
    }
}

/// Domain values that already round a control position into their own range.
macro_rules! rounded_slider_unit {
    ($($unit:ty),+ $(,)?) => {$(
        impl SliderUnit for $unit {
            fn to_slider(self) -> f32 {
                f32::from(self)
            }

            fn from_slider(raw: f32) -> Self {
                Self::from_rounded(raw)
            }
        }
    )+};
}

rounded_slider_unit!(
    Dpi,
    SmartShiftThreshold,
    ThumbwheelSensitivity,
    VerticalScrollSensitivity,
);

/// The bounds and step of one slider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SliderRange<T> {
    min: T,
    max: T,
    step: f32,
}

impl<T: SliderUnit> SliderRange<T> {
    /// The range between `a` and `b`, in whichever order, stepping by one unit.
    ///
    /// A device reports these bounds, and one that reports them the wrong way
    /// round must still get a slider: `SliderState` re-clamps its value on
    /// every builder call and panics the moment `min > max`.
    pub(crate) fn new(a: T, b: T) -> Self {
        let (min, max) = if a.to_slider() <= b.to_slider() {
            (a, b)
        } else {
            (b, a)
        };
        Self { min, max, step: 1. }
    }

    /// Step by `step` slider units instead of one.
    pub(crate) fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    /// A `SliderState` over this range with the thumb on `initial`.
    ///
    /// `SliderState` starts as `0..=100` and clamps against the bound it has
    /// not been given yet, so the bound that cannot cross the default goes
    /// first: the maximum unless the whole range is negative.
    fn state(self, initial: T) -> SliderState {
        let (min, max) = (self.min.to_slider(), self.max.to_slider());
        let bounded = if max >= 0. {
            SliderState::new().max(max).min(min)
        } else {
            SliderState::new().min(min).max(max)
        };
        bounded.step(self.step).default_value(initial.to_slider())
    }

    /// The value under a raw thumb position, which never leaves the range.
    fn value_at(self, raw: f32) -> T {
        T::from_slider(raw.clamp(self.min.to_slider(), self.max.to_slider()))
    }

    /// `value`, pulled inside the range.
    pub(crate) fn clamp(self, value: T) -> T {
        let raw = value.to_slider();
        if raw < self.min.to_slider() {
            self.min
        } else if raw > self.max.to_slider() {
            self.max
        } else {
            value
        }
    }
}

/// Where the thumb rests and whether it is being dragged.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Thumb<T> {
    /// The value the thumb was last put on: by a release, or by a re-seat.
    seated: T,
    /// The value under the thumb while a drag is in progress.
    dragged: Option<T>,
}

impl<T: Copy + PartialEq> Thumb<T> {
    fn seated_on(value: T) -> Self {
        Self {
            seated: value,
            dragged: None,
        }
    }

    fn drag(self, value: T) -> Self {
        Self {
            dragged: Some(value),
            ..self
        }
    }

    /// The value to move the thumb to so that it shows `committed`, or `None`
    /// when it already does or a drag is in progress.
    fn reseat(self, committed: T) -> Option<T> {
        (self.dragged.is_none() && self.seated != committed).then_some(committed)
    }

    fn shown(self, committed: T) -> T {
        self.dragged.unwrap_or(committed)
    }
}

/// A slider that commits once, on release.
pub(crate) struct CommitSlider<T> {
    state: Entity<SliderState>,
    range: SliderRange<T>,
    thumb: Rc<Cell<Thumb<T>>>,
    _subscription: Subscription,
}

impl<T: SliderUnit> CommitSlider<T> {
    /// A slider over `range` resting on `initial`. `on_commit` runs once per
    /// release with the value the thumb was dropped on; the owning view is
    /// repainted on every drag frame and on release.
    pub(crate) fn new<V: 'static>(
        range: SliderRange<T>,
        initial: T,
        cx: &mut Context<V>,
        on_commit: impl Fn(&mut V, T, &mut Context<V>) + 'static,
    ) -> Self {
        Self::previewing(range, initial, cx, |_, _, _| {}, on_commit)
    }

    /// [`Self::new`] for a value that something outside the owning view shows
    /// while it is dragged: `on_preview` runs on every drag frame.
    pub(crate) fn previewing<V: 'static>(
        range: SliderRange<T>,
        initial: T,
        cx: &mut Context<V>,
        on_preview: impl Fn(&mut V, T, &mut Context<V>) + 'static,
        on_commit: impl Fn(&mut V, T, &mut Context<V>) + 'static,
    ) -> Self {
        let state = cx.new(|_| range.state(initial));
        let thumb = Rc::new(Cell::new(Thumb::seated_on(initial)));
        let subscription = cx.subscribe(&state, {
            let thumb = thumb.clone();
            move |owner, _, event: &SliderEvent, cx| {
                match event {
                    SliderEvent::Change(value) => {
                        let value = range.value_at(value.start());
                        thumb.set(thumb.get().drag(value));
                        on_preview(owner, value, cx);
                    }
                    SliderEvent::Release(value) => {
                        let value = range.value_at(value.start());
                        thumb.set(Thumb::seated_on(value));
                        on_commit(owner, value, cx);
                    }
                }
                cx.notify();
            }
        });
        Self {
            state,
            range,
            thumb,
            _subscription: subscription,
        }
    }

    /// The state to render a `Slider` from.
    pub(crate) fn slider(&self) -> &Entity<SliderState> {
        &self.state
    }

    /// The value under the thumb right now.
    pub(crate) fn value(&self, cx: &App) -> T {
        self.range.value_at(self.state.read(cx).value().start())
    }

    /// What to show for this slider: the value being dragged, else `committed`.
    pub(crate) fn shown(&self, committed: T) -> T {
        self.thumb.get().shown(committed)
    }

    /// The value the thumb was last released or seated on.
    pub(crate) fn seated(&self) -> T {
        self.thumb.get().seated
    }

    /// Follow `committed` when it changed underneath the thumb — a device
    /// switch, a re-read, a write that was rolled back — but never mid-drag.
    pub(crate) fn sync(&self, committed: T, window: &mut Window, cx: &mut App) {
        if let Some(value) = self.thumb.get().reseat(committed) {
            self.seat(value, window, cx);
        }
    }

    /// [`Self::sync`] for a slider whose stops are coarser than its step: the
    /// thumb only moves when it resolves, through `snap`, to a value other
    /// than `committed`, so a thumb dropped between two stops is not yanked
    /// back every frame — and, like [`Self::sync`], never mid-drag.
    pub(crate) fn sync_snapped(
        &self,
        committed: T,
        snap: impl Fn(T) -> T,
        window: &mut Window,
        cx: &mut App,
    ) {
        if self.thumb.get().dragged.is_none() && snap(self.value(cx)) != committed {
            self.seat(committed, window, cx);
        }
    }

    /// Put the thumb on `value`, whatever it was doing.
    pub(crate) fn seat(&self, value: T, window: &mut Window, cx: &mut App) {
        self.thumb.set(Thumb::seated_on(value));
        self.state.update(cx, |state, cx| {
            state.set_value(value.to_slider(), window, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{Context, Empty, IntoElement, Render, TestAppContext, Window};
    use gpui_component::slider::SliderEvent;

    use super::{CommitSlider, SliderRange, Thumb};

    /// `SliderState` panics when its bounds cross, even between two builder
    /// calls. Every shape a device has reported — and the ones it should not —
    /// must come out as an ordered slider.
    #[test]
    fn a_range_builds_an_ordered_slider_whatever_order_its_bounds_come_in() {
        for (a, b, min, max) in [
            (0, 100, 0., 100.),
            (100, 0, 0., 100.),
            // Above the default maximum: setting the minimum first would cross it.
            (200, 6400, 200., 6400.),
            // Entirely negative (UVC exposure): setting the maximum first would.
            (-11, -2, -11., -2.),
            (-2, -11, -11., -2.),
            (-5, 10, -5., 10.),
            (7, 7, 7., 7.),
        ] {
            let state = SliderRange::new(a, b).state(a);

            assert_eq!(
                (state.min_value(), state.max_value()),
                (min, max),
                "{a}..{b}"
            );
        }
    }

    #[test]
    fn a_thumb_value_never_leaves_its_range() {
        let range = SliderRange::new(10_u8, 20);

        assert_eq!(range.value_at(3.), 10);
        assert_eq!(range.value_at(14.6), 15);
        assert_eq!(range.value_at(250.), 20);
    }

    /// A UVC control can report its bounds the wrong way round; a value is
    /// clamped between them, not to the first one.
    #[test]
    fn a_clamp_uses_the_ordered_bounds() {
        let reversed = SliderRange::new(-2_i32, -11);

        assert_eq!(reversed.clamp(-20), -11);
        assert_eq!(reversed.clamp(-5), -5);
        assert_eq!(reversed.clamp(3), -2);
    }

    #[test]
    fn the_thumb_follows_the_committed_value_except_while_dragged() {
        let resting = Thumb::seated_on(40);
        let dragging = resting.drag(55);

        // (thumb, committed) -> (re-seat to, shown)
        for (thumb, committed, reseat, shown) in [
            (resting, 40, None, 40),
            (resting, 70, Some(70), 70),
            (dragging, 40, None, 55),
            (dragging, 70, None, 55),
        ] {
            assert_eq!(thumb.reseat(committed), reseat, "{thumb:?} / {committed}");
            assert_eq!(thumb.shown(committed), shown, "{thumb:?} / {committed}");
        }
    }

    struct Owner {
        slider: CommitSlider<u8>,
        previews: Rc<RefCell<Vec<u8>>>,
        commits: Rc<RefCell<Vec<u8>>>,
    }

    impl Owner {
        fn new(cx: &mut Context<Self>) -> Self {
            let previews = Rc::new(RefCell::new(Vec::new()));
            let commits = Rc::new(RefCell::new(Vec::new()));
            let slider = CommitSlider::previewing(
                SliderRange::new(0, 100),
                40,
                cx,
                {
                    let previews = previews.clone();
                    move |_, value, _| previews.borrow_mut().push(value)
                },
                {
                    let commits = commits.clone();
                    move |_, value, _| commits.borrow_mut().push(value)
                },
            );
            Self {
                slider,
                previews,
                commits,
            }
        }
    }

    impl Render for Owner {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            Empty
        }
    }

    #[gpui::test]
    fn a_drag_previews_every_frame_and_commits_once_on_release(cx: &mut TestAppContext) {
        let (owner, cx) = cx.add_window_view(|_, cx| Owner::new(cx));
        let slider = owner.read_with(cx, |owner, _| owner.slider.slider().clone());

        for value in [45., 50.] {
            slider.update(cx, |_, cx| cx.emit(SliderEvent::Change(value.into())));
        }
        cx.run_until_parked();

        owner.read_with(cx, |owner, _| {
            assert_eq!(*owner.previews.borrow(), [45, 50]);
            assert!(owner.commits.borrow().is_empty());
            assert_eq!(owner.slider.shown(40), 50, "the dragged value wins");
        });

        slider.update(cx, |_, cx| cx.emit(SliderEvent::Release(50.0.into())));
        cx.run_until_parked();

        owner.read_with(cx, |owner, _| {
            assert_eq!(*owner.commits.borrow(), [50]);
            assert_eq!(owner.slider.seated(), 50);
            assert_eq!(owner.slider.shown(40), 40, "the committed value is back");
        });
    }

    /// The write behind a release can be refused or rolled back. The thumb
    /// then has to return to the value that is actually committed instead of
    /// resting on the one that was dropped.
    #[gpui::test]
    fn a_rejected_release_is_reseated_on_the_committed_value(cx: &mut TestAppContext) {
        let (owner, cx) = cx.add_window_view(|_, cx| Owner::new(cx));
        let slider = owner.read_with(cx, |owner, _| owner.slider.slider().clone());
        slider.update(cx, |_, cx| cx.emit(SliderEvent::Release(80.0.into())));
        cx.run_until_parked();

        cx.update(|window, cx| {
            owner.update(cx, |owner, cx| owner.slider.sync(40, window, cx));
        });

        owner.read_with(cx, |owner, cx| {
            assert_eq!(owner.slider.value(cx), 40);
            assert_eq!(owner.slider.seated(), 40);
        });
    }

    /// A device's supported values can be coarser than the slider's step. A
    /// thumb dropped between two of them already shows the committed one and
    /// must stay where the user left it.
    #[gpui::test]
    fn a_thumb_between_two_stops_only_moves_when_the_committed_stop_changes(
        cx: &mut TestAppContext,
    ) {
        let (owner, cx) = cx.add_window_view(|_, cx| Owner::new(cx));
        let nearest_ten = |value: u8| (value + 5) / 10 * 10;
        let sync = |committed, cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                owner.update(cx, |owner, cx| {
                    owner
                        .slider
                        .sync_snapped(committed, nearest_ten, window, cx);
                });
            });
            owner.read_with(cx, |owner, cx| owner.slider.value(cx))
        };
        cx.update(|window, cx| {
            owner.update(cx, |owner, cx| owner.slider.seat(44, window, cx));
        });

        assert_eq!(sync(40, cx), 44, "44 already resolves to 40");
        assert_eq!(sync(70, cx), 70);

        // A committed value that changes under a drag — a device refresh
        // landing mid-gesture — waits for the release like it does in `sync`.
        let slider = owner.read_with(cx, |owner, _| owner.slider.slider().clone());
        slider.update(cx, |_, cx| cx.emit(SliderEvent::Change(55.0.into())));
        cx.run_until_parked();
        assert_eq!(sync(40, cx), 70, "the dragged thumb stays where it is");
        slider.update(cx, |_, cx| cx.emit(SliderEvent::Release(55.0.into())));
        cx.run_until_parked();
        assert_eq!(
            sync(40, cx),
            40,
            "released, it follows the committed stop again"
        );
    }
}
