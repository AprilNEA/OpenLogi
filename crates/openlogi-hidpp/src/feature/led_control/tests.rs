use std::assert_matches;
use std::sync::Arc;

use super::{LedControlFeature, LedInfo, LedKind, LedState};
use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
use crate::feature::CreatableFeature;
use crate::nibble::U4;
use crate::protocol::v20::{Hidpp20Error, Message, MessageHeader};

/// The result of `call` when the device answers its one request, to
/// function `function`, with the long payload `reply`, and the reports the
/// feature sent.
fn exchange<T, F>(
    function: u8,
    call: impl FnOnce(LedControlFeature) -> F,
    reply: [u8; 16],
) -> (Result<T, Hidpp20Error>, Vec<Vec<u8>>)
where
    F: Future<Output = Result<T, Hidpp20Error>>,
{
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let mut pending = Box::pin(call(LedControlFeature::new(channel, 3, 8)));
        assert!(futures::poll!(pending.as_mut()).is_pending());
        let header = MessageHeader {
            device_index: 3,
            feature_index: 8,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(1),
        };
        handle
            .send_incoming(Message::Long(header, reply).into())
            .await;
        (pending.await, handle.written_reports())
    })
}

fn reply(bytes: &[u8]) -> [u8; 16] {
    let mut reply = [0; 16];
    reply[..bytes.len()].copy_from_slice(bytes);
    reply
}

#[test]
fn led_info_reads_every_field() {
    let (info, _) = exchange(
        1,
        |leds| async move { leds.get_led_info(1).await },
        reply(&[0x01, 0x02, 0x01, 0x00, 0x03, 0x01]),
    );

    let info = info.unwrap();
    assert_eq!(
        info,
        LedInfo {
            index: 1,
            kind: LedKind::Dpi,
            physical_leds: 1,
            capabilities: 0x0003,
            nv_capabilities: 1,
        }
    );
    assert!(info.supports(LedState::Off));
    assert!(info.supports(LedState::On));
}

#[test]
fn an_unknown_led_type_is_kept() {
    let (info, _) = exchange(
        1,
        |leds| async move { leds.get_led_info(0).await },
        reply(&[0x00, 0x09, 0x01, 0x00, 0x01]),
    );

    let info = info.unwrap();
    assert_eq!(info.kind, LedKind::Other(9));
    assert!(!info.supports(LedState::On));
}

#[test]
fn led_count_is_the_first_byte() {
    let (count, _) = exchange(
        0,
        |leds| async move { leds.get_led_count().await },
        reply(&[2]),
    );

    assert_matches!(count, Ok(2));
}

#[test]
fn sw_control_reads_any_nonzero_byte_as_software() {
    let (software, _) = exchange(
        2,
        |leds| async move { leds.get_sw_control().await },
        reply(&[1]),
    );
    assert_matches!(software, Ok(true));

    let (software, _) = exchange(
        2,
        |leds| async move { leds.get_sw_control().await },
        reply(&[0]),
    );
    assert_matches!(software, Ok(false));
}

#[test]
fn led_state_is_written_most_significant_byte_first() {
    let (written, reports) = exchange(
        5,
        |leds| async move { leds.set_led_state(1, LedState::On).await },
        reply(&[]),
    );

    assert_matches!(written, Ok(()));
    let report = reports.last().expect("one report sent");
    // Long report: id, device index, feature index, function and software
    // id, then the LED index and the state.
    assert_eq!(report[..7], [0x11, 3, 8, 0x51, 1, 0x00, 0x02]);
    assert!(report[7..].iter().all(|&byte| byte == 0));
}

#[test]
fn sw_control_is_requested_with_one_flag_byte() {
    let (written, reports) = exchange(
        3,
        |leds| async move { leds.set_sw_control(true).await },
        reply(&[]),
    );

    assert_matches!(written, Ok(()));
    let report = reports.last().expect("one report sent");
    assert_eq!(report[..5], [0x10, 3, 8, 0x31, 1]);
}
