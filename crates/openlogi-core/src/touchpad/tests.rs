use super::*;

fn contact(id: u8, x_um: u32, y_um: u32) -> TouchContact {
    TouchContact { id, x_um, y_um }
}

fn frame(timestamp_us: u64, contacts: Vec<TouchContact>) -> TouchFrame {
    TouchFrame::new(timestamp_us, false, contacts).expect("test contacts have unique ids")
}

fn translated_frame(
    timestamp_us: u64,
    count: u8,
    horizontal_um: i32,
    vertical_um: i32,
) -> TouchFrame {
    let contacts = (0..count)
        .map(|id| {
            let x = 50_000_i32 + i32::from(id) * 10_000 + horizontal_um;
            let y = 50_000_i32 + vertical_um;
            contact(
                id + 1,
                u32::try_from(x).expect("test x stays positive"),
                u32::try_from(y).expect("test y stays positive"),
            )
        })
        .collect();
    frame(timestamp_us, contacts)
}

#[test]
fn contacts_are_sorted_and_duplicate_ids_are_rejected() {
    let sorted = frame(
        0,
        vec![contact(3, 30, 0), contact(1, 10, 0), contact(2, 20, 0)],
    );

    assert_eq!(
        sorted
            .contacts()
            .iter()
            .map(|point| point.id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        TouchFrame::new(0, false, vec![contact(1, 0, 0), contact(1, 1, 1)]),
        Err(TouchFrameError::DuplicateContactId)
    );
}

#[test]
fn a_short_still_stroke_commits_a_tap_only_when_it_ends() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    assert_eq!(
        recognizer.update(&translated_frame(0, 3, 0, 0)),
        GestureRecognition::Pending
    );
    assert_eq!(
        recognizer.update(&translated_frame(80_000, 3, 500, 200)),
        GestureRecognition::Pending
    );

    assert_eq!(recognizer.end(), Some(ButtonId::TouchpadThreeFingerTap));
    assert_eq!(recognizer.end(), None);
}

#[test]
fn three_finger_swipes_commit_at_most_once_per_stroke() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&translated_frame(0, 3, 0, 0));

    assert_eq!(
        recognizer.update(&translated_frame(60_000, 3, 15_000, 0)),
        GestureRecognition::Gesture(ButtonId::TouchpadThreeFingerSwipeRight)
    );
    assert_eq!(
        recognizer.update(&translated_frame(90_000, 3, 25_000, 0)),
        GestureRecognition::Pending
    );
    assert_eq!(recognizer.end(), None);
}

#[test]
fn sequential_finger_landings_can_become_a_three_finger_swipe() {
    let mut recognizer = TouchpadGestureRecognizer::default();

    assert_eq!(
        recognizer.update(&translated_frame(0, 1, 0, 0)),
        GestureRecognition::Pending
    );
    assert_eq!(
        recognizer.update(&translated_frame(8_000, 2, 0, 0)),
        GestureRecognition::Pending
    );
    assert_eq!(
        recognizer.update(&translated_frame(16_000, 3, 0, 0)),
        GestureRecognition::Pending
    );
    assert_eq!(
        recognizer.update(&translated_frame(76_000, 3, 15_000, 0)),
        GestureRecognition::Gesture(ButtonId::TouchpadThreeFingerSwipeRight)
    );
}

#[test]
fn three_finger_swipe_can_finish_after_one_finger_lifts() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&translated_frame(0, 3, 0, 0));
    recognizer.update(&translated_frame(20_000, 3, 5_000, 0));

    assert_eq!(
        recognizer.update(&frame(
            30_000,
            vec![contact(1, 57_500, 50_000), contact(2, 67_500, 50_000)],
        )),
        GestureRecognition::Pending
    );
    assert_eq!(
        recognizer.update(&frame(
            60_000,
            vec![contact(1, 65_000, 50_000), contact(2, 75_000, 50_000)],
        )),
        GestureRecognition::Gesture(ButtonId::TouchpadThreeFingerSwipeRight)
    );
}

#[test]
fn a_fast_three_frame_flick_bypasses_the_duration_gate() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&translated_frame(0, 3, 0, 0));
    recognizer.update(&translated_frame(8_000, 3, 5_000, 0));
    recognizer.update(&translated_frame(16_000, 3, 10_000, 0));

    assert_eq!(
        recognizer.update(&translated_frame(24_000, 3, 15_000, 0)),
        GestureRecognition::Gesture(ButtonId::TouchpadThreeFingerSwipeRight)
    );
}

#[test]
fn a_fast_flick_still_requires_three_motion_frames() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&translated_frame(0, 3, 0, 0));
    recognizer.update(&translated_frame(8_000, 3, 7_500, 0));

    assert_eq!(
        recognizer.update(&translated_frame(16_000, 3, 15_000, 0)),
        GestureRecognition::Pending
    );
    assert_eq!(recognizer.end(), None);
}

#[test]
fn cardinal_swipes_keep_the_locked_finger_count_and_direction() {
    let recognize = |count, dx, dy| {
        let mut recognizer = TouchpadGestureRecognizer::default();
        recognizer.update(&translated_frame(0, count, 0, 0));
        recognizer.update(&translated_frame(60_000, count, dx, dy))
    };

    assert_eq!(
        recognize(3, -15_000, 0),
        GestureRecognition::Gesture(ButtonId::TouchpadThreeFingerSwipeLeft)
    );
    assert_eq!(
        recognize(3, 0, -15_000),
        GestureRecognition::Gesture(ButtonId::TouchpadThreeFingerSwipeUp)
    );
    assert_eq!(
        recognize(4, 0, 15_000),
        GestureRecognition::Gesture(ButtonId::TouchpadFourFingerSwipeDown)
    );
    assert_eq!(
        recognize(4, 15_000, 0),
        GestureRecognition::Gesture(ButtonId::TouchpadFourFingerSwipeRight)
    );
}

#[test]
fn a_two_finger_zoom_chord_commits_early() {
    // Two-finger pinches stream magnify zoom, whose dead zone must stay
    // native-short: 3 mm of spread (contacts ±3 mm apart from a 6 mm
    // resting spread) with a still centroid commits before the four-finger
    // reveal threshold would.
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&frame(
        0,
        vec![contact(1, 44_000, 50_000), contact(2, 56_000, 50_000)],
    ));
    assert_eq!(
        recognizer.update(&frame(
            20_000,
            vec![contact(1, 41_000, 50_000), contact(2, 59_000, 50_000)],
        )),
        GestureRecognition::Gesture(ButtonId::TouchpadTwoFingerPinchOut)
    );
}

#[test]
fn spread_dominance_commits_pinch_in_and_out() {
    let mut outward = TouchpadGestureRecognizer::default();
    outward.update(&frame(
        0,
        vec![contact(1, 40_000, 50_000), contact(2, 60_000, 50_000)],
    ));
    assert_eq!(
        outward.update(&frame(
            60_000,
            vec![contact(1, 20_000, 50_000), contact(2, 80_000, 50_000)],
        )),
        GestureRecognition::Gesture(ButtonId::TouchpadTwoFingerPinchOut)
    );

    let mut inward = TouchpadGestureRecognizer::default();
    inward.update(&frame(
        0,
        vec![
            contact(1, 20_000, 40_000),
            contact(2, 80_000, 40_000),
            contact(3, 20_000, 60_000),
            contact(4, 80_000, 60_000),
        ],
    ));
    assert_eq!(
        inward.update(&frame(
            60_000,
            vec![
                contact(1, 40_000, 48_000),
                contact(2, 60_000, 48_000),
                contact(3, 40_000, 52_000),
                contact(4, 60_000, 52_000),
            ],
        )),
        GestureRecognition::Gesture(ButtonId::TouchpadFourFingerPinchIn)
    );
}

#[test]
fn drifting_four_finger_pinch_beats_the_swipe_gate() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&frame(
        0,
        vec![
            contact(1, 20_000, 40_000),
            contact(2, 80_000, 40_000),
            contact(3, 20_000, 60_000),
            contact(4, 80_000, 60_000),
        ],
    ));
    // The hand drifts 10 mm down while closing (centroid travel ≈ spread
    // change), the shape real pinches take when the wrist pivots — the swipe
    // gate would otherwise claim it as TouchpadFourFingerSwipeDown.
    assert_eq!(
        recognizer.update(&frame(
            60_000,
            vec![
                contact(1, 34_000, 52_000),
                contact(2, 66_000, 52_000),
                contact(3, 34_000, 68_000),
                contact(4, 66_000, 68_000),
            ],
        )),
        GestureRecognition::Gesture(ButtonId::TouchpadFourFingerPinchIn)
    );
}

#[test]
fn two_finger_comotion_streams_scroll_deltas() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&translated_frame(0, 2, 0, 0));
    assert_eq!(
        recognizer.update(&translated_frame(8_000, 2, 2_000, 1_500)),
        GestureRecognition::Pending
    );

    // Activation happens past the tap travel; the first delta carries only
    // that frame's motion, never the pre-activation travel.
    assert_eq!(
        recognizer.update(&translated_frame(16_000, 2, 5_000, 3_000)),
        GestureRecognition::Scroll {
            dx_um: 3_000,
            dy_um: 1_500
        }
    );
    assert_eq!(
        recognizer.update(&translated_frame(24_000, 2, 7_000, 4_000)),
        GestureRecognition::Scroll {
            dx_um: 2_000,
            dy_um: 1_000
        }
    );
    // A scrolled stroke travelled far past the tap limits.
    assert_eq!(recognizer.end(), None);
}

#[test]
fn two_finger_scroll_waits_for_comotion_to_dominate_spread_change() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&frame(
        0,
        vec![contact(1, 40_000, 50_000), contact(2, 60_000, 50_000)],
    ));

    // The centroid passes the tap travel (3.5 mm), but the fingers also
    // spread 4.5 mm apart: spread change dominates, so the stroke is a zoom
    // chord in the making, not scrolling.
    assert_eq!(
        recognizer.update(&frame(
            20_000,
            vec![contact(1, 39_000, 50_000), contact(2, 68_000, 50_000)],
        )),
        GestureRecognition::Pending
    );
    assert_eq!(recognizer.end(), None);
}

#[test]
fn a_scrolling_stroke_never_reclassifies_as_a_pinch() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&frame(
        0,
        vec![contact(1, 60_000, 50_000), contact(2, 70_000, 50_000)],
    ));
    recognizer.update(&frame(
        8_000,
        vec![contact(1, 62_000, 50_000), contact(2, 72_000, 50_000)],
    ));
    assert_eq!(
        recognizer.update(&frame(
            16_000,
            vec![contact(1, 65_000, 50_000), contact(2, 75_000, 50_000)],
        )),
        GestureRecognition::Scroll {
            dx_um: 3_000,
            dy_um: 0
        }
    );

    // Spreading 20 mm apart with a still centroid would satisfy the pinch
    // gate; the scroll claim is sticky, so the stroke only streams a zero
    // delta and never fires a zoom.
    assert_eq!(
        recognizer.update(&frame(
            24_000,
            vec![contact(1, 55_000, 50_000), contact(2, 85_000, 50_000)],
        )),
        GestureRecognition::Scroll { dx_um: 0, dy_um: 0 }
    );
    assert_eq!(recognizer.end(), None);
}

#[test]
fn two_finger_scroll_stops_when_one_finger_lifts() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&frame(
        0,
        vec![contact(1, 60_000, 50_000), contact(2, 70_000, 50_000)],
    ));
    recognizer.update(&frame(
        8_000,
        vec![contact(1, 62_000, 50_000), contact(2, 72_000, 50_000)],
    ));
    assert_eq!(
        recognizer.update(&frame(
            16_000,
            vec![contact(1, 65_000, 50_000), contact(2, 75_000, 50_000)],
        )),
        GestureRecognition::Scroll {
            dx_um: 3_000,
            dy_um: 0
        }
    );

    assert_eq!(
        recognizer.update(&frame(24_000, vec![contact(1, 68_000, 50_000)],)),
        GestureRecognition::Pending
    );
    assert_eq!(
        recognizer.update(&frame(32_000, vec![contact(1, 74_000, 50_000)],)),
        GestureRecognition::Pending
    );
    assert_eq!(recognizer.end(), None);
}

#[test]
fn replacing_a_finger_before_commit_cancels_the_whole_stroke() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&translated_frame(0, 3, 0, 0));

    assert_eq!(
        recognizer.update(&frame(
            60_000,
            vec![
                contact(1, 65_000, 50_000),
                contact(2, 75_000, 50_000),
                contact(9, 85_000, 50_000),
            ],
        )),
        GestureRecognition::Pending
    );
    assert_eq!(recognizer.end(), None);
}

#[test]
fn five_fingers_cancel_instead_of_degrading_to_four() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&translated_frame(0, 4, 0, 0));

    assert_eq!(
        recognizer.update(&translated_frame(20_000, 5, 0, 0)),
        GestureRecognition::Pending
    );
    assert_eq!(recognizer.end(), None);
}

#[test]
fn cancellation_suppresses_frames_until_the_stroke_ends() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&translated_frame(0, 3, 0, 0));
    recognizer.cancel();

    assert_eq!(
        recognizer.update(&translated_frame(60_000, 3, 15_000, 0)),
        GestureRecognition::Pending
    );
    assert_eq!(recognizer.end(), None);

    recognizer.update(&translated_frame(200_000, 3, 0, 0));
    assert_eq!(
        recognizer.update(&translated_frame(260_000, 3, 15_000, 0)),
        GestureRecognition::Gesture(ButtonId::TouchpadThreeFingerSwipeRight)
    );
}

#[test]
fn a_lifted_and_relanded_finger_resumes_as_a_fresh_stroke() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&frame(
        0,
        vec![contact(1, 60_000, 50_000), contact(2, 70_000, 50_000)],
    ));
    recognizer.update(&frame(
        8_000,
        vec![contact(1, 62_000, 50_000), contact(2, 72_000, 50_000)],
    ));
    assert_eq!(
        recognizer.update(&frame(
            16_000,
            vec![contact(1, 65_000, 50_000), contact(2, 75_000, 50_000)],
        )),
        GestureRecognition::Scroll {
            dx_um: 3_000,
            dy_um: 0
        }
    );

    // One finger lifts mid-scroll, then lands again beside the survivor:
    // the recognizer must rebase and keep streaming instead of freezing
    // until the silence watchdog ends the stroke.
    assert_eq!(
        recognizer.update(&frame(24_000, vec![contact(1, 68_000, 50_000)],)),
        GestureRecognition::Pending
    );
    assert_eq!(
        recognizer.update(&frame(
            32_000,
            vec![contact(1, 68_000, 50_000), contact(3, 78_000, 50_000)],
        )),
        GestureRecognition::Pending
    );
    // 3.5 mm past the re-anchored start re-activates the scroll.
    assert_eq!(
        recognizer.update(&frame(
            40_000,
            vec![contact(1, 71_500, 50_000), contact(3, 81_500, 50_000)],
        )),
        GestureRecognition::Scroll {
            dx_um: 3_500,
            dy_um: 0
        }
    );
}

fn button_frame(timestamp_us: u64, contacts: Vec<TouchContact>) -> TouchFrame {
    TouchFrame::new(timestamp_us, true, contacts).expect("test contacts have unique ids")
}

#[test]
fn releasing_the_button_hands_the_contacts_back_to_gestures() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&button_frame(
        0,
        vec![contact(1, 50_000, 50_000), contact(2, 70_000, 50_000)],
    ));
    assert_eq!(
        recognizer.update(&button_frame(
            8_000,
            vec![contact(1, 54_000, 50_000), contact(2, 74_000, 50_000)],
        )),
        GestureRecognition::Pending
    );

    // The button lifts while both fingers stay down: the same co-motion is
    // a scroll stroke again, starting from its own activation travel.
    assert_eq!(
        recognizer.update(&frame(
            16_000,
            vec![contact(1, 90_000, 50_000), contact(2, 110_000, 50_000)],
        )),
        GestureRecognition::Pending
    );
    assert_eq!(
        recognizer.update(&frame(
            24_000,
            vec![contact(1, 94_000, 50_000), contact(2, 114_000, 50_000)],
        )),
        GestureRecognition::Scroll {
            dx_um: 4_000,
            dy_um: 0
        }
    );
}

#[test]
fn a_button_held_stroke_never_taps_or_gestures() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&button_frame(
        0,
        vec![contact(1, 50_000, 50_000), contact(2, 70_000, 50_000)],
    ));
    recognizer.update(&button_frame(
        80_000,
        vec![contact(1, 50_500, 50_000), contact(2, 70_500, 50_000)],
    ));
    assert_eq!(recognizer.end(), None);
}

#[test]
fn a_touch_shorter_than_thirty_milliseconds_is_not_a_tap() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&frame(
        0,
        vec![contact(1, 50_000, 50_000), contact(2, 70_000, 50_000)],
    ));
    recognizer.update(&frame(
        20_000,
        vec![contact(1, 50_000, 50_000), contact(2, 70_000, 50_000)],
    ));
    assert_eq!(recognizer.end(), None);
}

#[test]
fn two_fingers_spread_apart_do_not_tap() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&frame(
        0,
        vec![contact(1, 30_000, 50_000), contact(2, 70_000, 50_000)],
    ));
    recognizer.update(&frame(
        80_000,
        vec![contact(1, 30_500, 50_000), contact(2, 70_500, 50_000)],
    ));
    // 40 mm apart: a pinch attempt, not a tap.
    assert_eq!(recognizer.end(), None);
}

#[test]
fn a_four_finger_tap_spans_wider_than_two_fingers_may() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&frame(
        0,
        vec![
            contact(1, 20_000, 50_000),
            contact(2, 40_000, 50_000),
            contact(3, 60_000, 50_000),
            contact(4, 80_000, 50_000),
        ],
    ));
    recognizer.update(&frame(
        80_000,
        vec![
            contact(1, 20_500, 50_000),
            contact(2, 40_500, 50_000),
            contact(3, 60_500, 50_000),
            contact(4, 80_500, 50_000),
        ],
    ));
    assert_eq!(recognizer.end(), Some(ButtonId::TouchpadFourFingerTap));
}

#[test]
fn contacts_surviving_a_button_release_cannot_tap() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    // A click-drag: button held, two contacts, then the button lifts while
    // both fingers stay down for a short, still moment before liftoff.
    recognizer.update(&button_frame(
        0,
        vec![contact(1, 50_000, 50_000), contact(2, 70_000, 50_000)],
    ));
    recognizer.update(&button_frame(
        80_000,
        vec![contact(1, 50_500, 50_000), contact(2, 70_500, 50_000)],
    ));
    recognizer.update(&frame(
        100_000,
        vec![contact(1, 50_500, 50_000), contact(2, 70_500, 50_000)],
    ));
    // Lifting within the tap window after a short hold must not click: the
    // stroke never saw these fingers land. (That the same stale contacts can
    // still scroll is covered by
    // `releasing_the_button_hands_the_contacts_back_to_gestures`.)
    assert_eq!(recognizer.end(), None);
}
