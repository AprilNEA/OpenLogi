use super::*;

#[test]
fn matches_usb_ble_and_keyboard_hidpp_collections() {
    assert!(is_hidpp_long_collection(0xff00, 0x0002)); // USB / receiver / BT-classic
    assert!(is_hidpp_long_collection(0xff43, 0x0202)); // BLE-direct (Lift, Signature)
    assert!(is_hidpp_long_collection(0xff43, 0x0602)); // wired G-series keyboard (G513)
    assert!(!is_hidpp_long_collection(0x0001, 0x0002)); // generic-desktop mouse
    assert!(!is_hidpp_long_collection(0xff43, 0x0002)); // page right, usage wrong
}

#[test]
fn only_ble_collection_is_long_only() {
    assert!(is_long_only_collection(0xff43, 0x0202)); // BLE-direct → short-unsupported
    assert!(!is_long_only_collection(0xff00, 0x0002)); // USB / receiver carries both reports
    assert!(!is_long_only_collection(0xff43, 0x0602)); // wired G-series keyboard carries both
    assert!(!is_long_only_collection(0x0001, 0x0002)); // not a HID++ collection at all
}

#[test]
fn short_and_long_collections_of_one_interface_share_a_grouping_key() {
    // Real Bolt receiver paths: the short (Col01) and long (Col02) HID++
    // collections of interface MI_02 must collapse to the same key.
    let short = normalize_collection_path(
        r"\\?\HID#VID_046D&PID_C548&MI_02&Col01#7&348660ac&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
    );
    let long = normalize_collection_path(
        r"\\?\HID#VID_046D&PID_C548&MI_02&Col02#7&348660ac&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
    );
    assert_eq!(short, long);
    assert_eq!(short, "vid_046d&pid_c548&mi_02#7&348660ac&0");
}

#[test]
fn distinct_interfaces_do_not_share_a_grouping_key() {
    // A different interface (MI_01) on the same receiver has its own instance
    // hash, so it must not pair with MI_02's HID++ collections.
    let mi01 = normalize_collection_path(
        r"\\?\HID#VID_046D&PID_C548&MI_01&Col02#7&1cc2d467&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
    );
    let mi02 = normalize_collection_path(
        r"\\?\HID#VID_046D&PID_C548&MI_02&Col02#7&348660ac&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
    );
    assert_ne!(mi01, mi02);
}

#[test]
fn distinct_physical_receivers_do_not_share_a_grouping_key() {
    // Two receivers plugged in at once (here two identical Bolt receivers,
    // same VID/PID/interface/collection) must not cross-pair: each physical
    // device has a distinct instance hash, which the key preserves. This is
    // the multi-receiver scenario the single-interface tests don't cover.
    let recv_a = normalize_collection_path(
        r"\\?\HID#VID_046D&PID_C548&MI_02&Col01#7&348660ac&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
    );
    let recv_b = normalize_collection_path(
        r"\\?\HID#VID_046D&PID_C548&MI_02&Col01#7&9f1be20c&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
    );
    assert_ne!(recv_a, recv_b);

    // A Bolt + a Unifying receiver (different PID) must also stay distinct.
    let bolt = normalize_collection_path(
        r"\\?\HID#VID_046D&PID_C548&MI_02&Col02#7&348660ac&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
    );
    let unifying = normalize_collection_path(
        r"\\?\HID#VID_046D&PID_C52B&MI_02&Col02#7&1a2b3c4d&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
    );
    assert_ne!(bolt, unifying);
}

// Sysfs path: child of Unifying receiver
const UNIFYING_CHILD: &str = "/sys/devices/pci0000:00/0000:00:14.0/usb3/3-5/3-5.4/3-5.4.3/\
     3-5.4.3:1.2/0003:046D:C52B.0009/0003:046D:4076.000A";
// Sysfs path: the Unifying receiver node itself (terminal component has C52B)
const UNIFYING_RECEIVER: &str = "/sys/devices/pci0000:00/0000:00:14.0/usb3/3-5/3-5.4/3-5.4.3/\
     3-5.4.3:1.2/0003:046D:C52B.0009";
// Sysfs path: child of Bolt receiver
const BOLT_CHILD: &str = "/sys/devices/pci0000:00/0000:00:14.0/usb3/3-5/\
     0003:046D:C548.0001/0003:046D:B037.0002";
// Sysfs path: child of Lightspeed receiver
const LIGHTSPEED_CHILD: &str = "/sys/devices/pci0000:00/0000:00:14.0/usb3/3-5/\
     0003:046D:C547.0001/0003:046D:4099.0002";
// Sysfs path: unrelated non-Logitech device
const UNRELATED: &str = "/sys/devices/pci0000:00/0000:00:15.0/i2c-0/0018:06CB:CE67.0001";

#[test]
fn child_of_unifying_receiver_is_detected() {
    assert!(is_receiver_child_sysfs_path(UNIFYING_CHILD));
}

#[test]
fn unifying_receiver_itself_is_not_a_child() {
    assert!(!is_receiver_child_sysfs_path(UNIFYING_RECEIVER));
}

#[test]
fn child_of_bolt_receiver_is_detected() {
    assert!(is_receiver_child_sysfs_path(BOLT_CHILD));
}

#[test]
fn child_of_lightspeed_receiver_is_detected() {
    assert!(is_receiver_child_sysfs_path(LIGHTSPEED_CHILD));
}

#[test]
fn unrelated_device_is_not_a_child() {
    assert!(!is_receiver_child_sysfs_path(UNRELATED));
}
