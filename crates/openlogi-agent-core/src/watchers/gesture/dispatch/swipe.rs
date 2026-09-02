//! Pair-planned native gesture streaming.
//!
//! A DockSwipe animation belongs to a gesture *pair* — the left/right or
//! up/down directions of one finger count, or that count's pinch in/out —
//! not to a single direction: the recognizer commits one direction, but
//! every frame after the commit drives the pair's one shared progress
//! stream, so an unbound side still follows the finger and springs back at
//! release instead of dying silently.
//!
//! A pair streams only when the system can honor every bound direction: each
//! bound action needs a native swipe-commit consumer (desktop switching on
//! the horizontal motion, Mission Control / App Exposé on the vertical one)
//! and all of them must share one motion, with the travel mapping solved so
//! each bound side commits its own binding. Anything else — no binding on
//! the pair, a keyboard-style action, mixed motions, or the same commit sign
//! demanded twice — keeps the pair discrete, so an animation can never
//! commit an outcome the user did not bind. A bound pair whose one side is
//! unbound clamps progress at zero on that side: the finger can pull the
//! animation back, never push it past its start.
//!
//! Pinch pairs stream onto the existing motions the same way — the motion
//! follows the bound actions' consumer, and spread change drives its
//! progress. Their own native consumers (the Launchpad successor and Show
//! Desktop on the Dock's pinch axis) wait on hardware identification of the
//! event that drives them; until then, a pinch bound to a zoom action keeps
//! the discrete dispatch, matching the pair defaults.

use std::collections::BTreeMap;

use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::touchpad::{TouchContact, TouchFrame};
use openlogi_inject::DockSwipeMotion;

/// One pad-width of travel equals one progress unit; the constants mirror the
/// target Casa Touch pad (2775 × 1786 @ 600 dpi ≈ 117 × 76 mm) until real
/// geometry is plumbed through.
const HORIZONTAL_PAD_TRAVEL_UM: f64 = 117_000.0;
const VERTICAL_PAD_TRAVEL_UM: f64 = 75_600.0;
/// One full open↔close spread span equals one progress unit. A hardware
/// tuning constant: the native pinch reveal tracks roughly this much spread
/// change across the whole gesture.
const PINCH_SPREAD_TRAVEL_UM: f64 = 40_000.0;

/// One routed step of a session's swipe stream.
#[derive(Debug, Default, PartialEq)]
pub(super) enum SwipeOutput {
    /// Nothing to stream for this frame.
    #[default]
    Idle,
    /// `progress` is the opening frame's travel; later frames stream deltas.
    Begin {
        motion: DockSwipeMotion,
        progress: f64,
    },
    Advance {
        motion: DockSwipeMotion,
        delta: f64,
    },
    Finish {
        motion: DockSwipeMotion,
        end: SwipeEnd,
    },
}

/// Release lets the injector's sign rule commit or spring back; an abort
/// always springs back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SwipeEnd {
    AtRelease,
    Cancelled,
}

/// Direction along a pair's gesture axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    /// Left on the horizontal axis, down on the vertical one, closing on the
    /// pinch one.
    Negative,
    /// Right on the horizontal axis, up on the vertical one, spreading on
    /// the pinch one.
    Positive,
}

impl Side {
    fn opposite(self) -> Self {
        match self {
            Side::Positive => Side::Negative,
            Side::Negative => Side::Positive,
        }
    }
}

/// The physical quantity of a stroke that drives its pair's progress:
/// centroid travel along a finger axis, or contact spread on the pinch axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GestureAxis {
    Horizontal,
    Vertical,
    Pinch,
}

/// The native swipe-commit consumer an action maps onto, with the motion
/// progress sign it commits at. Flipping the two sign anchors re-anchors the
/// whole table if hardware verification disagrees.
fn swipe_consumer(action: &Action) -> Option<(DockSwipeMotion, i8)> {
    /// Fingers right commit the next Space.
    const NEXT_DESKTOP_SIGN: i8 = 1;
    match action {
        Action::NextDesktop => Some((DockSwipeMotion::Horizontal, NEXT_DESKTOP_SIGN)),
        Action::PreviousDesktop => Some((DockSwipeMotion::Horizontal, -NEXT_DESKTOP_SIGN)),
        Action::MissionControl => Some((DockSwipeMotion::Vertical, 1)),
        Action::AppExpose => Some((DockSwipeMotion::Vertical, -1)),
        _ => None,
    }
}

/// The sibling trigger on the same gesture axis, the pair's axis, and this
/// trigger's side along it.
fn pair_sibling(trigger: ButtonId) -> Option<(ButtonId, GestureAxis, Side)> {
    use ButtonId::{
        TouchpadFourFingerPinchIn, TouchpadFourFingerPinchOut, TouchpadFourFingerSwipeDown,
        TouchpadFourFingerSwipeLeft, TouchpadFourFingerSwipeRight, TouchpadFourFingerSwipeUp,
        TouchpadThreeFingerSwipeDown, TouchpadThreeFingerSwipeLeft, TouchpadThreeFingerSwipeRight,
        TouchpadThreeFingerSwipeUp, TouchpadTwoFingerPinchIn, TouchpadTwoFingerPinchOut,
    };
    Some(match trigger {
        TouchpadThreeFingerSwipeRight => (
            TouchpadThreeFingerSwipeLeft,
            GestureAxis::Horizontal,
            Side::Positive,
        ),
        TouchpadThreeFingerSwipeLeft => (
            TouchpadThreeFingerSwipeRight,
            GestureAxis::Horizontal,
            Side::Negative,
        ),
        TouchpadThreeFingerSwipeUp => (
            TouchpadThreeFingerSwipeDown,
            GestureAxis::Vertical,
            Side::Positive,
        ),
        TouchpadThreeFingerSwipeDown => (
            TouchpadThreeFingerSwipeUp,
            GestureAxis::Vertical,
            Side::Negative,
        ),
        TouchpadFourFingerSwipeRight => (
            TouchpadFourFingerSwipeLeft,
            GestureAxis::Horizontal,
            Side::Positive,
        ),
        TouchpadFourFingerSwipeLeft => (
            TouchpadFourFingerSwipeRight,
            GestureAxis::Horizontal,
            Side::Negative,
        ),
        TouchpadFourFingerSwipeUp => (
            TouchpadFourFingerSwipeDown,
            GestureAxis::Vertical,
            Side::Positive,
        ),
        TouchpadFourFingerSwipeDown => (
            TouchpadFourFingerSwipeUp,
            GestureAxis::Vertical,
            Side::Negative,
        ),
        TouchpadTwoFingerPinchOut => (TouchpadTwoFingerPinchIn, GestureAxis::Pinch, Side::Positive),
        TouchpadTwoFingerPinchIn => (
            TouchpadTwoFingerPinchOut,
            GestureAxis::Pinch,
            Side::Negative,
        ),
        TouchpadFourFingerPinchOut => (
            TouchpadFourFingerPinchIn,
            GestureAxis::Pinch,
            Side::Positive,
        ),
        TouchpadFourFingerPinchIn => (
            TouchpadFourFingerPinchOut,
            GestureAxis::Pinch,
            Side::Negative,
        ),
        _ => return None,
    })
}

/// One pair's resolved streaming plan: the motion its bound actions share,
/// the finger-travel mapping that honors them, and the binding each progress
/// sign commits.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SwipeStreamPlan {
    axis: GestureAxis,
    motion: DockSwipeMotion,
    /// Whether finger travel maps onto motion progress negated, solved from
    /// the bindings so each bound side commits its own action.
    flipped: bool,
    /// The binding each progress sign commits; an absent sign clamps at zero.
    positive: Option<(ButtonId, Action)>,
    negative: Option<(ButtonId, Action)>,
}

impl SwipeStreamPlan {
    /// Plan the pair `trigger` belongs to from a stroke's frozen bindings.
    pub(super) fn for_trigger(
        trigger: ButtonId,
        bindings: &BTreeMap<ButtonId, Action>,
    ) -> Option<Self> {
        let (sibling, axis, own_side) = pair_sibling(trigger)?;
        let mut motion = None;
        let mut positive = None;
        let mut negative = None;
        // The mapping every bound side demands: its travel, mapped onto the
        // motion, must commit at its binding's native sign.
        let mut demanded = None;
        for (button, side) in [(trigger, own_side), (sibling, own_side.opposite())] {
            let Some(action) = bindings.get(&button) else {
                continue;
            };
            let (action_motion, commit_sign) = swipe_consumer(action)?;
            if motion.is_some_and(|motion| motion != action_motion) {
                return None;
            }
            motion = Some(action_motion);
            let demand = side != commit_side(commit_sign);
            if demanded.is_some_and(|existing| existing != demand) {
                return None;
            }
            demanded = Some(demand);
            if commit_sign > 0 {
                positive = Some((button, action.clone()));
            } else {
                negative = Some((button, action.clone()));
            }
        }
        Some(Self {
            axis,
            motion: motion?,
            flipped: demanded?,
            positive,
            negative,
        })
    }
}

/// The finger side a commit sign belongs to: positive progress commits on
/// the pair's positive side unflipped, on the negative one flipped.
fn commit_side(commit_sign: i8) -> Side {
    if commit_sign > 0 {
        Side::Positive
    } else {
        Side::Negative
    }
}

/// A committed gesture streaming its pair's animation, anchored at the
/// commit frame and advanced by every later frame of the stroke.
pub(super) struct ActiveSwipe {
    plan: SwipeStreamPlan,
    /// The direction the recognizer committed on, whose binding a
    /// never-opened stream falls back to at release.
    committed: ButtonId,
    opened: bool,
    contact_ids: Box<[u8]>,
    centroid_um: (i64, i64),
    /// Mean contact distance from the centroid, the pinch axis's quantity.
    spread_um: f64,
    /// Clamped progress accumulated since the stream anchored.
    progress: f64,
}

impl ActiveSwipe {
    pub(super) fn new(frame: &TouchFrame, plan: SwipeStreamPlan, committed: ButtonId) -> Self {
        let centroid = frame_centroid(frame.contacts());
        Self {
            plan,
            committed,
            opened: false,
            contact_ids: frame.contacts().iter().map(|contact| contact.id).collect(),
            centroid_um: centroid,
            spread_um: frame_spread(frame.contacts(), centroid),
            progress: 0.0,
        }
    }

    /// Fold one frame into the stream. A contact-set change re-anchors
    /// without progress; frames whose clamped progress does not move — no
    /// travel, or an unbound sign held past zero — emit nothing. Pinned
    /// travel is dropped rather than deferred: once the finger re-enters a
    /// bound sign, the animation tracks it one-to-one from that frame.
    #[expect(
        clippy::cast_precision_loss,
        reason = "centroid and spread deltas are bounded by the pad size; f64 precision is ample"
    )]
    pub(super) fn advance(&mut self, frame: &TouchFrame) -> Option<SwipeOutput> {
        let contact_ids: Vec<u8> = frame.contacts().iter().map(|contact| contact.id).collect();
        let centroid = frame_centroid(frame.contacts());
        let spread = frame_spread(frame.contacts(), centroid);
        let mut mapped = 0.0;
        if contact_ids.as_slice() == &*self.contact_ids {
            let raw = match self.plan.axis {
                GestureAxis::Horizontal => {
                    (centroid.0 - self.centroid_um.0) as f64 / HORIZONTAL_PAD_TRAVEL_UM
                }
                // Window y grows downward, but vertical progress is
                // positive upward.
                GestureAxis::Vertical => {
                    -(centroid.1 - self.centroid_um.1) as f64 / VERTICAL_PAD_TRAVEL_UM
                }
                GestureAxis::Pinch => (spread - self.spread_um) / PINCH_SPREAD_TRAVEL_UM,
            };
            mapped = if self.plan.flipped { -raw } else { raw };
        }
        self.contact_ids = contact_ids.into_boxed_slice();
        self.centroid_um = centroid;
        self.spread_um = spread;
        let candidate = self.progress + mapped;
        // An unbound sign cannot cross zero: it may be pulled back to it,
        // never past it, so the release rule springs the stroke back instead
        // of committing a direction the user left unbound. Unclipped frames
        // stream the mapped travel itself — bit-identical to the raw
        // division — while a clipped one falls back to the progress
        // difference it actually achieved.
        let (lower, upper) = (self.lower_bound(), self.upper_bound());
        let clipped = candidate < lower || candidate > upper;
        let next = if clipped {
            candidate.clamp(lower, upper)
        } else {
            candidate
        };
        let delta = if clipped {
            next - self.progress
        } else {
            mapped
        };
        self.progress = next;
        if delta == 0.0 {
            return None;
        }
        if !self.opened {
            self.opened = true;
            return Some(SwipeOutput::Begin {
                motion: self.plan.motion,
                progress: delta,
            });
        }
        Some(SwipeOutput::Advance {
            motion: self.plan.motion,
            delta,
        })
    }

    /// The lowest progress the pair's bindings may commit at.
    fn lower_bound(&self) -> f64 {
        if self.plan.negative.is_some() {
            f64::NEG_INFINITY
        } else {
            0.0
        }
    }

    /// The highest progress the pair's bindings may commit at.
    fn upper_bound(&self) -> f64 {
        if self.plan.positive.is_some() {
            f64::INFINITY
        } else {
            0.0
        }
    }

    /// Resolve the stroke's lift. An opened stream hands the commit-versus-
    /// spring-back decision to the injector; one that never produced
    /// in-bounds travel opened no animation at all, so its committed
    /// direction's binding fires discretely — an ultra-short stroke must not
    /// lose it.
    pub(super) fn release(self) -> (SwipeOutput, Option<(ButtonId, Action)>) {
        if !self.opened {
            return (SwipeOutput::Idle, self.committed_binding());
        }
        (
            SwipeOutput::Finish {
                motion: self.plan.motion,
                end: SwipeEnd::AtRelease,
            },
            None,
        )
    }

    /// Session teardown always springs the animation back.
    pub(super) fn terminate(self) -> SwipeOutput {
        if !self.opened {
            return SwipeOutput::Idle;
        }
        SwipeOutput::Finish {
            motion: self.plan.motion,
            end: SwipeEnd::Cancelled,
        }
    }

    /// The action a failed `Begin` must fall back to: the slot of the sign
    /// the stream was opening toward.
    pub(super) fn opening_binding(&self, opening_progress: f64) -> Option<(ButtonId, Action)> {
        (if opening_progress > 0.0 {
            &self.plan.positive
        } else {
            &self.plan.negative
        })
        .clone()
    }

    fn committed_binding(&self) -> Option<(ButtonId, Action)> {
        [self.plan.positive.as_ref(), self.plan.negative.as_ref()]
            .into_iter()
            .flatten()
            .find(|(button, _)| *button == self.committed)
            .cloned()
    }
}

fn frame_centroid(contacts: &[TouchContact]) -> (i64, i64) {
    let count = i64::try_from(contacts.len()).unwrap_or(1);
    let sum = contacts.iter().fold((0_i64, 0_i64), |(sx, sy), contact| {
        (sx + i64::from(contact.x_um), sy + i64::from(contact.y_um))
    });
    (sum.0 / count, sum.1 / count)
}

/// Mean contact distance from the centroid — the recognizer's spread, as a
/// continuous quantity for progress tracking.
#[expect(
    clippy::cast_precision_loss,
    reason = "contact positions stay far below 2^53 micrometres"
)]
fn frame_spread(contacts: &[TouchContact], centroid: (i64, i64)) -> f64 {
    contacts
        .iter()
        .map(|contact| {
            let (dx, dy) = (
                i64::from(contact.x_um) - centroid.0,
                i64::from(contact.y_um) - centroid.1,
            );
            ((dx as f64) * (dx as f64) + (dy as f64) * (dy as f64)).sqrt()
        })
        .sum::<f64>()
        / contacts.len() as f64
}
