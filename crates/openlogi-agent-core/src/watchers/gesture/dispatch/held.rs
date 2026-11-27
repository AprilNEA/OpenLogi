//! Presses whose hold matters, and the trackpad drag a drag hold plays.
//!
//! One [`Held`] per held control, and [`HeldInteractions`] is the only owner
//! of its transitions: a press starts, gathers travel, becomes a drag,
//! follows motion, and ends by release or cancellation. A drag therefore
//! cannot outlive its press, and a glide launched by a release stays owned
//! here so a canceled session can stop it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use openlogi_core::binding::{ButtonId, HeldDrag};

use crate::runtime::{HidppSessionId, PressToken};

/// Raw-XY travel, in sensor counts, before a drag-hold press counts as a
/// drag: below it the press is still a click, as a hand never holds a mouse
/// perfectly still.
const DRAG_START_COUNTS: f64 = 12.0;

/// Plays a drag hold's trackpad gesture. The live output posts through
/// `openlogi-inject`; tests record instead, so they never post real events.
pub(crate) trait DragOutput: Send + Sync {
    /// Begin `kind` with its first motion, whose report arrived `at`, or
    /// `None` where the platform cannot play it.
    fn begin(&self, kind: HeldDrag, dx: f64, dy: f64, at: Instant) -> Option<Box<dyn PlayingDrag>>;
}

/// A trackpad gesture in progress.
pub(crate) trait PlayingDrag: Send {
    /// Follow motion whose report arrived `at`.
    fn moved(&mut self, dx: f64, dy: f64, at: Instant);
    /// The button was released: finish, handing back any glide launched.
    fn release(self: Box<Self>, at: Instant) -> Option<Box<dyn RunningGlide>>;
    /// The press ended without a release: stop with no glide.
    fn cancel(self: Box<Self>);
}

/// A glide still moving after its drag was released.
pub(crate) trait RunningGlide: Send {
    fn stop(&self);
    fn is_running(&self) -> bool;
}

/// The live output: trackpad gestures posted to the OS.
pub(crate) struct LiveDrags;

impl DragOutput for LiveDrags {
    fn begin(&self, kind: HeldDrag, dx: f64, dy: f64, at: Instant) -> Option<Box<dyn PlayingDrag>> {
        openlogi_inject::TrackpadDrag::begin(kind, dx, dy, at)
            .map(|drag| Box::new(drag) as Box<dyn PlayingDrag>)
    }
}

impl PlayingDrag for openlogi_inject::TrackpadDrag {
    fn moved(&mut self, dx: f64, dy: f64, at: Instant) {
        openlogi_inject::TrackpadDrag::moved(self, dx, dy, at);
    }

    fn release(self: Box<Self>, at: Instant) -> Option<Box<dyn RunningGlide>> {
        openlogi_inject::TrackpadDrag::release(*self, at)
            .map(|glide| Box::new(glide) as Box<dyn RunningGlide>)
    }

    fn cancel(self: Box<Self>) {
        openlogi_inject::TrackpadDrag::cancel(*self);
    }
}

impl RunningGlide for openlogi_inject::TrackpadGlide {
    fn stop(&self) {
        openlogi_inject::TrackpadGlide::stop(self);
    }

    fn is_running(&self) -> bool {
        openlogi_inject::TrackpadGlide::is_running(self)
    }
}

/// One held control.
enum Held {
    /// A gesture source or wheel hold: only its press is tracked.
    Press(PressToken),
    /// A drag hold still owing its click, gathering travel toward a drag.
    DragPending {
        token: PressToken,
        kind: HeldDrag,
        travel: (f64, f64),
    },
    /// A drag hold playing its trackpad gesture.
    Dragging {
        token: PressToken,
        drag: Box<dyn PlayingDrag>,
    },
}

impl Held {
    fn token(&self) -> &PressToken {
        match self {
            Self::Press(token) | Self::DragPending { token, .. } | Self::Dragging { token, .. } => {
                token
            }
        }
    }

    fn holds_drag(&self) -> bool {
        matches!(self, Self::DragPending { .. } | Self::Dragging { .. })
    }

    /// End without a release.
    fn cancel(self) {
        if let Self::Dragging { drag, .. } = self {
            drag.cancel();
        }
    }
}

/// What one motion report did to its press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Motion {
    /// The control holds no drag, or is not held.
    Ignored,
    /// A drag hold gathered travel and still owes its click.
    Gathering,
    /// The press became a drag of this kind and gave up its click.
    Started(HeldDrag),
    /// A drag followed the motion.
    Moved,
    /// The press had already ended in the button runtime: any drag was
    /// canceled and the control forgotten.
    Stale,
}

/// Every held control, by capture session and button, and the glides their
/// releases launched.
#[derive(Default)]
pub(super) struct HeldInteractions {
    held: HashMap<(HidppSessionId, ButtonId), Held>,
    glides: Vec<(HidppSessionId, Box<dyn RunningGlide>)>,
}

impl HeldInteractions {
    /// A press the runtime admitted: a drag hold of `drag`, or a plain held
    /// press. A press already held there was released unseen and is canceled.
    pub(super) fn start(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        token: PressToken,
        drag: Option<HeldDrag>,
    ) {
        let held = match drag {
            Some(kind) => Held::DragPending {
                token,
                kind,
                travel: (0.0, 0.0),
            },
            None => Held::Press(token),
        };
        if let Some(previous) = self.held.insert((session.clone(), button), held) {
            previous.cancel();
        }
    }

    /// The press `button` holds in `session`, if any.
    pub(super) fn token(&self, session: &HidppSessionId, button: ButtonId) -> Option<&PressToken> {
        self.held.get(&(session.clone(), button)).map(Held::token)
    }

    /// Whether a drag-hold button is held in `session`, dragging or not.
    pub(super) fn drag_held(&self, session: &HidppSessionId) -> bool {
        self.held
            .iter()
            .any(|((candidate, _), held)| candidate == session && held.holds_drag())
    }

    /// `button` was released `at`: a drag finishes and its glide is kept.
    /// A second release finds nothing.
    pub(super) fn release(&mut self, session: &HidppSessionId, button: ButtonId, at: Instant) {
        if let Some(Held::Dragging { drag, .. }) = self.held.remove(&(session.clone(), button))
            && let Some(glide) = drag.release(at)
        {
            self.glides.retain(|(_, glide)| glide.is_running());
            self.glides.push((session.clone(), glide));
        }
    }

    /// Forget `button` without a release: its press was not admitted.
    pub(super) fn forget(&mut self, session: &HidppSessionId, button: ButtonId) {
        if let Some(held) = self.held.remove(&(session.clone(), button)) {
            held.cancel();
        }
    }

    /// `session` ended: cancel its drags and stop its glides.
    pub(super) fn cancel_session(&mut self, session: &HidppSessionId) {
        let ended: Vec<_> = self
            .held
            .keys()
            .filter(|(candidate, _)| candidate == session)
            .cloned()
            .collect();
        for key in ended {
            if let Some(held) = self.held.remove(&key) {
                held.cancel();
            }
        }
        self.glides.retain(|(candidate, glide)| {
            if candidate == session {
                glide.stop();
                false
            } else {
                glide.is_running()
            }
        });
    }

    /// Motion of `dx`, `dy` counts under `button`, whose report arrived
    /// `at`. `is_current` says whether the runtime still holds a press;
    /// `use_hold` tells it the press dragged, and refuses an ended press.
    pub(super) fn motion(
        &mut self,
        (session, button): (&HidppSessionId, ButtonId),
        (dx, dy, at): (f64, f64, Instant),
        output: &dyn DragOutput,
        is_current: impl Fn(&PressToken) -> bool,
        use_hold: impl FnOnce(&PressToken) -> bool,
    ) -> Motion {
        let key = (session.clone(), button);
        let Some(held) = self.held.remove(&key) else {
            return Motion::Ignored;
        };
        let (next, outcome) = match held {
            Held::Press(_) => (Some(held), Motion::Ignored),
            Held::Dragging { token, mut drag } => {
                if is_current(&token) {
                    drag.moved(dx, dy, at);
                    (Some(Held::Dragging { token, drag }), Motion::Moved)
                } else {
                    drag.cancel();
                    (None, Motion::Stale)
                }
            }
            Held::DragPending {
                token,
                kind,
                travel,
            } => {
                let travel = (travel.0 + dx, travel.1 + dy);
                if travel.0.hypot(travel.1) < DRAG_START_COUNTS {
                    (
                        Some(Held::DragPending {
                            token,
                            kind,
                            travel,
                        }),
                        Motion::Gathering,
                    )
                } else if !is_current(&token) {
                    (None, Motion::Stale)
                } else {
                    // A fresh touch catches whatever is still gliding.
                    for (_, glide) in self.glides.drain(..) {
                        glide.stop();
                    }
                    match output.begin(kind, travel.0, travel.1, at) {
                        // The platform cannot play it: the press keeps its
                        // click, and gathers no further.
                        None => (Some(Held::Press(token)), Motion::Ignored),
                        Some(drag) if use_hold(&token) => {
                            (Some(Held::Dragging { token, drag }), Motion::Started(kind))
                        }
                        Some(drag) => {
                            drag.cancel();
                            (None, Motion::Stale)
                        }
                    }
                }
            }
        };
        if let Some(next) = next {
            self.held.insert(key, next);
        }
        outcome
    }
}

/// The shared handle a capture session's dispatcher plays drags through.
pub(crate) type SharedDragOutput = Arc<dyn DragOutput>;

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// What a fake drag saw, in order.
    #[derive(Debug, Clone, PartialEq)]
    enum Seen {
        Began(HeldDrag),
        Moved,
        Released,
        Canceled,
        GlideStopped,
    }

    #[derive(Default, Clone)]
    struct Recorder {
        seen: Arc<Mutex<Vec<Seen>>>,
        glides: bool,
        unsupported: bool,
    }

    impl Recorder {
        fn push(&self, seen: Seen) {
            self.seen.lock().expect("recorder").push(seen);
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().expect("recorder").clone()
        }
    }

    struct FakeDrag(Recorder);
    struct FakeGlide(Recorder, AtomicBool);

    impl DragOutput for Recorder {
        fn begin(
            &self,
            kind: HeldDrag,
            _: f64,
            _: f64,
            _: Instant,
        ) -> Option<Box<dyn PlayingDrag>> {
            if self.unsupported {
                return None;
            }
            self.push(Seen::Began(kind));
            Some(Box::new(FakeDrag(self.clone())))
        }
    }

    impl PlayingDrag for FakeDrag {
        fn moved(&mut self, _: f64, _: f64, _: Instant) {
            self.0.push(Seen::Moved);
        }

        fn release(self: Box<Self>, _: Instant) -> Option<Box<dyn RunningGlide>> {
            self.0.push(Seen::Released);
            self.0
                .glides
                .then(|| Box::new(FakeGlide(self.0.clone(), AtomicBool::new(true))) as _)
        }

        fn cancel(self: Box<Self>) {
            self.0.push(Seen::Canceled);
        }
    }

    impl RunningGlide for FakeGlide {
        fn stop(&self) {
            self.1.store(false, Ordering::Relaxed);
            self.0.push(Seen::GlideStopped);
        }

        fn is_running(&self) -> bool {
            self.1.load(Ordering::Relaxed)
        }
    }

    fn session() -> HidppSessionId {
        HidppSessionId::with_epoch("mouse-a", 1)
    }

    fn token() -> PressToken {
        PressToken::hook_for_test(1, ButtonId::Forward)
    }

    /// A Forward drag hold, moved far enough to start.
    fn dragging(output: &Recorder) -> HeldInteractions {
        let mut held = HeldInteractions::default();
        held.start(
            &session(),
            ButtonId::Forward,
            token(),
            Some(HeldDrag::TwoFingerSwipe),
        );
        let started = held.motion(
            (&session(), ButtonId::Forward),
            (20.0, 0.0, Instant::now()),
            output,
            |_| true,
            |_| true,
        );
        assert_eq!(started, Motion::Started(HeldDrag::TwoFingerSwipe));
        held
    }

    #[test]
    fn drag_session_cancel_stops_output() {
        // While dragging: the drag is canceled, never released.
        let output = Recorder::default();
        let mut held = dragging(&output);
        held.cancel_session(&session());
        assert_eq!(
            output.seen(),
            [Seen::Began(HeldDrag::TwoFingerSwipe), Seen::Canceled]
        );
        assert!(!held.drag_held(&session()));

        // While gliding: the released drag's glide is stopped.
        let output = Recorder {
            glides: true,
            ..Recorder::default()
        };
        let mut held = dragging(&output);
        held.release(&session(), ButtonId::Forward, Instant::now());
        held.cancel_session(&session());
        assert_eq!(
            output.seen(),
            [
                Seen::Began(HeldDrag::TwoFingerSwipe),
                Seen::Released,
                Seen::GlideStopped
            ]
        );
    }

    #[test]
    fn motion_after_the_press_ended_cancels_the_drag() {
        let output = Recorder::default();
        let mut held = dragging(&output);
        let stale = held.motion(
            (&session(), ButtonId::Forward),
            (5.0, 0.0, Instant::now()),
            &output,
            |_| false,
            |_| true,
        );
        assert_eq!(stale, Motion::Stale);
        assert_eq!(
            output.seen(),
            [Seen::Began(HeldDrag::TwoFingerSwipe), Seen::Canceled]
        );
        assert!(held.token(&session(), ButtonId::Forward).is_none());
    }

    #[test]
    fn a_repeated_release_ends_the_drag_once() {
        let output = Recorder::default();
        let mut held = dragging(&output);
        held.release(&session(), ButtonId::Forward, Instant::now());
        held.release(&session(), ButtonId::Forward, Instant::now());
        assert_eq!(
            output.seen(),
            [Seen::Began(HeldDrag::TwoFingerSwipe), Seen::Released]
        );
    }

    #[test]
    fn an_unplayable_drag_keeps_the_click() {
        let output = Recorder {
            unsupported: true,
            ..Recorder::default()
        };
        let mut held = HeldInteractions::default();
        held.start(
            &session(),
            ButtonId::Forward,
            token(),
            Some(HeldDrag::TwoFingerSwipe),
        );
        let outcome = held.motion(
            (&session(), ButtonId::Forward),
            (20.0, 0.0, Instant::now()),
            &output,
            |_| true,
            |_| panic!("the press must not give up its click"),
        );
        assert_eq!(outcome, Motion::Ignored);
        assert!(output.seen().is_empty());
        assert!(!held.drag_held(&session()), "back to a plain press");
    }

    #[test]
    fn a_drag_hold_counts_as_held_until_released() {
        // What keeps the thumb wheel quiet beside a held Back/Forward.
        let output = Recorder::default();
        let mut held = HeldInteractions::default();
        held.start(&session(), ButtonId::Back, token(), None);
        assert!(!held.drag_held(&session()), "a plain held press is no drag");
        held.start(
            &session(),
            ButtonId::Forward,
            token(),
            Some(HeldDrag::TwoFingerSwipe),
        );
        assert!(held.drag_held(&session()), "pending");
        let _ = held.motion(
            (&session(), ButtonId::Forward),
            (20.0, 0.0, Instant::now()),
            &output,
            |_| true,
            |_| true,
        );
        assert!(held.drag_held(&session()), "dragging");
        held.release(&session(), ButtonId::Forward, Instant::now());
        assert!(!held.drag_held(&session()));
    }

    #[test]
    fn a_new_drag_catches_a_running_glide() {
        let output = Recorder {
            glides: true,
            ..Recorder::default()
        };
        let mut held = dragging(&output);
        held.release(&session(), ButtonId::Forward, Instant::now());
        held.start(
            &session(),
            ButtonId::Forward,
            token(),
            Some(HeldDrag::TwoFingerSwipe),
        );
        let _ = held.motion(
            (&session(), ButtonId::Forward),
            (20.0, 0.0, Instant::now()),
            &output,
            |_| true,
            |_| true,
        );
        assert_eq!(
            output.seen()[2..],
            [Seen::GlideStopped, Seen::Began(HeldDrag::TwoFingerSwipe)]
        );
    }
}
