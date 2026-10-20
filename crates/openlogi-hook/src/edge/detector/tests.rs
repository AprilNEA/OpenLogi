use super::*;
use crate::edge::DisplayRect;

fn rect(x: f64, y: f64, width: f64, height: f64) -> DisplayRect {
    DisplayRect::new(x, y, width, height).expect("test rectangles are valid")
}

fn point(x: f64, y: f64) -> CursorPosition {
    CursorPosition { x, y }
}

fn assert_near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

/// Defaults without corner dead zones, for tests of corner tie-breaking.
fn no_corner_dead_zone() -> EdgeDetectorParams {
    EdgeDetectorParams {
        corner_dead_zone_fraction: 0.0,
        corner_dead_zone_min: 0.0,
        ..EdgeDetectorParams::default()
    }
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
    assert_near(params.corner_dead_zone_fraction, 0.05);
    assert_near(params.corner_dead_zone_min, 24.0);
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
            ..no_corner_dead_zone()
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
            ..no_corner_dead_zone()
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

#[test]
fn a_latched_arrival_waits_until_the_cursor_leaves_the_edge() {
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 100.0)]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::from_sides([EdgeSide::Left]),
        dwell_params(),
    );
    detector.latch();

    // Placed one pixel inside the armed edge and left there: no dwell crossing.
    assert!(detector.update(point(1.0, 50.0), Duration::ZERO).is_none());
    assert!(
        detector
            .update(point(1.0, 50.0), Duration::from_millis(400))
            .is_none()
    );

    // Away past the rearm distance, then back: an ordinary crossing again.
    assert!(
        detector
            .update(point(30.0, 50.0), Duration::from_millis(600))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(1_200))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 50.0), Duration::from_millis(1_400))
            .is_some()
    );
}

#[test]
fn contact_in_a_corner_dead_zone_never_crosses() {
    // A 1000-pixel left edge has 50-pixel dead zones (5 %) at both ends.
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 1600.0, 1000.0)]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::from_sides([EdgeSide::Left]),
        dwell_params(),
    );

    for (y, start_ms, label) in [(10.0, 0, "top corner"), (980.0, 500, "bottom corner")] {
        assert!(
            detector
                .update(point(0.0, y), Duration::from_millis(start_ms))
                .is_none()
        );
        assert!(
            detector
                .update(point(0.0, y), Duration::from_millis(start_ms + 400))
                .is_none(),
            "a dwell in the {label} must not cross"
        );
    }

    assert!(
        detector
            .update(point(0.0, 500.0), Duration::from_secs(1))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 500.0), Duration::from_millis(1_200))
            .is_some(),
        "the middle of the edge still crosses"
    );
}

#[test]
fn a_short_segment_keeps_its_middle_half() {
    // 60 pixels: the 24-pixel minimum would leave 12; a quarter (15) caps it.
    let edges = ExposedEdges::from_displays(&[rect(0.0, 0.0, 100.0, 60.0)]);
    let mut detector = EdgeDetector::new(
        edges,
        ArmedSides::from_sides([EdgeSide::Left]),
        dwell_params(),
    );

    assert!(detector.update(point(0.0, 10.0), Duration::ZERO).is_none());
    assert!(
        detector
            .update(point(0.0, 10.0), Duration::from_millis(200))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 30.0), Duration::from_millis(300))
            .is_none()
    );
    assert!(
        detector
            .update(point(0.0, 30.0), Duration::from_millis(500))
            .is_some()
    );
}
