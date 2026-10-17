use super::{VendorFunction, input_payload, output_header};

#[test]
fn input_payload_selects_the_function_on_the_vendor_channel() {
    // A grid frame and an image ack as captured on the keypad.
    let grid = [0x13, 0xff, 0x02, 0x00, 0xc8, 0x01, 0x08];
    let ack = [0x13, 0xff, 0x02, 0x2b, 0xc1, 0x00, 0x01];
    assert_eq!(
        input_payload(&grid, VendorFunction::GridKeys),
        Some(&grid[4..])
    );
    assert_eq!(input_payload(&ack, VendorFunction::Image), Some(&ack[4..]));
    // Each is foreign to the other function.
    assert_eq!(input_payload(&grid, VendorFunction::Image), None);
    assert_eq!(input_payload(&ack, VendorFunction::GridKeys), None);
}

#[test]
fn input_payload_rejects_other_reports_on_the_node() {
    // An HID++ long report, the write's own report ID echoed back, another
    // vendor channel, and a header cut short.
    assert_eq!(
        input_payload(&[0x11, 0xff, 0x02, 0x2b, 0x00], VendorFunction::Image),
        None
    );
    assert_eq!(
        input_payload(&[0x14, 0xff, 0x02, 0x2b, 0x00], VendorFunction::Image),
        None
    );
    assert_eq!(
        input_payload(&[0x13, 0xff, 0x03, 0x2b, 0x00], VendorFunction::Image),
        None
    );
    assert_eq!(
        input_payload(&[0x13, 0xff, 0x02], VendorFunction::GridKeys),
        None
    );
    // A bare header is a frame with an empty payload, not a rejection.
    assert_eq!(
        input_payload(&[0x13, 0xff, 0x02, 0x00], VendorFunction::GridKeys),
        Some(&[][..])
    );
}

#[test]
fn output_header_frames_a_write() {
    assert_eq!(
        output_header(VendorFunction::Image),
        [0x14, 0xff, 0x02, 0x2b]
    );
}
