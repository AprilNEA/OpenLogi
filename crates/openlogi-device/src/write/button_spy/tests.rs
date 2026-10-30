use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

const ONBOARD: u8 = 8;
const SPY: u8 = 9;
const GET_MODE: u8 = 2;
const GET_COUNT: u8 = 0;
const START_SPY: u8 = 1;
const STOP_SPY: u8 = 2;
const GET_MAPPING: u8 = 3;
const ONBOARD_MODE: u8 = 1;
const HOST_MODE: u8 = 2;
/// The G502 LIGHTSPEED's host-mode mapping: the five standard buttons, then
/// six silent slots.
const MAPPING: [u8; 11] = [1, 2, 3, 4, 5, 0, 0, 0, 0, 0, 0];

/// A scripted G-series mouse with `0x8110` at [`SPY`].
#[derive(Default)]
struct Mouse {
    /// The `0x8100` mode it reports; `None` without `0x8100`.
    mode: Option<Arc<AtomicU8>>,
    /// The error code it answers a `getMode` with.
    mode_error: Option<u8>,
    /// The error code it answers a spy start with.
    start_error: Option<u8>,
    /// The mode another program switches it to just before a spy stop lands.
    mode_at_stop: Option<u8>,
}

impl Mouse {
    /// A mouse with `0x8100` in `mode`, and the handle that switches it.
    fn in_mode(mode: u8) -> (Self, Arc<AtomicU8>) {
        let mode = Arc::new(AtomicU8::new(mode));
        let mouse = Self {
            mode: Some(Arc::clone(&mode)),
            ..Self::default()
        };
        (mouse, mode)
    }

    fn responder(self) -> impl Fn(&[u8]) -> Option<Vec<u8>> {
        move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => {
                    response[4] = match u16::from_be_bytes([request[4], request[5]]) {
                        0x8100 if self.mode.is_some() => ONBOARD,
                        0x8110 => SPY,
                        _ => 0,
                    };
                }
                (ONBOARD, GET_MODE) => {
                    if let Some(error) = self.mode_error {
                        return Some(feature_error(request, error));
                    }
                    response[4] = self.mode.as_ref()?.load(Ordering::SeqCst);
                }
                (SPY, GET_COUNT) => response[4] = 11,
                (SPY, GET_MAPPING) => response[4..15].copy_from_slice(&MAPPING),
                (SPY, START_SPY) => {
                    if let Some(error) = self.start_error {
                        return Some(feature_error(request, error));
                    }
                }
                (SPY, STOP_SPY) => {
                    if let (Some(mode), Some(at_stop)) = (&self.mode, self.mode_at_stop) {
                        mode.store(at_stop, Ordering::SeqCst);
                    }
                }
                _ => return None,
            }
            Some(response)
        }
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
    let (mouse, _mode) = Mouse::in_mode(ONBOARD_MODE);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse.responder());
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

    assert_eq!(watch.stop().await.unwrap(), ButtonSpyStop::Stopped);
    assert_eq!(
        spy_calls(&handle.written_reports()),
        [GET_COUNT, GET_MAPPING, START_SPY, STOP_SPY]
    );
}

#[tokio::test]
async fn a_mouse_without_onboard_profiles_has_no_host_mode_to_respect() {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(Mouse::default().responder());
    let channel = scripted_channel(raw).await;

    let watch = watch_button_spy_on_channel(&channel, 0xff).await.unwrap();
    assert_eq!(watch.stop().await.unwrap(), ButtonSpyStop::Stopped);
    assert_eq!(
        spy_calls(&handle.written_reports()),
        [GET_COUNT, GET_MAPPING, START_SPY, STOP_SPY]
    );
}

#[tokio::test]
async fn a_mouse_in_host_mode_is_refused_and_its_spy_left_alone() {
    let (mouse, _mode) = Mouse::in_mode(HOST_MODE);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse.responder());
    let channel = scripted_channel(raw).await;

    let Err(error) = watch_button_spy_on_channel(&channel, 0xff).await else {
        panic!("host mode belongs to another program");
    };
    assert!(matches!(error, ButtonSpyError::HostMode), "{error:?}");
    assert_eq!(spy_calls(&handle.written_reports()), Vec::<u8>::new());
}

#[tokio::test]
async fn a_mode_that_cannot_be_read_refuses_the_watch() {
    let (mut mouse, _mode) = Mouse::in_mode(HOST_MODE);
    mouse.mode_error = Some(0x05);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse.responder());
    let channel = scripted_channel(raw).await;

    let Err(error) = watch_button_spy_on_channel(&channel, 0xff).await else {
        panic!("an unread mode may be host mode");
    };
    assert!(
        matches!(
            error,
            ButtonSpyError::Write(WriteError::HidppFeature {
                feature_hex: 0x8100,
                ..
            })
        ),
        "{error:?}"
    );
    assert_eq!(spy_calls(&handle.written_reports()), Vec::<u8>::new());
}

#[tokio::test]
async fn a_mouse_switched_to_host_mode_during_the_watch_keeps_its_spy() {
    let (mouse, mode) = Mouse::in_mode(ONBOARD_MODE);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse.responder());
    let channel = scripted_channel(raw).await;

    let watch = watch_button_spy_on_channel(&channel, 0xff).await.unwrap();
    mode.store(HOST_MODE, Ordering::SeqCst);

    assert_eq!(watch.stop().await.unwrap(), ButtonSpyStop::HostOwned);
    assert_eq!(
        spy_calls(&handle.written_reports()),
        [GET_COUNT, GET_MAPPING, START_SPY]
    );
}

#[tokio::test]
async fn a_spy_taken_over_right_before_the_stop_is_started_again() {
    let (mut mouse, _mode) = Mouse::in_mode(ONBOARD_MODE);
    mouse.mode_at_stop = Some(HOST_MODE);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse.responder());
    let channel = scripted_channel(raw).await;

    let watch = watch_button_spy_on_channel(&channel, 0xff).await.unwrap();

    assert_eq!(watch.stop().await.unwrap(), ButtonSpyStop::HostOwned);
    assert_eq!(
        spy_calls(&handle.written_reports()),
        [GET_COUNT, GET_MAPPING, START_SPY, STOP_SPY, START_SPY]
    );
}

#[tokio::test]
async fn a_mouse_without_the_spy_is_unsupported_and_nothing_starts() {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(|request: &[u8]| {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(&request[..4]);
        response[0] = 0x11;
        // Answer the protocol ping; every feature lookup finds nothing.
        if request[2] == 0 && request[3] >> 4 == 1 {
            response[4] = 4;
        }
        Some(response)
    });
    let channel = scripted_channel(raw).await;

    let Err(error) = watch_button_spy_on_channel(&channel, 0xff).await else {
        panic!("a mouse without 0x8110 has no spy to watch");
    };
    assert!(
        matches!(
            error,
            ButtonSpyError::Write(WriteError::FeatureUnsupported {
                feature_hex: 0x8110
            })
        ),
        "{error:?}"
    );
    assert_eq!(spy_calls(&handle.written_reports()), Vec::<u8>::new());
}

#[tokio::test]
async fn a_refused_start_names_the_spy() {
    let (mut mouse, _mode) = Mouse::in_mode(ONBOARD_MODE);
    mouse.start_error = Some(0x05);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse.responder());
    let channel = scripted_channel(raw).await;

    let Err(error) = watch_button_spy_on_channel(&channel, 0xff).await else {
        panic!("the mouse refused the spy start");
    };
    assert!(
        matches!(
            error,
            ButtonSpyError::Write(WriteError::HidppFeature {
                feature_hex: 0x8110,
                ..
            })
        ),
        "{error:?}"
    );
}
