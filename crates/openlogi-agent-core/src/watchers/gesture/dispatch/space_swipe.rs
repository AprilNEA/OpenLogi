//! Live, follow-the-hand Space transitions for desktop-bound gestures.
//!
//! A horizontal swipe bound to Next/Previous Desktop opens a live transition
//! instead of firing the one-shot switch: the Dock drags the neighbouring
//! Space in as the hand keeps moving, every full desktop width of travel
//! completes one switch, and the release finishes the switch (past halfway,
//! or flicked) or lets it spring back — a trackpad's three-finger swipe,
//! driven by the gesture button.
//!
//! The Dock takes one swipe at a time, so a long sweep settles between
//! desktops: after a switch completes, motion is dropped until the Space
//! change lands, and the next transition starts from zero rather than jumping
//! to where the hand has got to. At the first and last desktop the transition
//! holds at the edge instead of asking for a switch the Dock cannot make.
//!
//! This is pure policy: it turns hold events into frames, and the dispatcher
//! posts them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId, GestureDirection};
use openlogi_inject::{SpacePosition, SpaceSwipePhase};

use crate::runtime::HidppSessionId;

/// Raw-XY travel that drags the transition across one full desktop, unless
/// [`openlogi_core::env::SPACE_SWIPE_TRAVEL`] overrides it.
pub(super) const DEFAULT_TRAVEL: f64 = 800.0;
/// Progress past which a release finishes the switch instead of springing back.
const COMPLETE_AT: f64 = 0.5;
/// Release speed, in desktop widths per second along the transition, that
/// finishes a switch short of [`COMPLETE_AT`], like a trackpad flick.
const FLICK_SPEED: f64 = 2.5;
/// Least progress a flick needs, so a twitch at the commit never switches.
const FLICK_MIN: f64 = 0.1;
/// Where a flick that finishes short of halfway is placed before it ends, so
/// the Dock reads it as past the midpoint and completes it.
const FLICK_FINISH: f64 = 0.55;
/// A motion gap this long means the hand stopped: the release carries no speed.
const STALE_MOTION: Duration = Duration::from_millis(100);
/// Reports closer together than this (the commit's own travel arriving with
/// the commit) move the transition but are no speed sample.
const MIN_SPEED_SAMPLE: Duration = Duration::from_millis(2);
/// Longest wait for a completed switch to land before the next transition
/// starts anyway (the Dock may not report a change it did not make).
const SETTLE_TIMEOUT: Duration = Duration::from_millis(500);

/// The travel per desktop in effect; `None` when live transitions are off.
pub(super) fn configured_travel() -> Option<f64> {
    let travel = std::env::var(openlogi_core::env::SPACE_SWIPE_TRAVEL)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .unwrap_or(DEFAULT_TRAVEL);
    (travel.is_finite() && travel > 0.0).then_some(travel)
}

/// What the transition asks of the desktop it runs on.
#[derive(Clone, Copy)]
pub(super) struct SpaceProbe {
    /// Where the display under the cursor is in its row of desktops; `None`
    /// when unknown (then both neighbours are assumed and a landing is read
    /// from `changes` alone).
    pub(super) position: fn() -> Option<SpacePosition>,
    /// A count that ticks when any display's active desktop changes; `None`
    /// when the platform does not report it.
    pub(super) changes: fn() -> Option<u64>,
}

impl SpaceProbe {
    /// The live desktop.
    #[cfg_attr(test, expect(dead_code, reason = "unit tests read a fake desktop"))]
    pub(super) const NATIVE: Self = Self {
        position: openlogi_inject::space_position,
        changes: openlogi_inject::space_change_count,
    };
}

/// One frame for the Dock: cumulative progress in desktop widths (positive
/// toward the next desktop) and its lifecycle phase.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Frame {
    pub(super) progress: f64,
    pub(super) phase: SpaceSwipePhase,
}

impl Frame {
    const fn new(progress: f64, phase: SpaceSwipePhase) -> Self {
        Self { progress, phase }
    }
}

/// The progress sign of a desktop action: `+1` toward the next desktop.
fn desktop_sign(action: &Action) -> Option<f64> {
    match action {
        Action::NextDesktop => Some(1.0),
        Action::PreviousDesktop => Some(-1.0),
        _ => None,
    }
}

/// The raw-XY sign of a horizontal swipe: `+1` rightward.
fn horizontal_sign(direction: GestureDirection) -> Option<f64> {
    match direction {
        GestureDirection::Right => Some(1.0),
        GestureDirection::Left => Some(-1.0),
        GestureDirection::Up | GestureDirection::Down | GestureDirection::Click => None,
    }
}

/// Which way a transition may complete from the current desktop.
#[derive(Clone, Copy)]
struct Bounds {
    previous: bool,
    next: bool,
}

impl Bounds {
    fn read(probe: SpaceProbe) -> Self {
        (probe.position)().map_or(
            Self {
                previous: true,
                next: true,
            },
            |position| Self {
                previous: position.has_previous,
                next: position.has_next,
            },
        )
    }

    /// Whether a desktop exists toward progress of `sign`.
    fn allows(self, sign: f64) -> bool {
        if sign > 0.0 { self.next } else { self.previous }
    }
}

enum Stage {
    /// A Dock transition is following the hand.
    InFlight { progress: f64, bounds: Bounds },
    /// A switch just completed; motion is dropped until it lands.
    Settling {
        since: Instant,
        /// The change count when the switch completed.
        changes: Option<u64>,
        /// This display's desktop when the switch completed, so a change on
        /// another display is not mistaken for this one landing.
        space: Option<u64>,
    },
}

struct Transition {
    /// Progress per unit of rightward raw travel's sign: `+1` when moving
    /// right heads toward the next desktop.
    polarity: f64,
    /// The progress sign the hand may not drag back across, when the opposite
    /// swipe is not bound to the opposite desktop action; `None` lets the
    /// transition swing to either neighbour.
    keep_sign: Option<f64>,
    stage: Stage,
    /// Smoothed speed along the transition, desktop widths per second.
    velocity: f64,
    moved_at: Instant,
}

/// Live transitions by capture session and held gesture source. A hold keeps
/// its entry from the first desktop swipe until release, so later swipes in
/// the same hold are motion for the transition rather than new actions.
pub(super) struct SpaceSwipes {
    travel: Option<f64>,
    probe: SpaceProbe,
    active: HashMap<(HidppSessionId, ButtonId), Transition>,
}

impl SpaceSwipes {
    /// Live transitions dragging `travel` raw-XY units per desktop (or none),
    /// reading the desktop through `probe`.
    pub(super) fn new(travel: Option<f64>, probe: SpaceProbe) -> Self {
        Self {
            travel,
            probe,
            active: HashMap::new(),
        }
    }

    /// Whether `button`'s hold on `session` is driving a live transition.
    pub(super) fn is_active(&self, session: &HidppSessionId, button: ButtonId) -> bool {
        self.active.contains_key(&(session.clone(), button))
    }

    /// Open a live transition for a committed swipe bound to a desktop action,
    /// returning its first frame; `None` (nothing opened) for anything else.
    /// `opposite` is the action bound to the reverse swipe. It opens at zero:
    /// the commit's own travel follows as the hold's first motion.
    pub(super) fn begin(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        direction: GestureDirection,
        action: &Action,
        opposite: Option<&Action>,
        now: Instant,
    ) -> Option<Frame> {
        // Live transitions are off without a travel per desktop.
        self.travel?;
        let source = horizontal_sign(direction)?;
        let target = desktop_sign(action)?;
        let swings_both_ways = opposite.and_then(desktop_sign) == Some(-target);
        // Subscribe to Space changes before the first switch can land.
        let _ = (self.probe.changes)();
        self.active.insert(
            (session.clone(), button),
            Transition {
                polarity: target * source,
                keep_sign: (!swings_both_ways).then_some(target),
                stage: Stage::InFlight {
                    progress: 0.0,
                    bounds: Bounds::read(self.probe),
                },
                velocity: 0.0,
                moved_at: now,
            },
        );
        Some(Frame::new(0.0, SpaceSwipePhase::Began))
    }

    /// Forget a transition whose first frame could not be posted.
    pub(super) fn abandon(&mut self, session: &HidppSessionId, button: ButtonId) {
        self.active.remove(&(session.clone(), button));
    }

    /// Spring back the transition `button`'s hold is driving, if any.
    pub(super) fn cancel(&mut self, session: &HidppSessionId, button: ButtonId) -> Vec<Frame> {
        match self.active.remove(&(session.clone(), button)) {
            Some(Transition {
                stage: Stage::InFlight { progress, .. },
                ..
            }) => vec![Frame::new(progress, SpaceSwipePhase::Cancelled)],
            _ => Vec::new(),
        }
    }

    /// Follow one raw-XY report of the hold, returning the frames to post.
    pub(super) fn motion(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        dx: i32,
        now: Instant,
    ) -> Vec<Frame> {
        let (Some(travel), probe) = (self.travel, self.probe) else {
            return Vec::new();
        };
        let Some(t) = self.active.get_mut(&(session.clone(), button)) else {
            return Vec::new();
        };
        let delta = t.polarity * f64::from(dx) / travel;
        let elapsed = now.saturating_duration_since(t.moved_at);
        if elapsed >= STALE_MOTION {
            t.velocity = 0.0;
        }
        if elapsed >= MIN_SPEED_SAMPLE {
            t.velocity = 0.5 * t.velocity + 0.5 * delta / elapsed.as_secs_f64();
        }
        t.moved_at = now;
        let keep_sign = t.keep_sign;
        let hold_sign = |progress: f64| match keep_sign {
            Some(sign) if progress * sign < 0.0 => 0.0,
            _ => progress,
        };

        match &mut t.stage {
            Stage::Settling {
                since,
                changes,
                space,
            } => {
                // Landed once a change was reported and this display's own
                // desktop moved on; either signal alone is not enough on a
                // multi-display desktop.
                let ticked = (probe.changes)()
                    .zip(*changes)
                    .map(|(current, at_end)| current != at_end);
                let moved = (probe.position)()
                    .map(|position| position.current)
                    .zip(*space)
                    .map(|(current, at_end)| current != at_end);
                let landed = match (ticked, moved) {
                    (None, None) => false,
                    (ticked, moved) => ticked.unwrap_or(true) && moved.unwrap_or(true),
                };
                if !landed && now.saturating_duration_since(*since) < SETTLE_TIMEOUT {
                    return Vec::new();
                }
                // The switch landed: the next transition starts from this
                // report, not from wherever the hand got to meanwhile.
                let progress = hold_sign(delta);
                t.velocity = 0.0;
                t.stage = Stage::InFlight {
                    progress,
                    bounds: Bounds::read(probe),
                };
                vec![Frame::new(progress, SpaceSwipePhase::Began)]
            }
            Stage::InFlight { progress, bounds } => {
                *progress = hold_sign(*progress + delta);
                let sign = progress.signum();
                if progress.abs() < 1.0 {
                    return vec![Frame::new(*progress, SpaceSwipePhase::Changed)];
                }
                if !bounds.allows(sign) {
                    // The last desktop: hold at the edge.
                    *progress = sign;
                    return vec![Frame::new(*progress, SpaceSwipePhase::Changed)];
                }
                // A full desktop width: finish this switch and settle.
                t.stage = Stage::Settling {
                    since: now,
                    changes: (probe.changes)(),
                    space: (probe.position)().map(|position| position.current),
                };
                vec![Frame::new(sign, SpaceSwipePhase::Ended)]
            }
        }
    }

    /// Release the hold: finish the switch in flight when it is past halfway
    /// or flicked toward a desktop that exists, otherwise let it spring back.
    pub(super) fn end(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        now: Instant,
    ) -> Vec<Frame> {
        let Some(t) = self.active.remove(&(session.clone(), button)) else {
            return Vec::new();
        };
        let Stage::InFlight { progress, bounds } = t.stage else {
            return Vec::new();
        };
        let sign = progress.signum();
        let speed = if now.saturating_duration_since(t.moved_at) >= STALE_MOTION {
            0.0
        } else {
            t.velocity * sign
        };
        let past_halfway = progress.abs() >= COMPLETE_AT && speed > -FLICK_SPEED;
        let flicked = progress.abs() >= FLICK_MIN && speed >= FLICK_SPEED;
        if !bounds.allows(sign) {
            vec![Frame::new(progress, SpaceSwipePhase::Cancelled)]
        } else if past_halfway {
            vec![Frame::new(progress, SpaceSwipePhase::Ended)]
        } else if flicked {
            let finish = sign * FLICK_FINISH;
            vec![
                Frame::new(finish, SpaceSwipePhase::Changed),
                Frame::new(finish, SpaceSwipePhase::Ended),
            ]
        } else {
            vec![Frame::new(progress, SpaceSwipePhase::Cancelled)]
        }
    }

    /// Spring back every transition `session` has in flight.
    pub(super) fn cancel_session(&mut self, session: &HidppSessionId) -> Vec<Frame> {
        let mut frames = Vec::new();
        self.active.retain(|(candidate, _), t| {
            if candidate != session {
                return true;
            }
            if let Stage::InFlight { progress, .. } = t.stage {
                frames.push(Frame::new(progress, SpaceSwipePhase::Cancelled));
            }
            false
        });
        frames
    }
}

#[cfg(test)]
mod tests;
