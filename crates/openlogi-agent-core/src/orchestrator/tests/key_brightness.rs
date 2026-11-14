//! Keypad LCD brightness writes are ordered per keypad.

use super::*;
use openlogi_core::config::KeyDisplayBrightness;
use openlogi_core::hid::{HidppOperation, WriteError};
use openlogi_device::test_support::publish_scripted_channel;
use openlogi_fixture::RequestMatch;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The `0x8040` feature index the scripted keypad reports.
const BRIGHTNESS_INDEX: u8 = 5;

/// A keypad whose `0x8040` range is `1..=100`, recording every level written.
async fn keypad(
    shared: &crate::orchestrator::SharedHandles,
) -> (
    DeviceRoute,
    Arc<Mutex<Vec<u16>>>,
    openlogi_device::replay::ReplayChannelHandle,
) {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xc354,
    };
    let levels = Arc::new(Mutex::new(Vec::new()));
    let written = Arc::clone(&levels);
    let (_, reports) = publish_scripted_channel(
        &shared.channel_registry,
        "brightness-keypad",
        route.clone(),
        move |request| {
            if request.len() < 7 {
                return None;
            }
            let payload = match (request[2], request[3] >> 4) {
                // Root: protocol version, then `0x8040` at its index.
                (0, 1) => [4, 0, 0],
                (0, 0) if request[4..6] == [0x80, 0x40] => [BRIGHTNESS_INDEX, 0, 0],
                (0, 0) => [0, 0, 0],
                // getInfo: max 100, 101 steps, min 1.
                (BRIGHTNESS_INDEX, 0) => [0, 100, 101],
                // getBrightness: the last level written, else full.
                (BRIGHTNESS_INDEX, 1) => {
                    let level = written
                        .lock()
                        .expect("levels")
                        .last()
                        .copied()
                        .unwrap_or(100_u16);
                    let [hi, lo] = level.to_be_bytes();
                    [hi, lo, 0]
                }
                (BRIGHTNESS_INDEX, 2) => {
                    written
                        .lock()
                        .expect("levels")
                        .push(u16::from_be_bytes([request[4], request[5]]));
                    [request[4], request[5], 0]
                }
                _ => return None,
            };
            let mut response = vec![0x10, request[1], request[2], request[3]];
            response.extend_from_slice(&payload);
            Some(response)
        },
    )
    .await;
    (route, levels, reports)
}

/// Two saves in quick succession: the first worker is held mid-write while
/// the second is requested. The keypad must end at the newer level, never
/// the older one landing after it.
#[tokio::test]
async fn key_brightness_latest_save_wins() {
    let shared = orchestrator(Config::default()).shared();
    let (route, levels, reports) = keypad(&shared).await;
    let level = |percent| KeyDisplayBrightness::try_new(percent).expect("valid brightness");
    let newer = level(80).scale(0, 100);
    let held = reports.hold_next_write_completion(
        RequestMatch::Hidpp20,
        &[0x10, 0xff, BRIGHTNESS_INDEX, 0x00, 0, 0, 0],
    );

    shared.write_key_brightness_in_background(&route, level(30));
    tokio::time::timeout(Duration::from_secs(2), held.write_accepted())
        .await
        .expect("the first save reaches the keypad");
    shared.write_key_brightness_in_background(&route, level(80));
    // Give an unordered second worker the chance to finish first.
    tokio::time::sleep(Duration::from_millis(200)).await;
    held.release();

    tokio::time::timeout(Duration::from_secs(5), async {
        while levels.lock().expect("levels").last() != Some(&newer) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the newer save is written");
    // Let a late first write land if the order is broken.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        levels.lock().expect("levels").last(),
        Some(&newer),
        "the older save landed after the newer one"
    );
}

/// The app's own write answers with what the keypad reads back afterwards,
/// and a read answers with the level the keypad shows.
#[tokio::test]
async fn a_gui_brightness_write_answers_with_the_keypads_read_back() {
    let shared = orchestrator(Config::default()).shared();
    let (route, levels, _reports) = keypad(&shared).await;
    let level = KeyDisplayBrightness::try_new(40).expect("valid brightness");

    let read = tokio::time::timeout(
        Duration::from_secs(5),
        shared.set_key_brightness(&route, level),
    )
    .await
    .expect("the write answers")
    .expect("the keypad takes the level");

    assert_eq!(read, level);
    assert_eq!(*levels.lock().expect("levels"), [level.scale(0, 100)]);
}

/// An app write that a newer write supersedes before its turn — here the
/// agent's own reload write — writes nothing and answers `WriteSuperseded`,
/// never a reading the app could mistake for its own level landing.
#[tokio::test]
async fn a_superseded_gui_brightness_write_answers_superseded() {
    let shared = orchestrator(Config::default()).shared();
    let (route, levels, reports) = keypad(&shared).await;
    let level = |percent| KeyDisplayBrightness::try_new(percent).expect("valid brightness");
    let held = reports.hold_next_write_completion(
        RequestMatch::Hidpp20,
        &[0x10, 0xff, BRIGHTNESS_INDEX, 0x00, 0, 0, 0],
    );
    shared.write_key_brightness_in_background(&route, level(30));
    tokio::time::timeout(Duration::from_secs(2), held.write_accepted())
        .await
        .expect("the first write reaches the keypad");

    let app_write = {
        let shared = shared.clone();
        let route = route.clone();
        tokio::spawn(async move { shared.set_key_brightness(&route, level(50)).await })
    };
    // Let the app's write take its ticket before the newer one.
    tokio::time::sleep(Duration::from_millis(50)).await;
    shared.write_key_brightness_in_background(&route, level(80));
    held.release();

    let answer = tokio::time::timeout(Duration::from_secs(5), app_write)
        .await
        .expect("the app's write answers")
        .expect("the write task did not panic");
    assert!(
        matches!(
            answer,
            Err(WriteError::WriteSuperseded {
                operation: HidppOperation::WriteKeyBrightness
            })
        ),
        "a superseded write answers WriteSuperseded, got {answer:?}"
    );
    let newest = level(80).scale(0, 100);
    tokio::time::timeout(Duration::from_secs(5), async {
        while levels.lock().expect("levels").last() != Some(&newest) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the newest write lands");
    assert!(
        !levels
            .lock()
            .expect("levels")
            .contains(&level(50).scale(0, 100)),
        "the superseded level was never written"
    );
}

/// A live battery key follows the device's online record: a sleeping
/// receiver slot sorted before the cable it is on now does not stand in for
/// it. With no record online, the first one is the fallback.
#[test]
fn live_battery_prefers_online_route() {
    use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus};
    let battery = |percentage| BatteryInfo {
        percentage: Some(percentage),
        level: BatteryLevel::Good,
        status: BatteryStatus::Discharging,
    };
    let with_battery = |key: &str, slot, online, percentage| {
        let mut device = dev(key, slot, online);
        device.battery = Some(battery(percentage));
        device
    };

    let devices = [
        with_battery("pad", 1, false, 12),
        with_battery("other", 3, true, 50),
        with_battery("pad", 2, true, 80),
    ];
    assert_eq!(
        super::super::live_battery(&devices, "pad"),
        Some(battery(80))
    );
    assert_eq!(
        super::super::live_battery(&[with_battery("pad", 1, false, 12)], "pad"),
        Some(battery(12)),
        "the only record stands in while none is online"
    );
    assert_eq!(super::super::live_battery(&devices, "missing"), None);
}
