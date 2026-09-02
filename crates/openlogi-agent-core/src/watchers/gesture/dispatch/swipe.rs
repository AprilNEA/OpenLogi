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
//! Pinch pairs stream the same way — the motion follows the bound actions'
//! consumer, and spread change drives the scale motion's progress, which
//! the Dock maps to Show Desktop (closing) and Launchpad, or its macOS 26+
//! replacement (spreading). A pinch bound to a zoom action keeps the
//! discrete dispatch, matching the pair defaults.

use std::collections::BTreeMap;

use openlogi_core::binding::{Action, ButtonId};
use openlogi_core::touchpad::{TouchContact, TouchFrame};
use openlogi_inject::{GestureMotion, GesturePhase};

/// One pad-width of travel equals one progress unit; the constants mirror the
/// target Casa Touch pad (2775 × 1786 @ 600 dpi ≈ 117 × 76 mm) until real
/// geometry is plumbed through.
const HORIZONTAL_PAD_TRAVEL_UM: f64 = 117_000.0;
const VERTICAL_PAD_TRAVEL_UM: f64 = 75_600.0;
/// Spread change equal to one progress unit, per pinch finger count and
/// direction: a close runs over the short travel between resting spread and
/// fingers-touching, a spread over the wide travel it has room for, so the
/// two directions normalize separately. Hardware-tuned on the Casa Touch.
const TWO_FINGER_PINCH_CLOSE_TRAVEL_UM: f64 = 15_000.0;
const TWO_FINGER_PINCH_OPEN_TRAVEL_UM: f64 = 30_000.0;
const FOUR_FINGER_PINCH_CLOSE_TRAVEL_UM: f64 = 10_000.0;
const FOUR_FINGER_PINCH_OPEN_TRAVEL_UM: f64 = 25_000.0;

/// One routed step of a session's swipe stream.
#[derive(Debug, Default, PartialEq)]
pub(super) enum SwipeOutput {
    /// Nothing to stream for this frame.
    #[default]
    Idle,
    /// `progress` is the opening frame's travel; later frames stream deltas.
    Begin {
        motion: GestureMotion,
        progress: f64,
    },
    Advance {
        motion: GestureMotion,
        delta: f64,
    },
    Finish {
        motion: GestureMotion,
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

impl SwipeEnd {
    /// Magnify has no spring-back: a release ends the zoom where it stands,
    /// whatever the release direction.
    pub(super) fn as_gesture_phase(self) -> GesturePhase {
        match self {
            SwipeEnd::AtRelease => GesturePhase::End,
            SwipeEnd::Cancelled => GesturePhase::Cancel,
        }
    }
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
/// progress sign it commits at. Flipping a sign anchor re-anchors the table
/// if hardware verification disagrees.
fn swipe_consumer(action: &Action) -> Option<(GestureMotion, i8)> {
    /// Fingers right commit the next Space.
    const NEXT_DESKTOP_SIGN: i8 = 1;
    /// Spreading drives positive scale progress, which the Dock resolves as
    /// Show Desktop; closing drives negative, resolving as Launchpad (its
    /// macOS 26+ replacement). Hardware-verified on macOS 27 — the opposite
    /// of the native trackpad's finger mapping.
    const SHOW_DESKTOP_SIGN: i8 = 1;
    /// Spreading commits zoom-in: magnification grows content.
    const ZOOM_IN_SIGN: i8 = 1;
    match action {
        Action::NextDesktop => Some((GestureMotion::Horizontal, NEXT_DESKTOP_SIGN)),
        Action::PreviousDesktop => Some((GestureMotion::Horizontal, -NEXT_DESKTOP_SIGN)),
        Action::MissionControl => Some((GestureMotion::Vertical, 1)),
        Action::AppExpose => Some((GestureMotion::Vertical, -1)),
        Action::ShowDesktop => Some((GestureMotion::Pinch, SHOW_DESKTOP_SIGN)),
        Action::LaunchpadShow => Some((GestureMotion::Pinch, -SHOW_DESKTOP_SIGN)),
        Action::ZoomIn => Some((GestureMotion::Zoom, ZOOM_IN_SIGN)),
        Action::ZoomOut => Some((GestureMotion::Zoom, -ZOOM_IN_SIGN)),
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
    /// Travel along the axis that equals one progress unit, micrometres —
    /// pinch pairs carry both directions' divisors.
    travel: AxisTravel,
    motion: GestureMotion,
    /// Whether finger travel maps onto motion progress negated, solved from
    /// the bindings so each bound side commits its own action.
    flipped: bool,
    /// The binding each progress sign commits; an absent sign clamps at zero.
    positive: Option<(ButtonId, Action)>,
    negative: Option<(ButtonId, Action)>,
}

/// The travel that equals one progress unit. Pinch travel is
/// direction-split: closing and spreading normalize against their own
/// divisors, chosen by each delta's physical direction.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AxisTravel {
    Linear(f64),
    Pinch { close: f64, spread: f64 },
}

impl AxisTravel {
    fn divisor_for(self, travel_um: f64) -> f64 {
        match self {
            AxisTravel::Linear(travel) => travel,
            AxisTravel::Pinch { close, spread } => {
                if travel_um < 0.0 {
                    close
                } else {
                    spread
                }
            }
        }
    }
}

impl SwipeStreamPlan {
    /// Plan the pair `trigger` belongs to from a stroke's frozen bindings.
    pub(super) fn for_trigger(
        trigger: ButtonId,
        bindings: &BTreeMap<ButtonId, Action>,
    ) -> Option<Self> {
        let (sibling, axis, own_side) = pair_sibling(trigger)?;
        let travel = match axis {
            GestureAxis::Horizontal => AxisTravel::Linear(HORIZONTAL_PAD_TRAVEL_UM),
            GestureAxis::Vertical => AxisTravel::Linear(VERTICAL_PAD_TRAVEL_UM),
            GestureAxis::Pinch
                if matches!(
                    trigger,
                    ButtonId::TouchpadTwoFingerPinchIn | ButtonId::TouchpadTwoFingerPinchOut
                ) =>
            {
                AxisTravel::Pinch {
                    close: TWO_FINGER_PINCH_CLOSE_TRAVEL_UM,
                    spread: TWO_FINGER_PINCH_OPEN_TRAVEL_UM,
                }
            }
            GestureAxis::Pinch => AxisTravel::Pinch {
                close: FOUR_FINGER_PINCH_CLOSE_TRAVEL_UM,
                spread: FOUR_FINGER_PINCH_OPEN_TRAVEL_UM,
            },
        };
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
            travel,
            motion: motion?,
            flipped: demanded?,
            positive,
            negative,
        })
    }

    /// The lowest progress the pair's bindings may commit at.
    fn lower_bound(&self) -> f64 {
        if self.negative.is_some() {
            f64::NEG_INFINITY
        } else {
            0.0
        }
    }

    /// The highest progress the pair's bindings may commit at.
    fn upper_bound(&self) -> f64 {
        if self.positive.is_some() {
            f64::INFINITY
        } else {
            0.0
        }
    }

    /// Zoom streams decline the banked seed: an accumulated catch-up is a
    /// visible pop in content scale, unlike a dock reveal's positional
    /// animation where the jump reads as responsiveness. A fast zoom flick
    /// instead falls to its discrete fallback, which works on every host.
    fn takes_banked_seed(&self) -> bool {
        self.motion != GestureMotion::Zoom
    }

    /// Whether streaming this plan needs the macOS 27 DockSwipe bridge.
    /// Magnify reads plain CGEvent fields and works wherever the OS runs.
    pub(super) fn needs_dock_swipe_bridge(&self) -> bool {
        self.motion != GestureMotion::Zoom
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
/// commit frame and advanced by every later frame of the stroke. A pinch
/// commit arrives with the stroke's banked spread, so the travel the
/// recognizer threshold consumed still counts: a fast close that crosses
/// the threshold near the end of its range begins the stream with real
/// progress instead of falling to a discrete dispatch that some consumers
/// cannot even perform (Launchpad is a no-op on macOS 26+).
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
    /// Anchor the stream at the commit frame. `banked_spread_um` is the
    /// stroke's accumulated pinch spread at that moment; when it maps to
    /// in-bounds progress the returned `Begin` opens the animation on the
    /// commit frame itself.
    pub(super) fn new(
        frame: &TouchFrame,
        plan: SwipeStreamPlan,
        committed: ButtonId,
        banked_spread_um: f64,
    ) -> (Self, Option<SwipeOutput>) {
        let centroid = frame_centroid(frame.contacts());
        let mut seed = 0.0;
        if banked_spread_um != 0.0 && plan.takes_banked_seed() {
            let raw = banked_spread_um / plan.travel.divisor_for(banked_spread_um);
            let mapped = if plan.flipped { -raw } else { raw };
            seed = mapped.clamp(plan.lower_bound(), plan.upper_bound());
        }
        let opened = seed != 0.0;
        let begin = opened.then_some(SwipeOutput::Begin {
            motion: plan.motion,
            progress: seed,
        });
        (
            Self {
                plan,
                committed,
                opened,
                contact_ids: frame.contacts().iter().map(|contact| contact.id).collect(),
                centroid_um: centroid,
                spread_um: frame_spread(frame.contacts(), centroid),
                progress: seed,
            },
            begin,
        )
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
            let travel = match self.plan.axis {
                GestureAxis::Horizontal => (centroid.0 - self.centroid_um.0) as f64,
                // Window y grows downward, but vertical progress is
                // positive upward.
                GestureAxis::Vertical => -(centroid.1 - self.centroid_um.1) as f64,
                GestureAxis::Pinch => spread - self.spread_um,
            };
            let raw = travel / self.plan.travel.divisor_for(travel);
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
        let (lower, upper) = (self.plan.lower_bound(), self.plan.upper_bound());
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

/// Banks a stroke's spread change frame by frame, so a pinch commit can
/// seed its stream with the travel the recognizer threshold consumed.
/// Contact-set changes skip their frame's delta, exactly like the stream's
/// own tracking, so a finger landing mid-stroke cannot fake banked travel.
#[derive(Default)]
pub(super) struct SpreadBank {
    anchor: Option<(Box<[u8]>, f64)>,
    banked_um: f64,
}

impl SpreadBank {
    pub(super) fn fold(&mut self, frame: &TouchFrame) {
        let centroid = frame_centroid(frame.contacts());
        let spread = frame_spread(frame.contacts(), centroid);
        let ids: Vec<u8> = frame.contacts().iter().map(|contact| contact.id).collect();
        let delta = match &self.anchor {
            Some((anchor_ids, anchor_spread)) if ids.as_slice() == &**anchor_ids => {
                Some(spread - *anchor_spread)
            }
            _ => None,
        };
        if let Some(delta) = delta {
            self.banked_um += delta;
        }
        self.anchor = Some((ids.into_boxed_slice(), spread));
    }

    /// Withdraw the banked spread, closing the bank for this stroke.
    pub(super) fn take(&mut self) -> f64 {
        std::mem::take(&mut self.banked_um)
    }

    pub(super) fn reset(&mut self) {
        self.anchor = None;
        self.banked_um = 0.0;
    }
}
