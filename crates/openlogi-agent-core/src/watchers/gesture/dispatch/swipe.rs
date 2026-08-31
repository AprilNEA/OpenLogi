//! Pair-planned native swipe streaming.
//!
//! A DockSwipe animation belongs to a swipe *pair* — the left/right or
//! up/down directions of one finger count — not to a single direction: the
//! recognizer commits one direction, but every frame after the commit drives
//! the pair's one shared progress stream, so an unbound side still follows
//! the finger and springs back at release instead of dying silently.
//!
//! A pair streams only when the system can honor every bound direction: each
//! bound action needs a native swipe-commit consumer (desktop switching on
//! the horizontal motion, Mission Control / App Exposé on the vertical one)
//! and all of them must share one motion, with the finger-travel mapping
//! solved so each bound side commits its own binding. Anything else — no
//! binding on the pair, a keyboard-style action, mixed motions, or the same
//! commit sign demanded twice — keeps the pair discrete, so an animation can
//! never commit an outcome the user did not bind. A bound pair whose one
//! side is unbound clamps progress at zero on that side: the finger can pull
//! the animation back, never push it past its start. Pinch triggers have no
//! swipe consumer at all (`ShowDesktop` and `LaunchpadShow` are pinch-native
//! on the Dock) and always stay discrete.

use std::collections::BTreeMap;

use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::touchpad::{TouchContact, TouchFrame};
use openlogi_inject::DockSwipeMotion;

/// One pad-width of travel equals one progress unit; the constants mirror the
/// target Casa Touch pad (2775 × 1786 @ 600 dpi ≈ 117 × 76 mm) until real
/// geometry is plumbed through.
const HORIZONTAL_PAD_TRAVEL_UM: f64 = 117_000.0;
const VERTICAL_PAD_TRAVEL_UM: f64 = 75_600.0;

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

/// Direction along a swipe pair's finger axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    /// Left on the horizontal axis, down on the vertical one.
    Negative,
    /// Right on the horizontal axis, up on the vertical one.
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

/// The finger axis a swipe pair lives on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwipeAxis {
    Horizontal,
    Vertical,
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

/// The sibling trigger on the same finger axis, the pair's axis, and this
/// trigger's side along it.
fn swipe_sibling(trigger: ButtonId) -> Option<(ButtonId, SwipeAxis, Side)> {
    use ButtonId::{
        TouchpadFourFingerSwipeDown, TouchpadFourFingerSwipeLeft, TouchpadFourFingerSwipeRight,
        TouchpadFourFingerSwipeUp, TouchpadThreeFingerSwipeDown, TouchpadThreeFingerSwipeLeft,
        TouchpadThreeFingerSwipeRight, TouchpadThreeFingerSwipeUp,
    };
    Some(match trigger {
        TouchpadThreeFingerSwipeRight => (
            TouchpadThreeFingerSwipeLeft,
            SwipeAxis::Horizontal,
            Side::Positive,
        ),
        TouchpadThreeFingerSwipeLeft => (
            TouchpadThreeFingerSwipeRight,
            SwipeAxis::Horizontal,
            Side::Negative,
        ),
        TouchpadThreeFingerSwipeUp => (
            TouchpadThreeFingerSwipeDown,
            SwipeAxis::Vertical,
            Side::Positive,
        ),
        TouchpadThreeFingerSwipeDown => (
            TouchpadThreeFingerSwipeUp,
            SwipeAxis::Vertical,
            Side::Negative,
        ),
        TouchpadFourFingerSwipeRight => (
            TouchpadFourFingerSwipeLeft,
            SwipeAxis::Horizontal,
            Side::Positive,
        ),
        TouchpadFourFingerSwipeLeft => (
            TouchpadFourFingerSwipeRight,
            SwipeAxis::Horizontal,
            Side::Negative,
        ),
        TouchpadFourFingerSwipeUp => (
            TouchpadFourFingerSwipeDown,
            SwipeAxis::Vertical,
            Side::Positive,
        ),
        TouchpadFourFingerSwipeDown => (
            TouchpadFourFingerSwipeUp,
            SwipeAxis::Vertical,
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
    axis: SwipeAxis,
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
        let (sibling, axis, own_side) = swipe_sibling(trigger)?;
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

/// A committed swipe streaming its pair's animation, anchored at the commit
/// frame and advanced by every later frame of the stroke.
pub(super) struct ActiveSwipe {
    plan: SwipeStreamPlan,
    /// The direction the recognizer committed on, whose binding a
    /// never-opened stream falls back to at release.
    committed: ButtonId,
    opened: bool,
    contact_ids: Box<[u8]>,
    centroid_um: (i64, i64),
    /// Clamped progress accumulated since the stream anchored.
    progress: f64,
}

impl ActiveSwipe {
    pub(super) fn new(frame: &TouchFrame, plan: SwipeStreamPlan, committed: ButtonId) -> Self {
        Self {
            plan,
            committed,
            opened: false,
            contact_ids: frame.contacts().iter().map(|contact| contact.id).collect(),
            centroid_um: frame_centroid(frame.contacts()),
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
        reason = "centroid deltas are bounded by the pad size; f64 precision is ample"
    )]
    pub(super) fn advance(&mut self, frame: &TouchFrame) -> Option<SwipeOutput> {
        let contact_ids: Vec<u8> = frame.contacts().iter().map(|contact| contact.id).collect();
        let centroid = frame_centroid(frame.contacts());
        let mut mapped = 0.0;
        if contact_ids.as_slice() == &*self.contact_ids {
            let (dx, dy) = (
                centroid.0 - self.centroid_um.0,
                centroid.1 - self.centroid_um.1,
            );
            let raw = match self.plan.axis {
                SwipeAxis::Horizontal => dx as f64 / HORIZONTAL_PAD_TRAVEL_UM,
                // Window y grows downward, but vertical progress is
                // positive upward.
                SwipeAxis::Vertical => -dy as f64 / VERTICAL_PAD_TRAVEL_UM,
            };
            mapped = if self.plan.flipped { -raw } else { raw };
        }
        self.contact_ids = contact_ids.into_boxed_slice();
        self.centroid_um = centroid;
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
