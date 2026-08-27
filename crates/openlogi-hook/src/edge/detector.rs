use std::cmp::Ordering;
use std::time::Duration;

use crate::CursorPosition;

use super::{EdgeSegment, EdgeSide, ExposedEdges};

/// Set of display sides enabled for Flow handoff.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArmedSides(u8);

impl ArmedSides {
    /// No sides armed.
    pub const NONE: Self = Self(0);
    /// Every side armed.
    pub const ALL: Self = Self(0b1111);

    /// Construct a set from side values.
    #[must_use]
    pub fn from_sides(sides: impl IntoIterator<Item = EdgeSide>) -> Self {
        sides
            .into_iter()
            .fold(Self::NONE, |set, side| Self(set.0 | (1 << side.index())))
    }

    /// Whether one side is armed.
    #[must_use]
    pub const fn contains(self, side: EdgeSide) -> bool {
        self.0 & (1 << side.index()) != 0
    }
}

/// Cursor velocity in logical pixels per second.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Velocity {
    /// Horizontal velocity; positive points right.
    pub x: f64,
    /// Vertical velocity; positive points down.
    pub y: f64,
}

impl EdgeSide {
    fn outward_velocity(self, velocity: Velocity) -> f64 {
        match self {
            Self::Left => -velocity.x,
            Self::Right => velocity.x,
            Self::Top => -velocity.y,
            Self::Bottom => velocity.y,
        }
    }
}

/// One screen-edge crossing reported to the Flow orchestrator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeCrossing {
    /// Exiting side.
    pub side: EdgeSide,
    /// Position in `0..=1` over the cumulative exposed length of this side.
    ///
    /// Disconnected segments are traversed in [`ExposedEdges::for_side`] order;
    /// physical gaps consume no range. A receiver can apply the same cumulative
    /// length rule to map this value proportionally onto its own exposed segments.
    pub t: f64,
    /// Finite-difference velocity when this approach first touched the edge.
    pub velocity: Velocity,
}

/// Tunable thresholds for [`EdgeDetector`].
///
/// Distance and speed fields are expected to be finite and non-negative. A
/// zero dwell or velocity threshold permits immediate contact triggering; a
/// zero cooldown disables rate limiting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeDetectorParams {
    /// Maximum normal distance from a segment that counts as edge contact.
    /// Default: 1 logical pixel.
    pub edge_tolerance: f64,
    /// Continuous edge contact required for a low-speed crossing.
    /// Default: 250 ms.
    pub dwell_time: Duration,
    /// Outward approach velocity that triggers immediately, in logical pixels
    /// per second. Default: 900 px/s.
    pub arrival_velocity_threshold: f64,
    /// Distance from every armed exposed segment required before another
    /// approach can trigger. Default: 12 logical pixels.
    pub rearm_distance: f64,
    /// Minimum interval between crossing events, even after leaving the edge.
    /// Default: 500 ms.
    pub cooldown: Duration,
}

impl Default for EdgeDetectorParams {
    fn default() -> Self {
        Self {
            edge_tolerance: 1.0,
            dwell_time: Duration::from_millis(250),
            arrival_velocity_threshold: 900.0,
            rearm_distance: 12.0,
            cooldown: Duration::from_millis(500),
        }
    }
}

/// Stateful detector that turns timestamped cursor samples into edge crossings.
///
/// Timestamps are durations from any monotonic caller-owned epoch. A fast
/// outward arrival or a continuous dwell triggers. Afterward, the detector
/// remains latched until the cursor moves at least `rearm_distance` from every
/// armed edge; `cooldown` independently limits event frequency.
pub struct EdgeDetector {
    edges: ExposedEdges,
    armed_sides: ArmedSides,
    params: EdgeDetectorParams,
    previous: Option<TimedPosition>,
    contact: Option<Contact>,
    latched: bool,
    last_crossing: Option<Duration>,
}

impl EdgeDetector {
    /// Construct a detector for one display layout and configured side set.
    #[must_use]
    pub const fn new(
        edges: ExposedEdges,
        armed_sides: ArmedSides,
        params: EdgeDetectorParams,
    ) -> Self {
        Self {
            edges,
            armed_sides,
            params,
            previous: None,
            contact: None,
            latched: false,
            last_crossing: None,
        }
    }

    /// Replace display geometry after a platform reconfiguration notification.
    ///
    /// Any in-progress contact is discarded; a prior crossing's latch and
    /// cooldown are retained so refreshing unchanged bounds cannot retrigger
    /// while the cursor remains pinned.
    pub fn set_edges(&mut self, edges: ExposedEdges) {
        self.edges = edges;
        self.previous = None;
        self.contact = None;
    }

    /// Consume one cursor sample and return at most one crossing.
    ///
    /// At a corner, an existing contact remains selected. A new contact chooses
    /// the side with the greater outward velocity; an exact tie uses the stable
    /// priority Left, Right, Top, Bottom.
    pub fn update(
        &mut self,
        position: CursorPosition,
        timestamp: Duration,
    ) -> Option<EdgeCrossing> {
        let velocity = self
            .previous
            .and_then(|previous| velocity_between(previous, position, timestamp))
            .unwrap_or_default();
        self.previous = Some(TimedPosition {
            position,
            timestamp,
        });

        if self.latched {
            if !is_near_armed_edge(
                &self.edges,
                position,
                self.params.rearm_distance,
                self.armed_sides,
            ) {
                self.latched = false;
            }
            self.contact = None;
            return None;
        }

        let candidates = candidates(
            &self.edges,
            position,
            self.params.edge_tolerance,
            self.armed_sides,
        );
        let preferred_segment = self.contact.map(|contact| contact.segment_index);
        let Some(candidate) = select_candidate(&candidates, preferred_segment, velocity) else {
            self.contact = None;
            return None;
        };

        let is_new_contact = self
            .contact
            .is_none_or(|contact| contact.segment_index != candidate.segment_index);
        if is_new_contact {
            self.contact = Some(Contact {
                segment_index: candidate.segment_index,
                started_at: timestamp,
                approach_velocity: velocity,
            });
        }
        let contact = self.contact?;
        let dwell_reached = timestamp
            .checked_sub(contact.started_at)
            .is_some_and(|elapsed| elapsed >= self.params.dwell_time);
        let fast_arrival = is_new_contact
            && candidate.side.outward_velocity(contact.approach_velocity)
                >= self.params.arrival_velocity_threshold;
        let cooldown_finished = self.last_crossing.is_none_or(|last_crossing| {
            timestamp
                .checked_sub(last_crossing)
                .is_some_and(|elapsed| elapsed >= self.params.cooldown)
        });
        if !(cooldown_finished && (fast_arrival || dwell_reached)) {
            return None;
        }

        let crossing = EdgeCrossing {
            side: candidate.side,
            t: normalized_position(&self.edges, candidate.segment_index, position),
            velocity: contact.approach_velocity,
        };
        self.contact = None;
        self.latched = true;
        self.last_crossing = Some(timestamp);
        Some(crossing)
    }
}

#[derive(Clone, Copy)]
struct TimedPosition {
    position: CursorPosition,
    timestamp: Duration,
}

fn velocity_between(
    previous: TimedPosition,
    position: CursorPosition,
    timestamp: Duration,
) -> Option<Velocity> {
    let seconds = timestamp.checked_sub(previous.timestamp)?.as_secs_f64();
    (seconds > 0.0).then_some(Velocity {
        x: (position.x - previous.position.x) / seconds,
        y: (position.y - previous.position.y) / seconds,
    })
}

#[derive(Clone, Copy)]
struct Contact {
    segment_index: usize,
    started_at: Duration,
    approach_velocity: Velocity,
}

#[derive(Clone, Copy)]
struct Candidate {
    segment_index: usize,
    side: EdgeSide,
    normal_distance: f64,
}

fn candidates(
    edges: &ExposedEdges,
    position: CursorPosition,
    tolerance: f64,
    armed_sides: ArmedSides,
) -> Vec<Candidate> {
    edges
        .segments()
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, segment)| {
            armed_sides.contains(segment.side())
                && contains_with_tolerance(*segment, position, tolerance)
        })
        .map(|(segment_index, segment)| Candidate {
            segment_index,
            side: segment.side(),
            normal_distance: (normal_coordinate(segment, position) - segment.coordinate()).abs(),
        })
        .collect()
}

fn select_candidate(
    candidates: &[Candidate],
    preferred_segment: Option<usize>,
    velocity: Velocity,
) -> Option<Candidate> {
    if let Some(preferred) = preferred_segment
        && let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.segment_index == preferred)
    {
        return Some(*candidate);
    }

    candidates.iter().copied().min_by(|left, right| {
        let left_outward = left.side.outward_velocity(velocity).max(0.0);
        let right_outward = right.side.outward_velocity(velocity).max(0.0);
        match right_outward.total_cmp(&left_outward) {
            Ordering::Equal => left
                .side
                .index()
                .cmp(&right.side.index())
                .then_with(|| left.normal_distance.total_cmp(&right.normal_distance))
                .then_with(|| left.segment_index.cmp(&right.segment_index)),
            ordering => ordering,
        }
    })
}

fn normalized_position(
    edges: &ExposedEdges,
    segment_index: usize,
    position: CursorPosition,
) -> f64 {
    let segment = edges.segments()[segment_index];
    let total_length: f64 = edges
        .for_side(segment.side())
        .copied()
        .map(segment_length)
        .sum();
    let preceding_length: f64 = edges.segments()[..segment_index]
        .iter()
        .copied()
        .filter(|part| part.side() == segment.side())
        .map(segment_length)
        .sum();
    let local =
        along_coordinate(segment, position).clamp(segment.start(), segment.end()) - segment.start();
    ((preceding_length + local) / total_length).clamp(0.0, 1.0)
}

fn is_near_armed_edge(
    edges: &ExposedEdges,
    position: CursorPosition,
    distance: f64,
    armed_sides: ArmedSides,
) -> bool {
    edges.segments().iter().copied().any(|segment| {
        armed_sides.contains(segment.side()) && distance_to(segment, position) < distance
    })
}

fn segment_length(segment: EdgeSegment) -> f64 {
    segment.end() - segment.start()
}

fn along_coordinate(segment: EdgeSegment, position: CursorPosition) -> f64 {
    match segment.side() {
        EdgeSide::Left | EdgeSide::Right => position.y,
        EdgeSide::Top | EdgeSide::Bottom => position.x,
    }
}

fn normal_coordinate(segment: EdgeSegment, position: CursorPosition) -> f64 {
    match segment.side() {
        EdgeSide::Left | EdgeSide::Right => position.x,
        EdgeSide::Top | EdgeSide::Bottom => position.y,
    }
}

fn contains_with_tolerance(segment: EdgeSegment, position: CursorPosition, tolerance: f64) -> bool {
    let along = along_coordinate(segment, position);
    (normal_coordinate(segment, position) - segment.coordinate()).abs() <= tolerance
        && along >= segment.start()
        && along <= segment.end()
}

fn distance_to(segment: EdgeSegment, position: CursorPosition) -> f64 {
    let normal = (normal_coordinate(segment, position) - segment.coordinate()).abs();
    let along = along_coordinate(segment, position);
    let along_distance = if along < segment.start() {
        segment.start() - along
    } else if along > segment.end() {
        along - segment.end()
    } else {
        0.0
    };
    normal.hypot(along_distance)
}

#[cfg(test)]
mod tests;
