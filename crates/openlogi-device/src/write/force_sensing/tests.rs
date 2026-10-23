//! Tests for the `0x19c0` press-force reads and writes.

use std::assert_matches;
use std::sync::{Arc, Mutex};

use super::*;
use crate::channel::route::DeviceRoute;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

/// The `0x19c0` feature index on the scripted mouse.
const FORCE_INDEX: u8 = 0x0c;
/// HID++ 2.0 `Busy`.
const BUSY: u8 = 0x08;

/// An MX Master 4 Haptic Sense Panel: `getButtonInfo` answers with
/// `info`, and the firmware stores every force it is sent.
struct Panel {
    /// The `getButtonInfo` reply: flags, default, max, min, big-endian.
    info: [u8; 8],
    current: u16,
    /// Whether a read after a write fails with `Busy`.
    fails_read_back: bool,
    /// A force the firmware reports after any write, instead of the one
    /// written.
    keeps: Option<u16>,
    /// Every force written.
    writes: Vec<u16>,
}

impl Panel {
    /// The MX Master 4 this layout was verified on: changeable, default
    /// 5089, max 6768, min 4071, still at its default.
    fn mx_master_4() -> Self {
        Self {
            info: [0x00, 0x01, 0x13, 0xe1, 0x1a, 0x70, 0x0f, 0xe7],
            current: 5089,
            fails_read_back: false,
            keeps: None,
            writes: Vec::new(),
        }
    }

    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
            return None;
        }
        let function = request[3] >> 4;
        let mut payload = [0u8; 16];
        match (request[2], function) {
            (0x00, 0x01) => payload[0] = 4,
            (0x00, 0x00) => {
                if u16::from_be_bytes([request[4], request[5]]) == 0x19c0 {
                    payload[0] = FORCE_INDEX;
                }
            }
            (FORCE_INDEX, 0x01) => {
                assert_eq!(request[4], 0, "button 0");
                payload[..8].copy_from_slice(&self.info);
            }
            (FORCE_INDEX, 0x02) => {
                assert_eq!(request[4], 0, "button 0");
                if self.fails_read_back && !self.writes.is_empty() {
                    return Some(feature_error(request, BUSY));
                }
                payload[..2].copy_from_slice(&self.current.to_be_bytes());
            }
            (FORCE_INDEX, 0x03) => {
                let force = u16::from_be_bytes([request[5], request[6]]);
                self.writes.push(force);
                self.current = self.keeps.unwrap_or(force);
                payload[..3].copy_from_slice(&request[4..7]);
            }
            _ => return None,
        }
        let mut response = vec![0u8; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        response[4..].copy_from_slice(&payload);
        Some(response)
    }
}

async fn panel_channel(panel: &Arc<Mutex<Panel>>) -> SharedChannel {
    let panel = Arc::clone(panel);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        panel.lock().expect("panel lock").respond(request)
    });
    let route = DeviceRoute::Unifying {
        receiver_uid: "BOLT".to_string(),
        slot: 2,
    };
    SharedChannel::new(scripted_channel(raw).await, route)
}

fn writes(panel: &Arc<Mutex<Panel>>) -> Vec<u16> {
    panel.lock().expect("panel lock").writes.clone()
}

#[tokio::test]
async fn reads_the_force_and_the_range_the_panel_reports() {
    let panel = Arc::new(Mutex::new(Panel::mx_master_4()));
    let shared = panel_channel(&panel).await;

    let info = get_force_sensing_on(&shared)
        .await
        .expect("the panel answers");

    assert_eq!(
        info,
        ForceSensingInfo {
            changeable: true,
            default: 5089,
            min: 4071,
            max: 6768,
            current: 5089,
        }
    );
    assert!(writes(&panel).is_empty(), "a read writes nothing");
}

#[tokio::test]
async fn writes_the_force_and_returns_the_read_back() {
    let panel = Arc::new(Mutex::new(Panel::mx_master_4()));
    let shared = panel_channel(&panel).await;

    let info = set_force_sensing_on(&shared, 4500)
        .await
        .expect("the panel takes the write");

    assert_eq!(info.current, 4500);
    assert_eq!((info.min, info.max, info.default), (4071, 6768, 5089));
    assert_eq!(writes(&panel), [4500]);
}

#[tokio::test]
async fn a_force_outside_the_range_is_clamped_before_the_write() {
    for (requested, written) in [(0, 4071), (4070, 4071), (6769, 6768), (u16::MAX, 6768)] {
        let panel = Arc::new(Mutex::new(Panel::mx_master_4()));
        let shared = panel_channel(&panel).await;

        let info = set_force_sensing_on(&shared, requested)
            .await
            .expect("the panel takes the write");

        assert_eq!(info.current, written, "requested {requested}");
        assert_eq!(writes(&panel), [written], "requested {requested}");
    }
}

#[tokio::test]
async fn an_unchangeable_button_is_refused_without_a_write() {
    let panel = Arc::new(Mutex::new(Panel {
        info: [0x00, 0x00, 0x13, 0xe1, 0x1a, 0x70, 0x0f, 0xe7],
        ..Panel::mx_master_4()
    }));
    let shared = panel_channel(&panel).await;

    assert_matches!(
        set_force_sensing_on(&shared, 4500).await,
        Err(WriteError::ForceSensingUnchangeable)
    );
    assert!(writes(&panel).is_empty());
}

#[tokio::test]
async fn the_force_the_panel_already_has_is_not_written_again() {
    let panel = Arc::new(Mutex::new(Panel::mx_master_4()));
    let shared = panel_channel(&panel).await;

    let info = set_force_sensing_on(&shared, 5089)
        .await
        .expect("nothing to write");

    assert_eq!(info.current, 5089);
    assert!(writes(&panel).is_empty());
}

#[tokio::test]
async fn a_failed_read_back_is_an_error() {
    let panel = Arc::new(Mutex::new(Panel {
        fails_read_back: true,
        ..Panel::mx_master_4()
    }));
    let shared = panel_channel(&panel).await;

    let result = set_force_sensing_on(&shared, 6000).await;

    assert!(
        result.is_err(),
        "an unconfirmed force must not be reported as applied: {result:?}"
    );
    assert_eq!(writes(&panel), [6000]);
}

#[tokio::test]
async fn a_read_back_of_another_force_is_not_applied() {
    let panel = Arc::new(Mutex::new(Panel {
        keeps: Some(5089),
        ..Panel::mx_master_4()
    }));
    let shared = panel_channel(&panel).await;

    assert_matches!(
        set_force_sensing_on(&shared, 6000).await,
        Err(WriteError::ForceSensingNotApplied {
            requested: 6000,
            actual: 5089,
        })
    );
}

#[tokio::test]
async fn info_without_a_usable_range_is_an_unsupported_response() {
    for info in [
        // Minimum 6768 above maximum 4071.
        [0x00, 0x01, 0x13, 0xe1, 0x0f, 0xe7, 0x1a, 0x70],
        // Default 3840 below minimum 4071.
        [0x00, 0x01, 0x0f, 0x00, 0x1a, 0x70, 0x0f, 0xe7],
    ] {
        let panel = Arc::new(Mutex::new(Panel {
            info,
            ..Panel::mx_master_4()
        }));
        let shared = panel_channel(&panel).await;

        assert_matches!(
            get_force_sensing_on(&shared).await,
            Err(WriteError::UnsupportedResponse {
                operation: HidppOperation::ReadForceSensing,
                feature_hex: 0x19c0,
            })
        );
        assert_matches!(
            set_force_sensing_on(&shared, 4500).await,
            Err(WriteError::UnsupportedResponse {
                operation: HidppOperation::WriteForceSensing,
                feature_hex: 0x19c0,
            })
        );
        assert!(writes(&panel).is_empty(), "info {info:02x?}");
    }
}
