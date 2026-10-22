use openlogi_core::binding::GridKey;
use openlogi_device_registry::keypad::{KeyGrid, find_keypad};

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};
use crate::replay::ReplayChannelHandle;

const KEYPAD: DeviceRoute = DeviceRoute::Direct {
    vendor_id: 0x046d,
    product_id: 0xc354,
};

fn display() -> KeyDisplay {
    key_display_for(&KEYPAD).expect("the MX Creative Keypad's keys are displays")
}

fn jpeg(len: usize) -> KeyImageJpeg {
    // Distinct bytes, so a misplaced chunk cannot pass for a correct one.
    let bytes = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
    KeyImageJpeg::new(bytes).unwrap()
}

/// The ack the hardware sent for a single-packet tile.
fn hardware_ack() -> Vec<u8> {
    let mut ack = vec![0u8; 32];
    ack[..7].copy_from_slice(&[0x13, 0xff, 0x02, 0x2b, 0xc1, 0x00, 0x01]);
    ack
}

/// An ack for a single-packet tile carrying `status`.
fn ack_with_status(status: [u8; 2]) -> Vec<u8> {
    let mut ack = hardware_ack();
    ack[5..7].copy_from_slice(&status);
    ack
}

async fn keypad(acks: bool) -> (SharedChannel, ReplayChannelHandle) {
    answering(acks.then(hardware_ack)).await
}

/// A keypad that answers every image packet with `answer`, or never.
async fn answering(answer: Option<Vec<u8>>) -> (SharedChannel, ReplayChannelHandle) {
    let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        answer
            .clone()
            .filter(|_| request.first() == Some(&IMAGE_HEADER[0]))
    });
    (
        SharedChannel::new(scripted_channel(raw).await, KEYPAD),
        handle,
    )
}

/// Byte-for-byte the first header `mx_keypad_lcd_probe` wrote for key 1 with a
/// 1024-byte JPEG — the write the panel drew correctly.
#[test]
fn a_single_packet_tile_matches_the_hardware_verified_header() {
    let display = display();
    let origin = display.key_origin(GridKey::TopLeft.position()).unwrap();
    let packets = image_packets(origin, display.key_size, &jpeg(1024));

    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].len(), PACKET_LEN);
    assert_eq!(
        packets[0][..FIRST_HEADER_LEN],
        [
            0x14, 0xff, 0x02, 0x2b, 0xe1, 0x01, 0x00, 0x01, 0x00, 0x00, 0x17, 0x00, 0x06, 0x00,
            0x76, 0x00, 0x76, 0x00, 0x04, 0x00,
        ]
    );
    assert_eq!(
        packets[0][FIRST_HEADER_LEN..FIRST_HEADER_LEN + 1024],
        *jpeg(1024).as_bytes()
    );
    assert!(
        packets[0][FIRST_HEADER_LEN + 1024..]
            .iter()
            .all(|b| *b == 0)
    );
}

#[test]
fn a_large_image_continues_in_marked_packets_that_carry_the_rest() {
    let first_room = PACKET_LEN - FIRST_HEADER_LEN;
    let next_room = PACKET_LEN - NEXT_HEADER_LEN;
    let len = first_room + next_room + 10;
    let image = jpeg(len);
    let packets = image_packets(PanelPoint { x: 181, y: 164 }, 118, &image);

    assert_eq!(packets.len(), 3);
    // First: index 1, first marker, not last; declares the whole length.
    assert_eq!(packets[0][4], 0xa1);
    assert_eq!(
        packets[0][18..20],
        u16::try_from(len).unwrap().to_be_bytes()
    );
    // Middle: index 2, neither marker. Last: index 3, last marker.
    assert_eq!(packets[1][..5], [0x14, 0xff, 0x02, 0x2b, 0x22]);
    assert_eq!(packets[2][..5], [0x14, 0xff, 0x02, 0x2b, 0x63]);
    let carried: Vec<u8> = packets[0][FIRST_HEADER_LEN..]
        .iter()
        .chain(&packets[1][NEXT_HEADER_LEN..])
        .chain(&packets[2][NEXT_HEADER_LEN..NEXT_HEADER_LEN + 10])
        .copied()
        .collect();
    assert_eq!(carried, image.as_bytes());
}

#[test]
fn an_image_exactly_filling_the_first_packet_is_one_packet() {
    let packets = image_packets(
        PanelPoint { x: 0, y: 0 },
        118,
        &jpeg(PACKET_LEN - FIRST_HEADER_LEN),
    );
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0][4], 0xe1);
}

#[test]
fn key_image_jpeg_rejects_what_the_length_field_cannot_declare() {
    assert!(KeyImageJpeg::new(Vec::new()).is_none());
    assert!(KeyImageJpeg::new(vec![0; usize::from(u16::MAX) + 1]).is_none());
    assert!(KeyImageJpeg::new(vec![0; usize::from(u16::MAX)]).is_some());
}

#[test]
fn only_a_wired_keypad_with_display_keys_has_a_key_display() {
    assert_eq!(
        key_display_for(&KEYPAD),
        find_keypad(0x046d, 0xc354).map(|keypad| keypad.key_display)
    );
    assert!(
        key_display_for(&DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc355,
        })
        .is_none()
    );
    assert!(
        key_display_for(&DeviceRoute::Bolt {
            receiver_uid: "uid".into(),
            slot: 1,
        })
        .is_none()
    );
}

#[test]
fn only_a_vendor_image_report_for_this_write_counts_as_an_ack() {
    // A single-packet write's part is 0xe1; the ack echoes it as 0xc1.
    let part = 0xe1 & !PART_MARKER;
    assert_eq!(image_ack(&hardware_ack(), part), Some(Ok(())));
    // The last packet of a three-packet write is 0x63, echoed as 0x43.
    assert_eq!(image_ack(&hardware_ack(), 0x63 & !PART_MARKER), None);
    // A grid-key press shares the channel, not the function.
    assert_eq!(
        image_ack(&[0x13, 0xff, 0x02, 0x00, 0xc1, 0x00, 0x01], part),
        None
    );
    // A HID++ long reply with the same leading bytes is another report.
    assert_eq!(
        image_ack(&[0x11, 0xff, 0x02, 0x2b, 0xc1, 0x00, 0x01], part),
        None
    );
    assert_eq!(image_ack(&[0x13, 0xff, 0x02, 0x2b, 0xc1], part), None);
    assert_eq!(image_ack(&[0x13, 0xff], part), None);
}

#[test]
fn an_ack_with_any_other_status_is_a_refusal() {
    assert_eq!(
        image_ack(&ack_with_status([0x00, 0x00]), 0xc1),
        Some(Err([0x00, 0x00]))
    );
}

#[tokio::test]
async fn a_tile_the_device_refuses_fails_instead_of_counting_as_written() {
    let (shared, _handle) = answering(Some(ack_with_status([0x01, 0x00]))).await;

    let error = write_key_image_on(&shared, display(), GridKey::TopLeft, &jpeg(900))
        .await
        .unwrap_err();

    assert!(matches!(error, WriteError::Hidpp(message) if message.contains("01 00")));
}

#[tokio::test]
async fn a_written_tile_lands_at_its_key_and_completes_on_the_ack() {
    let (shared, handle) = keypad(true).await;

    write_key_image_on(&shared, display(), GridKey::BottomMiddle, &jpeg(900))
        .await
        .unwrap();

    let images: Vec<_> = handle
        .written_reports()
        .into_iter()
        .filter(|report| report.first() == Some(&IMAGE_HEADER[0]))
        .collect();
    assert_eq!(images.len(), 1);
    // Bottom-middle is row 2, column 1: (23 + 158, 6 + 2·158).
    assert_eq!(images[0][9..13], [0x00, 0xb5, 0x01, 0x42]);
}

#[tokio::test(start_paused = true)]
async fn a_tile_the_device_never_acknowledges_times_out() {
    let (shared, _handle) = keypad(false).await;

    let error = write_key_image_on(&shared, display(), GridKey::TopLeft, &jpeg(900))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        WriteError::RequestTimedOut {
            operation: HidppOperation::WriteKeyImage
        }
    ));
}

#[tokio::test]
async fn a_key_off_the_panel_writes_nothing() {
    let (shared, handle) = keypad(true).await;
    let one_key = KeyDisplay {
        grid: KeyGrid { rows: 1, cols: 1 },
        ..display()
    };

    let error = write_key_image_on(&shared, one_key, GridKey::Middle, &jpeg(10))
        .await
        .unwrap_err();

    assert!(matches!(error, WriteError::Hidpp(_)));
    assert!(
        !handle
            .written_reports()
            .iter()
            .any(|report| report.first() == Some(&IMAGE_HEADER[0]))
    );
}
