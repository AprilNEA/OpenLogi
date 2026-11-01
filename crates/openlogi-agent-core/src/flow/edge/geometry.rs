use super::EdgeSide;

/// A display rectangle in the global desktop coordinate space (see the
/// module docs), where x grows rightward and y downward.
///
/// Construction guarantees finite bounds and a positive size, including the
/// computed far edges, so every edge derived from it is finite.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayRect {
    x: f64,
    y: f64,
    max_x: f64,
    max_y: f64,
}

impl DisplayRect {
    /// A rectangle at `(x, y)` with the given size.
    ///
    /// Returns `None` unless the coordinates, the size, and both far edges
    /// (`x + width`, `y + height`) are finite and the rectangle has a positive
    /// area after rounding.
    #[must_use]
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Option<Self> {
        // `+ 0.0` folds -0.0 into 0.0: collinear edges are grouped by an exact
        // coordinate, and `total_cmp` tells the two zeros apart.
        let (x, y) = (x + 0.0, y + 0.0);
        let (max_x, max_y) = (x + width, y + height);
        let finite = [x, y, width, height, max_x, max_y]
            .iter()
            .all(|value| value.is_finite());
        (finite && max_x > x && max_y > y).then_some(Self { x, y, max_x, max_y })
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
        self.max_x - self.x
    }

    /// Rectangle height.
    #[must_use]
    pub const fn height(self) -> f64 {
        self.max_y - self.y
    }

    const fn side_span(self, side: EdgeSide) -> (f64, f64, f64) {
        match side {
            EdgeSide::Left => (self.x, self.y, self.max_y),
            EdgeSide::Right => (self.max_x, self.y, self.max_y),
            EdgeSide::Top => (self.y, self.x, self.max_x),
            EdgeSide::Bottom => (self.max_y, self.x, self.max_x),
        }
    }

    fn blocks_side(self, other: Self, side: EdgeSide) -> bool {
        match side {
            EdgeSide::Left => other.x < self.x && other.max_x >= self.x,
            EdgeSide::Right => other.x <= self.max_x && other.max_x > self.max_x,
            EdgeSide::Top => other.y < self.y && other.max_y >= self.y,
            EdgeSide::Bottom => other.y <= self.max_y && other.max_y > self.max_y,
        }
    }

    const fn overlap_span(other: Self, side: EdgeSide) -> (f64, f64) {
        match side {
            EdgeSide::Left | EdgeSide::Right => (other.y, other.max_y),
            EdgeSide::Top | EdgeSide::Bottom => (other.x, other.max_x),
        }
    }
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

    /// Exclusive upper coordinate along the edge: displays are half-open, so
    /// the point there belongs to the next display or lies off this one.
    #[must_use]
    pub const fn end(self) -> f64 {
        self.end
    }
}

/// Exposed edge segments for one virtual desktop layout.
///
/// A neighboring display removes only the overlapping interval of the side it
/// touches. Displays whose sides lie on one line expose one segment there, so
/// an overlap is never counted twice. Gaps remain exposed. Segments are
/// ordered by side, then by their along-edge start and perpendicular
/// coordinate.
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
        let mut segments = merge_collinear(segments);
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

/// Join the segments that lie on one line of one side and overlap or touch.
///
/// Two displays whose sides share a coordinate (an overlap, or a stack with
/// aligned edges) expose one continuous edge there. Kept apart, the shared
/// stretch would count twice toward the side's length and skew every
/// normalized position on it. Segments on different lines, and gaps on one
/// line, stay separate.
fn merge_collinear(mut segments: Vec<EdgeSegment>) -> Vec<EdgeSegment> {
    segments.sort_by(|left, right| {
        left.side
            .index()
            .cmp(&right.side.index())
            .then_with(|| left.coordinate.total_cmp(&right.coordinate))
            .then_with(|| left.start.total_cmp(&right.start))
    });
    let mut merged: Vec<EdgeSegment> = Vec::with_capacity(segments.len());
    for segment in segments {
        match merged.last_mut() {
            Some(last)
                if last.side == segment.side
                    && last.coordinate.total_cmp(&segment.coordinate).is_eq()
                    && segment.start <= last.end =>
            {
                last.end = last.end.max(segment.end);
            }
            _ => merged.push(segment),
        }
    }
    merged
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
