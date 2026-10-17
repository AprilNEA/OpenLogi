//! Tests for the `0x8061` / `0x8060` report-rate reads and writes.

use std::assert_matches;
use std::sync::{Arc, Mutex};

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

fn hz(rates: &[u16]) -> Vec<ReportRate> {
    rates.iter().copied().map(ReportRate::new).collect()
}

#[test]
fn legacy_intervals_round_to_the_nearest_hertz() {
    let rates: Vec<u16> = legacy_intervals_ms()
        .map(|ms| legacy_rate(ms).into())
        .collect();
    assert_eq!(rates, [1000, 500, 333, 250, 200, 167, 143, 125]);
    for interval_ms in legacy_intervals_ms() {
        assert_eq!(legacy_interval(legacy_rate(interval_ms)), Some(interval_ms));
    }
    assert_eq!(legacy_interval(ReportRate::new(2000)), None);
    assert_eq!(legacy_interval(ReportRate::new(334)), None);
}

#[test]
fn extended_settings_double_from_125_hz() {
    let rates: Vec<u16> = EXTENDED_RATES
        .map(|setting| extended_rate(setting).into())
        .to_vec();
    assert_eq!(rates, [125, 250, 500, 1000, 2000, 4000, 8000]);
    for setting in EXTENDED_RATES {
        assert_eq!(extended_setting(extended_rate(setting)), Some(setting));
    }
    assert_eq!(extended_setting(ReportRate::new(333)), None);
}

#[test]
fn rate_lists_read_ascending() {
    // 0x8060: bit n is an interval of n + 1 ms, so 8 ms (125 Hz) is bit 7.
    let legacy = ReportRateList::from_bits_retain(0b1000_1011);
    assert_eq!(legacy_rates(legacy), hz(&[125, 250, 500, 1000]));
    // 0x8061: bit n is the nth setting; bit 7 names no rate.
    let extended = ExtendedReportRateList::from_bits_retain(0b1100_1001);
    assert_eq!(extended_rates(extended), hz(&[125, 1000, 8000]));
}

#[test]
fn the_connection_follows_the_hidpp_index() {
    assert_eq!(connection_for(DIRECT_DEVICE_INDEX), ConnectionType::Wired);
    assert_eq!(connection_for(1), ConnectionType::GamingWireless);
    assert_eq!(connection_for(6), ConnectionType::GamingWireless);
}

/// The `0x8061` feature index on the scripted mouse.
const EXTENDED_INDEX: u8 = 0x0f;
/// The `0x8060` feature index on the scripted mouse.
const LEGACY_INDEX: u8 = 0x0e;
/// HID++ 2.0 `Busy`.
const BUSY: u8 = 0x08;

/// Which report-rate feature a scripted mouse lists.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RateFeature {
    /// `0x8061 ExtendedAdjustableReportRate`.
    Extended,
    /// `0x8060 AdjustableReportRate`.
    Legacy,
    /// Neither: the mouse has no report-rate control.
    Neither,
}

/// A gaming mouse with `0x8061`, `0x8060` or neither.
struct Mouse {
    feature: RateFeature,
    /// The rate mask for the current connection (`0x8061` byte 1, `0x8060`
    /// byte 0).
    mask: u8,
    /// `0x8061` setting per connection type: `[wired, wireless]`.
    extended_rates: [u8; 2],
    /// The connection the mouse is on, which a `0x8061` write sets.
    connection: usize,
    /// `0x8060` interval in milliseconds.
    interval_ms: u8,
    /// Whether a write changes the rate.
    takes_writes: bool,
    /// Whether a rate read after a write fails with `Busy`.
    fails_read_back: bool,
    /// Every rate write, as its wire byte.
    writes: Vec<u8>,
    /// The connection type every `0x8061` rate read named.
    rate_reads: Vec<u8>,
}

impl Mouse {
    fn extended() -> Self {
        Self {
            feature: RateFeature::Extended,
            mask: 0b0111_1111,
            extended_rates: [3, 3],
            connection: 0,
            interval_ms: 1,
            takes_writes: true,
            fails_read_back: false,
            writes: Vec::new(),
            rate_reads: Vec::new(),
        }
    }

    fn legacy() -> Self {
        Self {
            feature: RateFeature::Legacy,
            mask: 0b1000_1011,
            interval_ms: 8,
            ..Self::extended()
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
                payload[0] = match u16::from_be_bytes([request[4], request[5]]) {
                    0x8061 if self.feature == RateFeature::Extended => EXTENDED_INDEX,
                    0x8060 if self.feature == RateFeature::Legacy => LEGACY_INDEX,
                    _ => 0,
                };
            }
            (EXTENDED_INDEX, 0x00 | 0x01) => payload[1] = self.mask,
            (EXTENDED_INDEX, 0x02) => {
                self.rate_reads.push(request[4]);
                if self.fails_read_back && !self.writes.is_empty() {
                    return Some(feature_error(request, BUSY));
                }
                payload[0] = *self.extended_rates.get(usize::from(request[4]))?;
            }
            (EXTENDED_INDEX, 0x03) => {
                self.writes.push(request[4]);
                if self.takes_writes {
                    self.extended_rates[self.connection] = request[4];
                }
            }
            (LEGACY_INDEX, 0x00) => payload[0] = self.mask,
            (LEGACY_INDEX, 0x01) => {
                if self.fails_read_back && !self.writes.is_empty() {
                    return Some(feature_error(request, BUSY));
                }
                payload[0] = self.interval_ms;
            }
            (LEGACY_INDEX, 0x02) => {
                self.writes.push(request[4]);
                if self.takes_writes {
                    self.interval_ms = request[4];
                }
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

async fn mouse_channel(mouse: &Arc<Mutex<Mouse>>, route: DeviceRoute) -> SharedChannel {
    let mouse = Arc::clone(mouse);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        mouse.lock().expect("mouse lock").respond(request)
    });
    SharedChannel::new(scripted_channel(raw).await, route)
}

fn direct() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc547,
    }
}

fn behind_receiver() -> DeviceRoute {
    DeviceRoute::Unifying {
        receiver_uid: "LIGHTSPEED".to_string(),
        slot: 1,
    }
}

#[tokio::test]
async fn reads_the_wireless_rate_of_a_mouse_behind_a_receiver() {
    let mouse = Arc::new(Mutex::new(Mouse {
        connection: 1,
        extended_rates: [3, 5],
        ..Mouse::extended()
    }));
    let shared = mouse_channel(&mouse, behind_receiver()).await;

    let info = get_report_rate_info_on(&shared)
        .await
        .expect("the mouse answers");

    assert_eq!(info.current, ReportRate::new(4000));
    assert_eq!(info.supported, hz(&[125, 250, 500, 1000, 2000, 4000, 8000]));
    assert_eq!(mouse.lock().expect("mouse lock").rate_reads, [1]);
}

#[tokio::test]
async fn reads_the_wired_rate_of_a_mouse_on_its_cable() {
    let mouse = Arc::new(Mutex::new(Mouse {
        extended_rates: [6, 3],
        ..Mouse::extended()
    }));
    let shared = mouse_channel(&mouse, direct()).await;

    let info = get_report_rate_info_on(&shared)
        .await
        .expect("the mouse answers");

    assert_eq!(info.current, ReportRate::new(8000));
    assert_eq!(mouse.lock().expect("mouse lock").rate_reads, [0]);
}

#[tokio::test]
async fn writes_a_listed_rate_and_reads_it_back() {
    let mouse = Arc::new(Mutex::new(Mouse {
        connection: 1,
        ..Mouse::extended()
    }));
    let shared = mouse_channel(&mouse, behind_receiver()).await;

    let info = set_report_rate_on(&shared, ReportRate::new(2000))
        .await
        .expect("the mouse takes the write");

    assert_eq!(info.current, ReportRate::new(2000));
    let mouse = mouse.lock().expect("mouse lock");
    assert_eq!(mouse.writes, [4]);
    assert_eq!(mouse.extended_rates, [3, 4]);
    // The list read, then the read-back, both for the wireless link.
    assert_eq!(mouse.rate_reads, [1, 1]);
}

#[tokio::test]
async fn refuses_a_rate_the_device_does_not_list() {
    // 125–1000 Hz only.
    let mouse = Arc::new(Mutex::new(Mouse {
        mask: 0b0000_1111,
        ..Mouse::extended()
    }));
    let shared = mouse_channel(&mouse, direct()).await;

    let error = set_report_rate_on(&shared, ReportRate::new(8000))
        .await
        .expect_err("8 kHz is not listed");

    assert_eq!(
        error,
        WriteError::ReportRateUnsupported {
            requested: ReportRate::new(8000)
        }
    );
    assert!(mouse.lock().expect("mouse lock").writes.is_empty());
}

#[tokio::test]
async fn leaves_a_device_already_at_the_rate_alone() {
    let mouse = Arc::new(Mutex::new(Mouse::extended()));
    let shared = mouse_channel(&mouse, direct()).await;

    let info = set_report_rate_on(&shared, ReportRate::new(1000))
        .await
        .expect("nothing to do");

    assert_eq!(info.current, ReportRate::new(1000));
    assert!(mouse.lock().expect("mouse lock").writes.is_empty());
}

#[tokio::test]
async fn a_rate_the_device_does_not_keep_is_not_applied() {
    let mouse = Arc::new(Mutex::new(Mouse {
        takes_writes: false,
        ..Mouse::extended()
    }));
    let shared = mouse_channel(&mouse, direct()).await;

    let error = set_report_rate_on(&shared, ReportRate::new(500))
        .await
        .expect_err("the mouse keeps 1 kHz");

    assert_eq!(
        error,
        WriteError::ReportRateNotApplied {
            requested: ReportRate::new(500),
            actual: ReportRate::new(1000),
        }
    );
    assert_eq!(mouse.lock().expect("mouse lock").writes, [2]);
}

#[tokio::test]
async fn an_acknowledged_write_whose_read_back_fails_counts_as_written() {
    let mouse = Arc::new(Mutex::new(Mouse {
        fails_read_back: true,
        ..Mouse::extended()
    }));
    let shared = mouse_channel(&mouse, direct()).await;

    let info = set_report_rate_on(&shared, ReportRate::new(250))
        .await
        .expect("the write was acknowledged");

    assert_eq!(info.current, ReportRate::new(250));
    assert_eq!(mouse.lock().expect("mouse lock").writes, [1]);
}

#[tokio::test]
async fn falls_back_to_the_legacy_feature_in_milliseconds() {
    let mouse = Arc::new(Mutex::new(Mouse::legacy()));
    let shared = mouse_channel(&mouse, direct()).await;

    let info = get_report_rate_info_on(&shared)
        .await
        .expect("the mouse answers");
    assert_eq!(info.current, ReportRate::new(125));
    assert_eq!(info.supported, hz(&[125, 250, 500, 1000]));

    let info = set_report_rate_on(&shared, ReportRate::new(500))
        .await
        .expect("the mouse takes the write");
    assert_eq!(info.current, ReportRate::new(500));
    let mouse = mouse.lock().expect("mouse lock");
    assert_eq!(mouse.writes, [2]);
    assert_eq!(mouse.interval_ms, 2);
    assert!(mouse.rate_reads.is_empty(), "0x8061 was never asked");
}

#[tokio::test]
async fn writes_a_legacy_interval_whose_rate_is_rounded() {
    let mouse = Arc::new(Mutex::new(Mouse {
        mask: 0b1111_1111,
        ..Mouse::legacy()
    }));
    let shared = mouse_channel(&mouse, direct()).await;

    let info = set_report_rate_on(&shared, ReportRate::new(333))
        .await
        .expect("3 ms is listed");

    assert_eq!(info.current, ReportRate::new(333));
    assert_eq!(mouse.lock().expect("mouse lock").writes, [3]);
}

#[tokio::test]
async fn a_device_without_either_feature_is_unsupported() {
    let mouse = Arc::new(Mutex::new(Mouse {
        feature: RateFeature::Neither,
        ..Mouse::extended()
    }));
    let shared = mouse_channel(&mouse, direct()).await;

    let error = get_report_rate_info_on(&shared)
        .await
        .expect_err("no report-rate feature");

    assert_matches!(
        error,
        WriteError::FeatureUnsupported {
            feature_hex: 0x8060
        }
    );
}

#[tokio::test]
async fn a_device_that_lists_no_rate_answered_nothing_usable() {
    let mouse = Arc::new(Mutex::new(Mouse {
        mask: 0,
        ..Mouse::extended()
    }));
    let shared = mouse_channel(&mouse, direct()).await;

    let error = get_report_rate_info_on(&shared)
        .await
        .expect_err("an empty list");

    assert_matches!(
        error,
        WriteError::UnsupportedResponse {
            operation: HidppOperation::ReadReportRate,
            feature_hex: 0x8061,
        }
    );
}
