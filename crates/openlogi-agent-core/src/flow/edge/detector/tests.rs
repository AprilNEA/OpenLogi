use super::*;
use crate::flow::edge::DisplayRect;

fn rect(x: f64, y: f64, width: f64, height: f64) -> DisplayRect {
    DisplayRect::new(x, y, width, height).expect("test rectangles are valid")
}

fn point(x: f64, y: f64) -> DesktopPoint {
    DesktopPoint { x, y }
}

fn assert_near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

fn dwell_params() -> EdgeDetectorParams {
    EdgeDetectorParams {
        dwell_time: Duration::from_millis(100),
        arrival_velocity_threshold: 10_000.0,
        cooldown: Duration::from_millis(500),
        ..EdgeDetectorParams::default()
    }
}

#[test]
fn defaults_define_dwell_velocity_hysteresis_and_cooldown() {
    let params = EdgeDetectorParams::default();

    assert_near(params.edge_tolerance, 1.0);
    assert_eq!(params.dwell_time, Duration::from_millis(250));
    assert_near(params.arrival_velocity_threshold, 900.0);
    assert_near(params.rearm_distance, 12.0);
    assert_eq!(params.cooldown, Duration::from_millis(500));
}

#[test]
fn normalization_uses_cumulative_exposed_length_and_skips_gaps() {
    let edges = ExposedEdges::from_displays(&[
        rect(0.0, 0.0, 100.0, 100.0),
        rect(0.0, 200.0, 100.0, 100.0),
    ]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::from_sides([EdgeSide::Left]),
        EdgeDetectorParams {
            arrival_velocity_threshold: 1.0,
            ..EdgeDetectorParams::default()
        },
    );

    assert!(
        detector
            .update(point(10.0, 250.0), Duration::ZERO)
            .is_none()
    );
    let crossing = detector
        .update(point(0.0, 250.0), Duration::from_millis(10))
        .expect("fast arrival should cross");

    assert_near(crossing.t, 0.75);
}

#[test]
fn unarmed_side_never_starts_a_crossing() {
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::from_sides([EdgeSide::Right]),
        EdgeDetectorParams {
            dwell_time: Duration::ZERO,
            ..EdgeDetectorParams::default()
        },
    );

    assert!(detector.update(point(0.0, 50.0), Duration::ZERO).is_none());
}

#[test]
fn fast_arrival_reports_side_position_and_velocity() {
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::from_sides([EdgeSide::Left]),
        EdgeDetectorParams {
            arrival_velocity_threshold: 900.0,
            ..EdgeDetectorParams::default()
        },
    );

    assert!(detector.update(point(10.0, 25.0), Duration::ZERO).is_none());
    let crossing = detector
        .update(point(0.0, 50.0), Duration::from_millis(10))
        .expect("1,000 px/s outward arrival should cross");

    assert_eq!(crossing.side, EdgeSide::Left);
    assert_near(crossing.t, 0.5);
    assert_near(crossing.velocity.x, -1_000.0);
    assert_near(crossing.velocity.y, 2_500.0);
}

#[test]
fn dwell_triggers_once_and_edge_jitter_does_not_retrigger() {
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(edges, ArmedSides::ALL, dwell_params());

    assert!(detector.update(point(0.0, 40.0), Duration::ZERO).is_none());
    assert!(
        detector
            .update(point(0.5, 40.0), Duration::from_millis(99))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 40.0), Duration::from_millis(100))
            .is_some()
    );
    assert!(
        detector
            .update(point(0.5, 40.0), Duration::from_millis(700))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 40.0), Duration::from_millis(1_500))
            .is_none()
    );
}

#[test]
fn hysteresis_and_cooldown_both_gate_a_second_approach() {
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(edges, ArmedSides::ALL, dwell_params());

    assert!(detector.update(point(0.0, 50.0), Duration::ZERO).is_none());
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(100))
            .is_some()
    );

    // Five pixels is inside the 12 px hysteresis band, so this does not re-arm.
    assert!(
        detector
            .update(point(5.0, 50.0), Duration::from_millis(150))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(300))
            .is_none()
    );

    // Leaving the band re-arms, but the 500 ms cooldown still applies.
    assert!(
        detector
            .update(point(20.0, 50.0), Duration::from_millis(350))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(400))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(500))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(600))
            .is_some()
    );
}

#[test]
fn leaving_hysteresis_band_rearms_fast_arrival() {
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::from_sides([EdgeSide::Right]),
        EdgeDetectorParams {
            arrival_velocity_threshold: 500.0,
            cooldown: Duration::from_millis(200),
            ..EdgeDetectorParams::default()
        },
    );

    assert!(detector.update(point(90.0, 50.0), Duration::ZERO).is_none());
    assert!(
        detector
            .update(point(100.0, 50.0), Duration::from_millis(10))
            .is_some()
    );
    assert!(
        detector
            .update(point(80.0, 50.0), Duration::from_millis(20))
            .is_none()
    );
    assert!(
        detector
            .update(point(90.0, 50.0), Duration::from_millis(210))
            .is_none()
    );
    assert!(
        detector
            .update(point(100.0, 50.0), Duration::from_millis(220))
            .is_some()
    );
}

#[test]
fn corner_prefers_greater_outward_velocity() {
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::ALL,
        EdgeDetectorParams {
            arrival_velocity_threshold: 500.0,
            ..EdgeDetectorParams::default()
        },
    );

    assert!(detector.update(point(10.0, 20.0), Duration::ZERO).is_none());
    let crossing = detector
        .update(point(0.0, 0.0), Duration::from_millis(10))
        .expect("corner arrival should cross");

    assert_eq!(crossing.side, EdgeSide::Top);
    assert_near(crossing.t, 0.0);
}

#[test]
fn corner_exact_velocity_tie_uses_left_first_priority() {
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::ALL,
        EdgeDetectorParams {
            arrival_velocity_threshold: 500.0,
            ..EdgeDetectorParams::default()
        },
    );

    assert!(detector.update(point(10.0, 10.0), Duration::ZERO).is_none());
    let crossing = detector
        .update(point(0.0, 0.0), Duration::from_millis(10))
        .expect("corner arrival should cross");

    assert_eq!(crossing.side, EdgeSide::Left);
    assert_near(crossing.t, 0.0);
}

#[test]
fn geometry_replacement_discards_in_progress_dwell() {
    let display = rect(0.0, 0.0, 100.0, 100.0);
    let mut detector = EdgeDetector::new(
        ExposedEdges::from_displays(&[display]),
        ArmedSides::ALL,
        dwell_params(),
    );

    assert!(detector.update(point(0.0, 50.0), Duration::ZERO).is_none());
    detector.set_edges(ExposedEdges::from_displays(&[display]));
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(100))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(200))
            .is_some()
    );
}

#[test]
fn geometry_replacement_preserves_crossing_latch_until_rearm() {
    let display = rect(0.0, 0.0, 100.0, 100.0);
    let mut detector = EdgeDetector::new(
        ExposedEdges::from_displays(&[display]),
        ArmedSides::ALL,
        dwell_params(),
    );

    assert!(detector.update(point(0.0, 50.0), Duration::ZERO).is_none());
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(100))
            .is_some()
    );
    detector.set_edges(ExposedEdges::from_displays(&[display]));
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(700))
            .is_none()
    );

    assert!(
        detector
            .update(point(20.0, 50.0), Duration::from_millis(750))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(800))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(900))
            .is_some()
    );
}

fn params_with(
    edge_tolerance: f64,
    arrival_velocity_threshold: f64,
    rearm_distance: f64,
) -> Result<EdgeDetectorParams, InvalidEdgeDetectorParams> {
    EdgeDetectorParams::new(
        edge_tolerance,
        Duration::from_millis(250),
        arrival_velocity_threshold,
        rearm_distance,
        Duration::from_millis(500),
    )
}

#[test]
fn defaults_pass_their_own_validation() {
    assert_eq!(
        params_with(1.0, 900.0, 12.0),
        Ok(EdgeDetectorParams::default())
    );
}

#[test]
fn params_refuse_nan_infinite_and_negative_thresholds() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        assert!(
            matches!(
                params_with(bad, 900.0, 12.0),
                Err(InvalidEdgeDetectorParams::NotFiniteOrNegative {
                    field: "edge tolerance",
                    ..
                })
            ),
            "edge tolerance {bad} must be refused"
        );
        assert!(
            matches!(
                params_with(1.0, bad, 12.0),
                Err(InvalidEdgeDetectorParams::NotFiniteOrNegative {
                    field: "arrival velocity threshold",
                    ..
                })
            ),
            "arrival velocity threshold {bad} must be refused"
        );
        assert!(
            matches!(
                params_with(1.0, 900.0, bad),
                Err(InvalidEdgeDetectorParams::NotFiniteOrNegative {
                    field: "rearm distance",
                    ..
                })
            ),
            "rearm distance {bad} must be refused"
        );
    }
}

#[test]
fn params_refuse_a_rearm_distance_the_resting_cursor_already_reaches() {
    assert_eq!(
        params_with(1.0, 900.0, 1.0),
        Err(InvalidEdgeDetectorParams::RearmWithinTolerance {
            rearm_distance: 1.0,
            edge_tolerance: 1.0,
        })
    );
    params_with(1.0, 900.0, 0.0).expect_err("a zero rearm distance never leaves the edge");
    // Zero tolerance and zero arrival velocity stay legal.
    params_with(0.0, 0.0, 0.5).expect("zero tolerance and zero velocity are valid");
}

#[test]
fn collinear_overlap_normalizes_over_the_outer_boundary() {
    let edges =
        ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0), rect(0.0, 50.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::from_sides([EdgeSide::Left]),
        EdgeDetectorParams {
            arrival_velocity_threshold: 1.0,
            ..EdgeDetectorParams::default()
        },
    );

    assert!(detector.update(point(10.0, 75.0), Duration::ZERO).is_none());
    let crossing = detector
        .update(point(0.0, 75.0), Duration::from_millis(10))
        .expect("fast arrival should cross");

    // The left boundary is [0, 150]; y = 75 is its midpoint.
    assert_near(crossing.t, 0.5);
}

#[test]
fn a_clock_step_back_neither_counts_as_dwell_nor_panics() {
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::from_sides([EdgeSide::Left]),
        dwell_params(),
    );

    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(500))
            .is_none()
    );
    // Earlier than the contact's start: no elapsed time, so no dwell.
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(100))
            .is_none()
    );
    // The contact keeps its original start and dwells from there.
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(600))
            .is_some()
    );
}

/// A right edge exposed only above a second display that starts at its
/// bottom end: the row where they meet leads into that display, so a fast
/// approach there stays on this computer, while one a row higher crosses.
#[test]
fn the_point_where_an_exposed_edge_meets_a_display_does_not_cross() {
    let edges = ExposedEdges::from_displays(&[
        rect(0.0, 0.0, 100.0, 100.0),
        rect(100.0, 50.0, 100.0, 50.0),
    ]);
    let detector = || {
        EdgeDetector::new(
            edges.clone(),
            ArmedSides::from_sides([EdgeSide::Right]),
            EdgeDetectorParams::default(),
        )
    };

    let mut shared_row = detector();
    assert!(
        shared_row
            .update(point(89.0, 50.0), Duration::ZERO)
            .is_none()
    );
    assert!(
        shared_row
            .update(point(99.0, 50.0), Duration::from_millis(10))
            .is_none(),
        "y = 50 belongs to the second display"
    );

    let mut row_above = detector();
    assert!(
        row_above
            .update(point(89.0, 49.0), Duration::ZERO)
            .is_none()
    );
    let crossing = row_above
        .update(point(99.0, 49.0), Duration::from_millis(10))
        .expect("the exposed part of the edge crosses");
    assert_eq!(crossing.side, EdgeSide::Right);
}

#[test]
fn dwell_deadline_names_when_a_resting_cursor_can_cross() {
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(edges, ArmedSides::ALL, dwell_params());
    assert_eq!(detector.dwell_deadline(), None, "no contact yet");

    assert!(detector.update(point(0.0, 50.0), Duration::ZERO).is_none());
    assert_eq!(detector.dwell_deadline(), Some(Duration::from_millis(100)));
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(100))
            .is_some()
    );
    assert_eq!(detector.dwell_deadline(), None, "latched");

    // Re-armed and back on the edge at 350 ms: the dwell ends at 450 ms, but
    // the cooldown from the crossing at 100 ms runs to 600 ms.
    assert!(
        detector
            .update(point(20.0, 50.0), Duration::from_millis(300))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(350))
            .is_none()
    );
    assert_eq!(detector.dwell_deadline(), Some(Duration::from_millis(600)));
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(600))
            .is_some()
    );
}
