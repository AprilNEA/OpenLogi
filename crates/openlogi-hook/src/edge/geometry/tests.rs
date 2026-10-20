use super::*;

fn rect(x: f64, y: f64, width: f64, height: f64) -> DisplayRect {
    DisplayRect::new(x, y, width, height).expect("test rectangles are valid")
}

fn side_segments(edges: &ExposedEdges, side: EdgeSide) -> Vec<(f64, f64, f64)> {
    edges
        .for_side(side)
        .map(|segment| (segment.coordinate, segment.start, segment.end))
        .collect()
}

#[test]
fn display_rect_rejects_invalid_geometry() {
    assert!(DisplayRect::new(0.0, 0.0, 100.0, 100.0).is_some());
    assert!(DisplayRect::new(0.0, 0.0, 0.0, 100.0).is_none());
    assert!(DisplayRect::new(0.0, 0.0, -1.0, 100.0).is_none());
    assert!(DisplayRect::new(f64::NAN, 0.0, 100.0, 100.0).is_none());
}

#[test]
fn single_display_exposes_all_four_sides() {
    let edges = ExposedEdges::from_displays(&[rect(-100.0, -50.0, 200.0, 100.0)]);

    assert_eq!(
        side_segments(&edges, EdgeSide::Left),
        vec![(-100.0, -50.0, 50.0)]
    );
    assert_eq!(
        side_segments(&edges, EdgeSide::Right),
        vec![(100.0, -50.0, 50.0)]
    );
    assert_eq!(
        side_segments(&edges, EdgeSide::Top),
        vec![(-50.0, -100.0, 100.0)]
    );
    assert_eq!(
        side_segments(&edges, EdgeSide::Bottom),
        vec![(50.0, -100.0, 100.0)]
    );
}

#[test]
fn side_by_side_displays_hide_shared_vertical_edges() {
    let edges = ExposedEdges::from_displays(&[
        rect(-100.0, 0.0, 100.0, 100.0),
        rect(0.0, 0.0, 150.0, 100.0),
    ]);

    assert_eq!(
        side_segments(&edges, EdgeSide::Left),
        vec![(-100.0, 0.0, 100.0)]
    );
    assert_eq!(
        side_segments(&edges, EdgeSide::Right),
        vec![(150.0, 0.0, 100.0)]
    );
}

#[test]
fn vertically_stacked_displays_hide_shared_horizontal_edges() {
    let edges =
        ExposedEdges::from_displays(&[rect(0.0, -80.0, 100.0, 80.0), rect(0.0, 0.0, 100.0, 120.0)]);

    assert_eq!(
        side_segments(&edges, EdgeSide::Top),
        vec![(-80.0, 0.0, 100.0)]
    );
    assert_eq!(
        side_segments(&edges, EdgeSide::Bottom),
        vec![(120.0, 0.0, 100.0)]
    );
}

#[test]
fn l_shape_keeps_only_uncovered_part_of_a_touching_edge() {
    let edges = ExposedEdges::from_displays(&[
        rect(0.0, 0.0, 100.0, 100.0),
        rect(100.0, 50.0, 100.0, 50.0),
    ]);

    assert_eq!(
        side_segments(&edges, EdgeSide::Right),
        vec![(100.0, 0.0, 50.0), (200.0, 50.0, 100.0)]
    );
    assert_eq!(
        side_segments(&edges, EdgeSide::Left),
        vec![(0.0, 0.0, 100.0)]
    );
}

#[test]
fn gap_keeps_both_facing_edges_exposed() {
    let edges = ExposedEdges::from_displays(&[
        rect(0.0, 0.0, 100.0, 100.0),
        rect(110.0, 0.0, 100.0, 100.0),
    ]);

    assert_eq!(
        side_segments(&edges, EdgeSide::Right),
        vec![(100.0, 0.0, 100.0), (210.0, 0.0, 100.0)]
    );
    assert_eq!(
        side_segments(&edges, EdgeSide::Left),
        vec![(0.0, 0.0, 100.0), (110.0, 0.0, 100.0)]
    );
}

#[test]
fn overlapping_neighbors_merge_their_covered_spans() {
    let edges = ExposedEdges::from_displays(&[
        rect(0.0, 0.0, 100.0, 200.0),
        rect(100.0, 25.0, 50.0, 75.0),
        rect(100.0, 80.0, 50.0, 70.0),
    ]);

    let primary_segments: Vec<_> = edges
        .for_side(EdgeSide::Right)
        .filter(|segment| segment.coordinate.total_cmp(&100.0).is_eq())
        .map(|segment| (segment.start, segment.end))
        .collect();
    assert_eq!(primary_segments, vec![(0.0, 25.0), (150.0, 200.0)]);
}
