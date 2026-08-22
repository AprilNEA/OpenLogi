//! Pure `classify` cases — no device nodes involved.

use crate::PermissionStatus;
use crate::linux::classify;

#[test]
fn classify_granted_when_all_ok() {
    assert_eq!(
        classify(true, Some(true), Some(true)),
        PermissionStatus::Granted
    );
}

#[test]
fn classify_denied_when_uinput_not_ok() {
    assert_eq!(
        classify(false, Some(true), Some(true)),
        PermissionStatus::Denied
    );
    assert_eq!(
        classify(false, Some(false), Some(false)),
        PermissionStatus::Denied
    );
    assert_eq!(classify(false, None, None), PermissionStatus::Denied);
}

#[test]
fn classify_denied_when_hidraw_denied() {
    assert_eq!(
        classify(true, Some(false), Some(true)),
        PermissionStatus::Denied
    );
}

/// The case this probe exists for: rules left by another Logitech manager cover
/// uinput and hidraw but not the mouse's event node, so HID++ works while the
/// hook cannot grab the mouse. Reporting `Granted` here is what made button
/// remapping look like it was silently broken.
#[test]
fn classify_denied_when_event_node_denied() {
    assert_eq!(
        classify(true, Some(true), Some(false)),
        PermissionStatus::Denied
    );
}

#[test]
fn classify_unknown_when_no_logitech_device_connected() {
    assert_eq!(classify(true, None, None), PermissionStatus::Unknown);
}

/// No event node found is not evidence of a permission problem — with an
/// accessible hidraw it points at sysfs, so it must not downgrade a working
/// machine to `Denied`.
#[test]
fn classify_granted_when_event_node_absent() {
    assert_eq!(classify(true, Some(true), None), PermissionStatus::Granted);
}

mod event_nodes {
    use std::io::ErrorKind;

    use crate::linux::fold_event_access;

    /// The case Greptile flagged: one readable Logitech node must not mask a
    /// denied one, whichever order the walk happens to reach them in. Stopping
    /// at the first readable node would report `Granted` for a machine whose
    /// mouse the hook cannot open.
    #[test]
    fn a_denied_node_outweighs_a_readable_one_in_either_order() {
        let denied_first = [Err(ErrorKind::PermissionDenied), Ok(())];
        let readable_first = [Ok(()), Err(ErrorKind::PermissionDenied)];
        assert_eq!(fold_event_access(denied_first), Some(false));
        assert_eq!(fold_event_access(readable_first), Some(false));
    }

    #[test]
    fn all_readable_is_accessible() {
        assert_eq!(fold_event_access([Ok(()), Ok(())]), Some(true));
    }

    #[test]
    fn no_logitech_nodes_is_unknown() {
        assert_eq!(fold_event_access([]), None);
    }

    /// A device unplugged mid-walk is neither evidence of access nor of its
    /// absence, so it must not decide the verdict on its own.
    #[test]
    fn non_permission_errors_are_ignored() {
        assert_eq!(fold_event_access([Err(ErrorKind::NotFound)]), None);
        assert_eq!(
            fold_event_access([Err(ErrorKind::NotFound), Ok(())]),
            Some(true)
        );
    }
}

/// Capability-bitmap decoding, using the strings a real MX Master 3S and a real
/// G502 publish. The G502's second node is the reason this filter exists: it is
/// a Logitech event node with no `uaccess` ACL on a correctly configured
/// machine, so counting it would report a working system as `Denied`.
mod capability_bits {
    use crate::linux::has_capability_bit;

    const REL_X: usize = 0x00;
    const REL_Y: usize = 0x01;
    const BTN_LEFT: usize = 0x110;

    /// `Logitech MX Master 3S` / `Logitech G502 HERO Gaming Mouse`.
    const MOUSE_REL: &str = "1943";
    const MOUSE_KEY: &str = "ffff0000 0 0 0 0";

    /// `Logitech G502 HERO Gaming Mouse Keyboard`.
    const KEYBOARD_REL: &str = "1040";
    const KEYBOARD_KEY: &str = "733eff 0 0 483ffff17aff32d bfd4444600000000 1 \
                                130ff38b17c007 ffe77bfad941dfff febeffdfffefffff \
                                fffffffffffffffe";

    #[test]
    fn a_mouse_node_clicks_and_moves() {
        assert!(has_capability_bit(MOUSE_REL, REL_X));
        assert!(has_capability_bit(MOUSE_REL, REL_Y));
        assert!(has_capability_bit(MOUSE_KEY, BTN_LEFT));
    }

    #[test]
    fn the_companion_keyboard_node_does_neither() {
        assert!(!has_capability_bit(KEYBOARD_REL, REL_X));
        assert!(!has_capability_bit(KEYBOARD_REL, REL_Y));
        assert!(!has_capability_bit(KEYBOARD_KEY, BTN_LEFT));
    }

    /// The words run most significant first, so a bit beyond the printed width
    /// is absent rather than wrapping onto some other word.
    #[test]
    fn bits_past_the_bitmap_are_absent() {
        assert!(!has_capability_bit(MOUSE_KEY, 0x400));
        assert!(!has_capability_bit("", REL_X));
        assert!(!has_capability_bit("zz", REL_X));
    }
}
