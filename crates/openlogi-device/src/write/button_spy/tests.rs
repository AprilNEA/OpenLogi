use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

const ONBOARD: u8 = 8;
const SPY: u8 = 9;
const SET_MODE: u8 = 1;
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
    /// The `0x8100` mode it is in; `None` without `0x8100`.
    mode: Option<Arc<AtomicU8>>,
    /// The error code it answers a `getMode` with.
    mode_error: Option<u8>,
    /// The error code it answers a switch to host mode with, after it
    /// switched: a reply lost on the way back.
    host_error: Option<u8>,
    /// The error code it answers a spy start with.
    start_error: Option<u8>,
    /// The error code it answers a spy stop with.
    stop_error: Option<u8>,
}

impl Mouse {
    /// A mouse with `0x8100` in `mode`, and the handle that reads it.
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
            let refuse = |error: Option<u8>| error.map(|error| feature_error(request, error));
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
                    if let Some(refused) = refuse(self.mode_error) {
                        return Some(refused);
                    }
                    response[4] = self.mode.as_ref()?.load(Ordering::SeqCst);
                }
                (ONBOARD, SET_MODE) => {
                    self.mode.as_ref()?.store(request[4], Ordering::SeqCst);
                    if request[4] == HOST_MODE
                        && let Some(refused) = refuse(self.host_error)
                    {
                        return Some(refused);
                    }
                }
                (SPY, GET_COUNT) => response[4] = 11,
                (SPY, GET_MAPPING) => response[4..15].copy_from_slice(&MAPPING),
                (SPY, START_SPY) => {
                    if let Some(refused) = refuse(self.start_error) {
                        return Some(refused);
                    }
                }
                (SPY, STOP_SPY) => {
                    if let Some(refused) = refuse(self.stop_error) {
                        return Some(refused);
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

/// The functions called on the feature at `index`, in order.
fn calls_on(index: u8, reports: &[Vec<u8>]) -> Vec<u8> {
    reports
        .iter()
        .filter(|report| report[2] == index)
        .map(|report| report[3] >> 4)
        .collect()
}

async fn next_mask(watch: &mut ButtonSpyWatch) -> Option<u16> {
    tokio::time::timeout(Duration::from_secs(2), watch.next())
        .await
        .expect("the spy report arrives in time")
}

#[tokio::test]
async fn a_watch_holds_host_mode_reports_each_mask_and_gives_onboard_mode_back() {
    let (mouse, mode) = Mouse::in_mode(ONBOARD_MODE);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse.responder());
    let incoming = raw.incoming_sender();
    let channel = scripted_channel(raw).await;

    let mut watch = watch_button_spy_on_channel(&channel, 0xff).await.unwrap();
    assert_eq!(watch.mapping(), MAPPING);
    assert!(watch.holds_host_mode());
    assert_eq!(mode.load(Ordering::SeqCst), HOST_MODE);
    assert_eq!(
        calls_on(SPY, &handle.written_reports()),
        [GET_COUNT, GET_MAPPING, START_SPY]
    );

    incoming.send(spy_report(0x0020)).unwrap();
    incoming.send(spy_report(0x0000)).unwrap();
    assert_eq!(next_mask(&mut watch).await, Some(0x0020));
    assert_eq!(next_mask(&mut watch).await, Some(0x0000));

    watch.stop().await.unwrap();
    assert_eq!(mode.load(Ordering::SeqCst), ONBOARD_MODE);
    assert_eq!(
        calls_on(SPY, &handle.written_reports()),
        [GET_COUNT, GET_MAPPING, START_SPY, STOP_SPY]
    );
}

#[tokio::test]
async fn a_mouse_without_onboard_profiles_is_watched_as_it_is() {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(Mouse::default().responder());
    let channel = scripted_channel(raw).await;

    let watch = watch_button_spy_on_channel(&channel, 0xff).await.unwrap();
    assert!(!watch.holds_host_mode());
    watch.stop().await.unwrap();
    assert_eq!(
        calls_on(SPY, &handle.written_reports()),
        [GET_COUNT, GET_MAPPING, START_SPY, STOP_SPY]
    );
}

#[tokio::test]
async fn a_mouse_in_host_mode_is_refused_and_left_alone() {
    let (mouse, mode) = Mouse::in_mode(HOST_MODE);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse.responder());
    let channel = scripted_channel(raw).await;

    let Err(error) = watch_button_spy_on_channel(&channel, 0xff).await else {
        panic!("host mode belongs to another program");
    };
    assert!(matches!(error, ButtonSpyError::HostMode), "{error:?}");
    assert_eq!(mode.load(Ordering::SeqCst), HOST_MODE);
    let written = handle.written_reports();
    assert_eq!(calls_on(ONBOARD, &written), [GET_MODE]);
    assert_eq!(calls_on(SPY, &written), Vec::<u8>::new());
}

#[tokio::test]
async fn a_mode_that_cannot_be_read_refuses_the_watch() {
    let (mut mouse, _mode) = Mouse::in_mode(ONBOARD_MODE);
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
    assert_eq!(calls_on(SPY, &handle.written_reports()), Vec::<u8>::new());
}

#[tokio::test]
async fn a_refused_start_names_the_spy_and_gives_onboard_mode_back() {
    let (mut mouse, mode) = Mouse::in_mode(ONBOARD_MODE);
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
    assert_eq!(mode.load(Ordering::SeqCst), ONBOARD_MODE);
}

#[tokio::test]
async fn a_failed_switch_to_host_mode_still_gives_onboard_mode_back() {
    let (mut mouse, mode) = Mouse::in_mode(ONBOARD_MODE);
    mouse.host_error = Some(0x05);
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse.responder());
    let channel = scripted_channel(raw).await;

    let Err(error) = watch_button_spy_on_channel(&channel, 0xff).await else {
        panic!("the mouse refused host mode");
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
    assert_eq!(mode.load(Ordering::SeqCst), ONBOARD_MODE);
    assert_eq!(
        calls_on(SPY, &handle.written_reports()),
        [GET_COUNT, GET_MAPPING]
    );
}

#[tokio::test]
async fn a_refused_stop_still_gives_onboard_mode_back() {
    let (mut mouse, mode) = Mouse::in_mode(ONBOARD_MODE);
    mouse.stop_error = Some(0x05);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(mouse.responder());
    let channel = scripted_channel(raw).await;

    let watch = watch_button_spy_on_channel(&channel, 0xff).await.unwrap();
    let error = watch.stop().await.unwrap_err();
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
    assert_eq!(mode.load(Ordering::SeqCst), ONBOARD_MODE);
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
    assert_eq!(calls_on(SPY, &handle.written_reports()), Vec::<u8>::new());
}
