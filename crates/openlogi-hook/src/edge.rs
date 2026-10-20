//! Platform-neutral screen-edge geometry and crossing detection for Flow.
//!
//! Platform code supplies display rectangles and timestamped cursor positions;
//! this module computes the handoff edges and turns a sustained or fast approach
//! into one crossing event. It performs no display enumeration or input I/O.

mod detector;
mod geometry;

pub use detector::{ArmedSides, EdgeCrossing, EdgeDetector, EdgeDetectorParams, Velocity};
pub use geometry::{DisplayGeometryProvider, DisplayRect, EdgeSegment, ExposedEdges};

/// A side of a display in a coordinate space where x increases rightward and y
/// increases downward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EdgeSide {
    /// The minimum-x side.
    Left,
    /// The maximum-x side.
    Right,
    /// The minimum-y side.
    Top,
    /// The maximum-y side.
    Bottom,
}

impl EdgeSide {
    pub(super) const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Top => 2,
            Self::Bottom => 3,
        }
    }
}
