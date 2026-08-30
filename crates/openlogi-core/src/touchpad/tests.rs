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
fn common_two_finger_motion_is_left_to_native_scrolling() {
    let mut recognizer = TouchpadGestureRecognizer::default();
    recognizer.update(&translated_frame(0, 2, 0, 0));

    assert_eq!(
        recognizer.update(&translated_frame(20_000, 2, 5_000, 0)),
        GestureRecognition::NativeScroll
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
