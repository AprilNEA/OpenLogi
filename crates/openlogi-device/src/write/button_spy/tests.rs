use std::time::Duration;

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

const SPY: u8 = 9;
const GET_COUNT: u8 = 0;
const START_SPY: u8 = 1;
const STOP_SPY: u8 = 2;
const GET_MAPPING: u8 = 3;
/// The G502 LIGHTSPEED's host-mode mapping: the five standard buttons, then
/// six silent slots.
const MAPPING: [u8; 11] = [1, 2, 3, 4, 5, 0, 0, 0, 0, 0, 0];

/// A mouse with `0x8110` at [`SPY`] when `has_spy`, refusing a spy start
/// with `start_error` when set.
fn mouse(has_spy: bool, start_error: Option<u8>) -> impl Fn(&[u8]) -> Option<Vec<u8>> {
    move |request| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => {
                if has_spy && u16::from_be_bytes([request[4], request[5]]) == 0x8110 {
                    response[4] = SPY;
                }
            }
            (SPY, GET_COUNT) => response[4] = 11,
            (SPY, GET_MAPPING) => response[4..15].copy_from_slice(&MAPPING),
            (SPY, START_SPY) => {
                if let Some(error) = start_error {
                    return Some(feature_error(request, error));
                }
            }
            (SPY, STOP_SPY) => {}
            _ => return None,
        }
        Some(response)
    }
}

/// A spy report holding the slots in `mask`.
fn spy_report(mask: u16) -> Vec<u8> {
    let mut report = vec![0; 20];
    report[..4].copy_from_slice(&[0x11, 0xff, SPY, 0x00]);
    report[4..6].copy_from_slice(&mask.to_be_bytes());
    report
}

/// The functions called on `0x8110`, in order.
fn spy_calls(reports: &[Vec<u8>]) -> Vec<u8> {
    reports
        .iter()
        .filter(|report| report[2] == SPY)
        .map(|report| report[3] >> 4)
        .collect()
}

async fn next_mask(watch: &mut ButtonSpyWatch) -> Option<u16> {
    tokio::time::timeout(Duration::from_secs(2), watch.next())
        .await
        .expect("the spy report arrives in time")
}

#[tokio::test]
async fn a_watch_reads_the_mapping_and_reports_each_mask_until_stopped() {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse(true, None));
    let incoming = raw.incoming_sender();
    let channel = scripted_channel(raw).await;

    let mut watch = watch_button_spy_on_channel(&channel, 0xff).await.unwrap();
    assert_eq!(watch.mapping(), MAPPING);
    assert_eq!(
        spy_calls(&handle.written_reports()),
        [GET_COUNT, GET_MAPPING, START_SPY]
    );

    incoming.send(spy_report(0x0020)).unwrap();
    incoming.send(spy_report(0x0000)).unwrap();
    assert_eq!(next_mask(&mut watch).await, Some(0x0020));
    assert_eq!(next_mask(&mut watch).await, Some(0x0000));

    watch.stop().await.unwrap();
    assert_eq!(
        spy_calls(&handle.written_reports()),
        [GET_COUNT, GET_MAPPING, START_SPY, STOP_SPY]
    );
}

#[tokio::test]
async fn a_mouse_without_the_spy_is_unsupported_and_nothing_starts() {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse(false, None));
    let channel = scripted_channel(raw).await;

    let Err(error) = watch_button_spy_on_channel(&channel, 0xff).await else {
        panic!("a mouse without 0x8110 has no spy to watch");
    };
    assert!(
        matches!(
            error,
            WriteError::FeatureUnsupported {
                feature_hex: 0x8110
            }
        ),
        "{error:?}"
    );
    assert_eq!(spy_calls(&handle.written_reports()), Vec::<u8>::new());
}

#[tokio::test]
async fn a_refused_start_names_the_spy() {
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse(true, Some(0x05)));
    let channel = scripted_channel(raw).await;

    let Err(error) = watch_button_spy_on_channel(&channel, 0xff).await else {
        panic!("the mouse refused the spy start");
    };
    assert!(
        matches!(
            error,
            WriteError::HidppFeature {
                feature_hex: 0x8110,
                ..
            }
        ),
        "{error:?}"
    );
}
