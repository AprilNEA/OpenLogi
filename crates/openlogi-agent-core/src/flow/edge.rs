//! Screen-edge geometry and crossing detection for Flow.
//!
//! Display bounds and cursor positions share one global desktop coordinate
//! space in which the displays tile without overlap: points on macOS,
//! root-window pixels on X11, and the virtual-screen coordinates that Win32
//! reports to the agent on Windows. Per-monitor DIP scaling does not apply
//! here, because displays with different scale factors would overlap in it.
//!
//! This module computes which display edges lead off this computer and turns
//! a sustained or fast approach to one of them into a single crossing event.
//! It performs no display enumeration or input I/O.

mod detector;
mod geometry;

pub use detector::{
    ArmedSides, EdgeCrossing, EdgeDetector, EdgeDetectorParams, InvalidEdgeDetectorParams, Velocity,
};
pub use geometry::{DisplayRect, EdgeSegment, ExposedEdges};

/// A point in the desktop coordinate space of [`DisplayRect`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DesktopPoint {
    /// Horizontal coordinate, increasing rightward.
    pub x: f64,
    /// Vertical coordinate, increasing downward.
    pub y: f64,
}

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
