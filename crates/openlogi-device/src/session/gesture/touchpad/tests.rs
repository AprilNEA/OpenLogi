//! Strict touchpad frame-stream tests.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use hidpp::feature::CreatableFeature;
use openlogi_core::binding::ButtonId;
use openlogi_core::touchpad::{GestureRecognition, TouchpadGestureRecognizer};

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

const TOUCHPAD_INDEX: u8 = 0x04;
static RAW_MODE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static RAW_MODE: AtomicU8 = AtomicU8::new(0);
static RAW_MODE_WRITE_RESULT: AtomicU8 = AtomicU8::new(u8::MAX);

#[derive(Default)]
struct MemoryJournal(Mutex<Option<RawModeJournal>>);

impl TouchpadJournalStore for MemoryJournal {
    fn load(&self, _: &str) -> Result<Option<RawModeJournal>, TouchpadJournalError> {
        Ok(*self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn save(&self, _: &str, journal: RawModeJournal) -> Result<(), TouchpadJournalError> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(journal);
        Ok(())
    }

    fn clear(&self, _: &str) -> Result<(), TouchpadJournalError> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }
}

fn raw_mode_responder(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let mut payload = [0u8; 3];
    match (request[2], request[3] >> 4) {
        (TOUCHPAD_INDEX, 0x01) => payload[0] = RAW_MODE.load(Ordering::Relaxed),
        (TOUCHPAD_INDEX, 0x02) => {
            let forced = RAW_MODE_WRITE_RESULT.load(Ordering::Relaxed);
            RAW_MODE.store(
                if forced == u8::MAX {
                    request[4]
                } else {
                    forced
                },
                Ordering::Relaxed,
            );
        }
        _ => return None,
    }
    let mut response = vec![0u8; 7];
    response[0] = 0x10;
    response[1..4].copy_from_slice(&request[1..4]);
    response[4..].copy_from_slice(&payload);
    Some(response)
}

async fn raw_mode_feature(initial: u8) -> TouchpadRawXyFeature {
    RAW_MODE.store(initial, Ordering::Relaxed);
    RAW_MODE_WRITE_RESULT.store(u8::MAX, Ordering::Relaxed);
    let (raw, _) = ScriptedRawHidChannel::with_responder(raw_mode_responder);
    TouchpadRawXyFeature::new(scripted_channel(raw).await, 1, TOUCHPAD_INDEX)
}

fn geometry(origin: Origin) -> Geometry {
    Geometry {
        x_size: 2_775,
        y_size: 1_786,
        dpi: 90,
        max_finger_count: 4,
        flip_x: matches!(origin, Origin::UpperRight | Origin::LowerRight),
        flip_y: matches!(origin, Origin::LowerLeft | Origin::LowerRight),
    }
}

fn point(id: u8, x: u16, y: u16) -> RawPoint {
    RawPoint {
        contact_type: 0,
        contact_status: 1,
        x,
        y,
        finger_id: id,
    }
}

fn empty() -> RawPoint {
    RawPoint {
        contact_type: 0,
        contact_status: 0,
        x: 0,
        y: 0,
        finger_id: 0,
    }
}

fn chunk(timestamp: u16, points: [RawPoint; 2], finger_count: u8, end_of_frame: bool) -> RawChunk {
    RawChunk {
        timestamp,
        points,
        button: false,
        spurious: false,
        end_of_frame,
        finger_count,
    }
}

fn assembler(origin: Origin) -> FrameAssembler {
    FrameAssembler {
        geometry: geometry(origin),
        pending: None,
        rejected_timestamp: None,
        timestamp: TimestampUnwrapper::new(1),
        dropped_frames: 0,
    }
}

fn stream() -> TouchpadFrameStream {
    TouchpadFrameStream {
        assembler: assembler(Origin::UpperLeft),
        active_contacts: None,
        last_frame_at: None,
        last_timestamp_us: None,
        cadence_us: None,
    }
}

#[tokio::test]
async fn journal_owned_raw_mode_is_restored_on_disarm() {
    let _guard = RAW_MODE_TEST_LOCK.lock().await;
    let feature = raw_mode_feature(0).await;
    let journal = Arc::new(MemoryJournal::default());

    let armed = ArmedRawMode::arm(&feature, journal.as_ref(), "unit:casa")
        .await
        .expect("raw mode should arm");

    assert_eq!(RAW_MODE.load(Ordering::Relaxed), 5);
    assert_eq!(
        journal.load("unit:casa").expect("load journal"),
        Some(RawModeJournal {
            original: 0,
            requested: 5,
            readback: Some(5),
            armed: true,
        })
    );

    armed
        .disarm(&feature, journal.as_ref(), "unit:casa")
        .await
        .expect("owned mode should restore");
    assert_eq!(RAW_MODE.load(Ordering::Relaxed), 0);
    assert_eq!(journal.load("unit:casa").expect("load journal"), None);
}

#[tokio::test]
async fn recovery_writes_only_when_the_current_mode_is_journal_owned() {
    let _guard = RAW_MODE_TEST_LOCK.lock().await;
    let journal = Arc::new(MemoryJournal::default());
    let record = RawModeJournal {
        original: 0,
        requested: 5,
        readback: Some(5),
        armed: true,
    };
    journal.save("unit:casa", record).expect("save journal");
    let feature = raw_mode_feature(5).await;

    ArmedRawMode::recover(&feature, journal.as_ref(), "unit:casa")
        .await
        .expect("owned mode should recover");
    assert_eq!(RAW_MODE.load(Ordering::Relaxed), 0);
    assert_eq!(journal.load("unit:casa").expect("load journal"), None);

    journal.save("unit:casa", record).expect("save journal");
    let feature = raw_mode_feature(9).await;
    ArmedRawMode::recover(&feature, journal.as_ref(), "unit:casa")
        .await
        .expect("external mode should only clear the stale journal");
    assert_eq!(RAW_MODE.load(Ordering::Relaxed), 9);
    assert_eq!(journal.load("unit:casa").expect("load journal"), None);
}

#[tokio::test]
async fn exact_raw_mode_without_a_journal_is_not_claimed_or_restored() {
    let _guard = RAW_MODE_TEST_LOCK.lock().await;
    let feature = raw_mode_feature(5).await;
    let journal = Arc::new(MemoryJournal::default());

    let armed = ArmedRawMode::arm(&feature, journal.as_ref(), "unit:casa")
        .await
        .expect("exact external layout can be observed");
    armed
        .disarm(&feature, journal.as_ref(), "unit:casa")
        .await
        .expect("unowned mode has nothing to restore");

    assert_eq!(RAW_MODE.load(Ordering::Relaxed), 5);
    assert_eq!(journal.load("unit:casa").expect("load journal"), None);
}

#[tokio::test]
async fn an_external_change_during_capture_is_not_overwritten_on_disarm() {
    let _guard = RAW_MODE_TEST_LOCK.lock().await;
    let feature = raw_mode_feature(0).await;
    let journal = Arc::new(MemoryJournal::default());
    let armed = ArmedRawMode::arm(&feature, journal.as_ref(), "unit:casa")
        .await
        .expect("raw mode should arm");
    RAW_MODE.store(9, Ordering::Relaxed);

    armed
        .disarm(&feature, journal.as_ref(), "unit:casa")
        .await
        .expect("external takeover should only clear ownership");

    assert_eq!(RAW_MODE.load(Ordering::Relaxed), 9);
    assert_eq!(journal.load("unit:casa").expect("load journal"), None);
}

#[tokio::test]
async fn mismatched_readback_is_not_treated_as_openlogi_owned() {
    let _guard = RAW_MODE_TEST_LOCK.lock().await;
    let feature = raw_mode_feature(0).await;
    RAW_MODE_WRITE_RESULT.store(9, Ordering::Relaxed);
    let journal = Arc::new(MemoryJournal::default());

    let Err(error) = ArmedRawMode::arm(&feature, journal.as_ref(), "unit:casa").await else {
        panic!("a mismatched raw-mode readback must fail arming");
    };

    assert!(matches!(
        error,
        TouchpadCaptureError::Readback {
            requested: 5,
            actual: 9,
        }
    ));
    assert_eq!(RAW_MODE.load(Ordering::Relaxed), 9);
    assert_eq!(journal.load("unit:casa").expect("load journal"), None);
}

#[tokio::test]
async fn incompatible_external_raw_mode_is_never_overwritten() {
    let _guard = RAW_MODE_TEST_LOCK.lock().await;
    let feature = raw_mode_feature(9).await;
    let journal = Arc::new(MemoryJournal::default());

    let Err(error) = ArmedRawMode::arm(&feature, journal.as_ref(), "unit:casa").await else {
        panic!("an incompatible external layout must block capture");
    };

    assert!(matches!(
        error,
        TouchpadCaptureError::ExternalRawMode { actual: 9 }
    ));
    assert_eq!(RAW_MODE.load(Ordering::Relaxed), 9);
    assert_eq!(journal.load("unit:casa").expect("load journal"), None);
}

#[test]
fn assembles_only_a_complete_eof_terminated_frame() {
    let mut assembler = assembler(Origin::UpperLeft);
    assert!(
        assembler
            .push(chunk(
                10,
                [point(1, 100, 200), point(2, 300, 400)],
                3,
                false
            ))
            .is_none()
    );
    let Some(FrameOutcome::Frame(frame)) =
        assembler.push(chunk(10, [point(3, 500, 600), empty()], 3, true))
    else {
        panic!("complete frame should commit at EOF");
    };

    assert_eq!(frame.timestamp_us, 1_000);
    assert_eq!(frame.contacts().len(), 3);
    assert_eq!(frame.contacts()[0].x_um, 28_222);
}

#[test]
fn drops_incomplete_old_timestamp_and_preserves_identical_next_frame() {
    let mut assembler = assembler(Origin::UpperLeft);
    assert!(
        assembler
            .push(chunk(20, [point(1, 100, 200), empty()], 2, false))
            .is_none()
    );
    let Some(FrameOutcome::Frame(first)) =
        assembler.push(chunk(21, [point(1, 100, 200), point(2, 300, 400)], 2, true))
    else {
        panic!("new timestamp should replace the incomplete frame");
    };
    assert_eq!(assembler.take_dropped_frames(), 1);
    let Some(FrameOutcome::Frame(second)) =
        assembler.push(chunk(22, [point(1, 100, 200), point(2, 300, 400)], 2, true))
    else {
        panic!("same coordinates at a new timestamp remain a real frame");
    };

    assert_eq!(first.contacts(), second.contacts());
    assert!(second.timestamp_us > first.timestamp_us);
}

#[test]
fn oversized_contact_frame_counts_the_incomplete_frame_it_replaces() {
    let mut assembler = assembler(Origin::UpperLeft);
    assert!(
        assembler
            .push(chunk(20, [point(1, 100, 200), empty()], 2, false))
            .is_none()
    );

    let outcome = assembler.push(chunk(21, [point(1, 100, 200), point(2, 300, 400)], 5, true));

    assert!(matches!(outcome, Some(FrameOutcome::Cancel { .. })));
    assert_eq!(assembler.take_dropped_frames(), 1);
}

#[test]
fn rejects_mismatched_metadata_duplicates_and_spurious_reports() {
    let mut assembler = assembler(Origin::UpperLeft);
    assert!(
        assembler
            .push(chunk(30, [point(1, 1, 1), point(2, 2, 2)], 3, false))
            .is_none()
    );
    assert!(
        assembler
            .push(chunk(30, [point(3, 3, 3), empty()], 2, true))
            .is_none()
    );

    assert!(
        assembler
            .push(chunk(31, [point(1, 1, 1), point(1, 2, 2)], 2, true))
            .is_none()
    );

    let mut spurious = chunk(32, [point(1, 1, 1), point(2, 2, 2)], 2, true);
    spurious.spurious = true;
    assert!(assembler.push(spurious).is_none());
}

#[test]
fn normalizes_every_origin_to_left_and_down_positive() {
    let source = point(1, 100, 200);
    let expected = [
        (Origin::UpperLeft, (100, 200)),
        (Origin::UpperRight, (2_675, 200)),
        (Origin::LowerLeft, (100, 1_586)),
        (Origin::LowerRight, (2_675, 1_586)),
    ];

    for (origin, (x, y)) in expected {
        let contact = geometry(origin)
            .normalize(source)
            .expect("valid point")
            .expect("active point");
        assert_eq!(contact.x_um, coordinate_um(x, 90));
        assert_eq!(contact.y_um, coordinate_um(y, 90));
    }
}

#[test]
fn unwraps_timestamp_rollover() {
    let mut timestamps = TimestampUnwrapper::new(1);
    let before = timestamps.unwrap(u16::MAX - 2);
    let after = timestamps.unwrap(3);

    assert_eq!(after - before, 600);
}

#[test]
fn five_fingers_cancel_immediately() {
    let mut assembler = assembler(Origin::UpperLeft);
    assert!(matches!(
        assembler.push(chunk(40, [point(1, 1, 1), point(2, 2, 2)], 5, false)),
        Some(FrameOutcome::Cancel {
            finger_count: 5,
            ..
        })
    ));
    assert!(
        assembler
            .push(chunk(40, [point(3, 3, 3), point(4, 4, 4)], 5, true))
            .is_none(),
        "later chunks from the same five-finger frame must not cancel twice"
    );
}

#[test]
fn five_fingers_suppress_a_smaller_gesture_until_liftoff() {
    let mut stream = stream();
    let mut recognizer = TouchpadGestureRecognizer::default();
    let now = Instant::now();

    let cancel = stream.push_chunk(
        chunk(40, [point(1, 100, 100), point(2, 200, 100)], 5, false),
        now,
    );
    assert_eq!(cancel, vec![TouchpadStreamEvent::Cancel]);
    recognizer.cancel();
    assert!(
        stream
            .push_chunk(
                chunk(40, [point(3, 300, 100), point(4, 400, 100)], 5, true),
                now,
            )
            .is_empty()
    );

    assert!(
        stream
            .push_chunk(
                chunk(120, [point(1, 200, 100), point(2, 300, 100)], 4, false,),
                now + Duration::from_millis(8),
            )
            .is_empty()
    );
    let smaller = stream.push_chunk(
        chunk(120, [point(3, 400, 100), point(4, 500, 100)], 4, true),
        now + Duration::from_millis(8),
    );
    let [TouchpadStreamEvent::Frame(frame)] = smaller.as_slice() else {
        panic!("the smaller frame should remain in the cancelled stroke");
    };
    assert_eq!(recognizer.update(frame), GestureRecognition::Pending);

    let timeout = stream.stroke_end_timeout();
    assert_eq!(
        stream.poll_end(now + Duration::from_millis(8) + timeout),
        vec![TouchpadStreamEvent::End]
    );
    assert_eq!(recognizer.end(), None);
}

#[test]
fn a_dropped_active_frame_keeps_the_stroke_tappable() {
    let mut stream = stream();
    let mut recognizer = TouchpadGestureRecognizer::default();
    let now = Instant::now();

    assert!(
        stream
            .push_chunk(
                chunk(100, [point(1, 100, 100), point(2, 200, 100)], 3, false,),
                now,
            )
            .is_empty()
    );
    let initial = stream.push_chunk(chunk(100, [point(3, 300, 100), empty()], 3, true), now);
    let [TouchpadStreamEvent::Frame(frame)] = initial.as_slice() else {
        panic!("the initial frame should assemble");
    };
    assert_eq!(recognizer.update(frame), GestureRecognition::Pending);

    assert!(
        stream
            .push_chunk(
                chunk(200, [point(1, 100, 100), point(2, 200, 100)], 3, false,),
                now + Duration::from_millis(10),
            )
            .is_empty()
    );
    let dropped = stream.push_chunk(
        chunk(300, [point(1, 100, 100), point(2, 200, 100)], 3, false),
        now + Duration::from_millis(20),
    );
    // The liftoff drop reports the frame loss without cancelling: the stroke
    // must stay tappable for the watchdog End.
    assert_eq!(dropped, vec![TouchpadStreamEvent::DroppedFrames(1)]);

    let later = stream.push_chunk(
        chunk(300, [point(3, 300, 100), empty()], 3, true),
        now + Duration::from_millis(20),
    );
    let [TouchpadStreamEvent::Frame(frame)] = later.as_slice() else {
        panic!("the complete later frame should still be observable");
    };
    assert_eq!(recognizer.update(frame), GestureRecognition::Pending);
    assert_eq!(
        recognizer.end(),
        Some(ButtonId::TouchpadThreeFingerTap),
        "a dropped liftoff frame must not suppress the tap"
    );
}

#[test]
fn silence_ends_the_previous_stroke() {
    let mut stream = TouchpadFrameStream {
        assembler: assembler(Origin::UpperLeft),
        active_contacts: None,
        last_frame_at: None,
        last_timestamp_us: None,
        cadence_us: None,
    };
    let now = Instant::now();
    let two = stream
        .assembler
        .push(chunk(50, [point(1, 1, 1), point(2, 2, 2)], 2, true));
    assert!(matches!(two, Some(FrameOutcome::Frame(_))));

    // Drive the public stream bookkeeping with the same state shape; the raw
    // HID++ constructor is intentionally not public, so unit tests exercise
    // strict assembly above and lifecycle transitions directly here.
    stream.active_contacts = Some(2);
    stream.last_frame_at = Some(now);
    let timeout = stream.stroke_end_timeout();
    assert_eq!(
        stream.poll_end(now + timeout),
        vec![TouchpadStreamEvent::End]
    );
    assert!(stream.poll_end(now + timeout).is_empty());
}

#[test]
fn silence_drops_the_incomplete_final_frame_before_ending() {
    let mut stream = stream();
    let now = Instant::now();
    let initial = TouchFrame::new(
        1_000,
        false,
        vec![
            TouchContact {
                id: 1,
                x_um: 10_000,
                y_um: 10_000,
            },
            TouchContact {
                id: 2,
                x_um: 20_000,
                y_um: 10_000,
            },
        ],
    )
    .expect("valid frame");
    let mut events = Vec::new();
    stream.publish_frame(initial, now, &mut events);
    assert!(
        stream
            .push_chunk(
                chunk(20, [point(1, 100, 100), empty()], 2, false),
                now + Duration::from_millis(1),
            )
            .is_empty()
    );
    let timeout = stream.stroke_end_timeout();

    assert_eq!(
        stream.poll_end(now + Duration::from_millis(1) + timeout),
        vec![
            TouchpadStreamEvent::DroppedFrames(1),
            TouchpadStreamEvent::End,
        ]
    );
}

#[test]
fn stroke_end_timeout_tracks_report_cadence_with_bounds() {
    let mut stream = TouchpadFrameStream {
        assembler: assembler(Origin::UpperLeft),
        active_contacts: Some(2),
        last_frame_at: Some(Instant::now()),
        last_timestamp_us: Some(8_000),
        cadence_us: None,
    };

    assert_eq!(
        stream.stroke_end_timeout(),
        Duration::from_micros(FALLBACK_STROKE_END_TIMEOUT_US)
    );

    stream.cadence_us = Some(8_000);
    assert_eq!(stream.stroke_end_timeout(), Duration::from_millis(32));

    stream.cadence_us = Some(1_000);
    assert_eq!(
        stream.stroke_end_timeout(),
        Duration::from_micros(MIN_STROKE_END_TIMEOUT_US)
    );

    stream.cadence_us = Some(30_000);
    assert_eq!(
        stream.stroke_end_timeout(),
        Duration::from_micros(MAX_STROKE_END_TIMEOUT_US)
    );
}

#[test]
fn finger_count_change_stays_in_one_stroke_for_recognizer_cancellation() {
    let mut stream = TouchpadFrameStream {
        assembler: assembler(Origin::UpperLeft),
        active_contacts: None,
        last_frame_at: None,
        last_timestamp_us: None,
        cadence_us: None,
    };
    let now = Instant::now();
    let two = TouchFrame::new(
        1_000,
        false,
        vec![
            TouchContact {
                id: 1,
                x_um: 10_000,
                y_um: 10_000,
            },
            TouchContact {
                id: 2,
                x_um: 20_000,
                y_um: 10_000,
            },
        ],
    )
    .expect("valid frame");
    let three = TouchFrame::new(
        9_000,
        false,
        vec![
            TouchContact {
                id: 1,
                x_um: 10_000,
                y_um: 10_000,
            },
            TouchContact {
                id: 2,
                x_um: 20_000,
                y_um: 10_000,
            },
            TouchContact {
                id: 3,
                x_um: 30_000,
                y_um: 10_000,
            },
        ],
    )
    .expect("valid frame");
    let mut events = Vec::new();

    stream.publish_frame(two, now, &mut events);
    events.clear();
    stream.publish_frame(three.clone(), now + Duration::from_millis(8), &mut events);

    assert_eq!(events, vec![TouchpadStreamEvent::Frame(three)]);
}

#[test]
fn abnormal_device_timestamp_gap_ends_before_the_next_frame() {
    let mut stream = TouchpadFrameStream {
        assembler: assembler(Origin::UpperLeft),
        active_contacts: None,
        last_frame_at: None,
        last_timestamp_us: None,
        cadence_us: None,
    };
    let now = Instant::now();
    let frame = |timestamp_us| {
        TouchFrame::new(
            timestamp_us,
            false,
            vec![
                TouchContact {
                    id: 1,
                    x_um: 10_000,
                    y_um: 10_000,
                },
                TouchContact {
                    id: 2,
                    x_um: 20_000,
                    y_um: 10_000,
                },
            ],
        )
        .expect("valid frame")
    };
    let mut events = Vec::new();
    stream.publish_frame(frame(1_000), now, &mut events);
    events.clear();
    stream.publish_frame(frame(9_000), now + Duration::from_millis(8), &mut events);
    events.clear();
    stream.publish_frame(frame(17_000), now + Duration::from_millis(16), &mut events);
    events.clear();
    let next = frame(57_000);

    stream.publish_frame(next.clone(), now + Duration::from_millis(56), &mut events);

    assert_eq!(
        events,
        vec![TouchpadStreamEvent::End, TouchpadStreamEvent::Frame(next)]
    );
}

#[test]
fn a_version_zero_touchpad_builds_a_stream_like_a_version_one_pad() {
    use hidpp::feature::touchpad_raw_xy::{Origin, TouchpadInfo};

    fn info(version: u8) -> TouchpadInfo {
        let mut payload = [0_u8; 16];
        payload[0..2].copy_from_slice(&2775_u16.to_be_bytes());
        payload[2..4].copy_from_slice(&1786_u16.to_be_bytes());
        payload[6] = 13;
        payload[7] = 5;
        payload[8] = Origin::UpperLeft as u8;
        payload[12] = version;
        payload[13..15].copy_from_slice(&600_u16.to_be_bytes());
        TouchpadInfo::from_payload(&payload).expect("valid fixture payload")
    }

    // The Casa Touch reports mapping version 0 with the observed DualXY
    // layout of version 1 — rejecting it disarms capture on the very device
    // this feature exists for.
    TouchpadFrameStream::new(info(0)).expect("version 0 builds a stream");
    TouchpadFrameStream::new(info(1)).expect("version 1 builds a stream");
    assert_eq!(
        TouchpadFrameStream::new(info(2)).err(),
        Some(TouchpadStreamError::UnsupportedMapping(2))
    );
}
