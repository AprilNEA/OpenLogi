use super::EdgeSide;

/// A validated display rectangle in global logical-pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl DisplayRect {
    /// Construct a finite rectangle with positive width and height.
    ///
    /// Returns `None` for non-finite coordinates or non-positive dimensions.
    #[must_use]
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Option<Self> {
        (x.is_finite()
            && y.is_finite()
            && width.is_finite()
            && height.is_finite()
            && width > 0.0
            && height > 0.0)
            .then_some(Self {
                x,
                y,
                width,
                height,
            })
    }

    /// Minimum x coordinate.
    #[must_use]
    pub const fn x(self) -> f64 {
        self.x
    }

    /// Minimum y coordinate.
    #[must_use]
    pub const fn y(self) -> f64 {
        self.y
    }

    /// Rectangle width.
    #[must_use]
    pub const fn width(self) -> f64 {
        self.width
    }

    /// Rectangle height.
    #[must_use]
    pub const fn height(self) -> f64 {
        self.height
    }

    const fn max_x(self) -> f64 {
        self.x + self.width
    }

    const fn max_y(self) -> f64 {
        self.y + self.height
    }

    const fn side_span(self, side: EdgeSide) -> (f64, f64, f64) {
        match side {
            EdgeSide::Left => (self.x, self.y, self.max_y()),
            EdgeSide::Right => (self.max_x(), self.y, self.max_y()),
            EdgeSide::Top => (self.y, self.x, self.max_x()),
            EdgeSide::Bottom => (self.max_y(), self.x, self.max_x()),
        }
    }

    fn blocks_side(self, other: Self, side: EdgeSide) -> bool {
        match side {
            EdgeSide::Left => other.x < self.x && other.max_x() >= self.x,
            EdgeSide::Right => other.x <= self.max_x() && other.max_x() > self.max_x(),
            EdgeSide::Top => other.y < self.y && other.max_y() >= self.y,
            EdgeSide::Bottom => other.y <= self.max_y() && other.max_y() > self.max_y(),
        }
    }

    const fn overlap_span(other: Self, side: EdgeSide) -> (f64, f64) {
        match side {
            EdgeSide::Left | EdgeSide::Right => (other.y, other.max_y()),
            EdgeSide::Top | EdgeSide::Bottom => (other.x, other.max_x()),
        }
    }
}

/// Integration seam for platform display enumeration.
///
/// Platform implementations should convert native monitor bounds into the
/// coordinate convention used by [`DisplayRect`]. The engine itself consumes
/// only the returned rectangles, so enumeration and reconfiguration callbacks
/// remain outside this module.
pub trait DisplayGeometryProvider {
    /// Platform-specific enumeration error.
    type Error;

    /// Return the current display rectangles in global logical-pixel coordinates.
    fn display_rects(&self) -> Result<Vec<DisplayRect>, Self::Error>;
}

/// One exposed portion of a display edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeSegment {
    side: EdgeSide,
    coordinate: f64,
    start: f64,
    end: f64,
}

impl EdgeSegment {
    /// Side this segment belongs to.
    #[must_use]
    pub const fn side(self) -> EdgeSide {
        self.side
    }

    /// Fixed coordinate: x for left/right, y for top/bottom.
    #[must_use]
    pub const fn coordinate(self) -> f64 {
        self.coordinate
    }

    /// Inclusive lower coordinate along the edge.
    #[must_use]
    pub const fn start(self) -> f64 {
        self.start
    }

    /// Inclusive upper coordinate along the edge.
    #[must_use]
    pub const fn end(self) -> f64 {
        self.end
    }
}

/// Exposed per-display edge segments for one virtual desktop layout.
///
/// A neighboring display removes only the overlapping interval of the side it
/// touches. Gaps remain exposed. Segments are ordered by side, then by their
/// along-edge start and perpendicular coordinate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExposedEdges {
    segments: Vec<EdgeSegment>,
}

impl ExposedEdges {
    /// Compute exposed segments from the current display layout.
    #[must_use]
    pub fn from_displays(displays: &[DisplayRect]) -> Self {
        let mut unique_displays = Vec::with_capacity(displays.len());
        for display in displays {
            if !unique_displays.contains(display) {
                unique_displays.push(*display);
            }
        }

        let mut segments = Vec::new();
        for (display_index, display) in unique_displays.iter().copied().enumerate() {
            for side in EdgeSide::ALL {
                let (coordinate, start, end) = display.side_span(side);
                let covered = unique_displays
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|(other_index, other)| {
                        *other_index != display_index && display.blocks_side(*other, side)
                    })
                    .map(|(_, other)| DisplayRect::overlap_span(other, side));

                segments.extend(exposed_intervals(start, end, covered).map(
                    |(segment_start, segment_end)| EdgeSegment {
                        side,
                        coordinate,
                        start: segment_start,
                        end: segment_end,
                    },
                ));
            }
        }
        segments.sort_by(|left, right| {
            left.side
                .index()
                .cmp(&right.side.index())
                .then_with(|| left.start.total_cmp(&right.start))
                .then_with(|| left.coordinate.total_cmp(&right.coordinate))
                .then_with(|| left.end.total_cmp(&right.end))
        });
        Self { segments }
    }

    /// Every exposed segment in deterministic side order.
    #[must_use]
    pub fn segments(&self) -> &[EdgeSegment] {
        &self.segments
    }

    /// Exposed segments for one side, in normalization order.
    pub fn for_side(&self, side: EdgeSide) -> impl Iterator<Item = &EdgeSegment> {
        self.segments
            .iter()
            .filter(move |segment| segment.side == side)
    }
}

fn exposed_intervals(
    start: f64,
    end: f64,
    covered: impl IntoIterator<Item = (f64, f64)>,
) -> impl Iterator<Item = (f64, f64)> {
    let mut covered: Vec<_> = covered
        .into_iter()
        .map(|(covered_start, covered_end)| (covered_start.max(start), covered_end.min(end)))
        .filter(|(covered_start, covered_end)| covered_start < covered_end)
        .collect();
    covered.sort_by(|left, right| left.0.total_cmp(&right.0));

    let mut exposed = Vec::new();
    let mut cursor = start;
    for (covered_start, covered_end) in covered {
        if covered_end <= cursor {
            continue;
        }
        if covered_start > cursor {
            exposed.push((cursor, covered_start));
        }
        cursor = cursor.max(covered_end);
        if cursor >= end {
            break;
        }
    }
    if cursor < end {
        exposed.push((cursor, end));
    }
    exposed.into_iter()
}

#[cfg(test)]
mod tests;
