//! Probe-classification cases, plus the uinput open mode on scratch files —
//! no device nodes involved.

use crate::PermissionStatus;
use crate::linux::{EventProbe, HidrawProbe, Probes};

/// The whole truth table: any definite denial → `Denied`; otherwise an
/// accessible hidraw → `Granted` and no hidraw → `Unknown`. A missing event
/// node is not a denial, so it never downgrades a working machine.
#[test]
fn every_probe_combination_maps_to_its_verdict() {
    use EventProbe::{Denied as EventDenied, NonePresent as NoEvent, Readable};
    use HidrawProbe::{Accessible, Denied as HidrawDenied, NonePresent as NoHidraw};
    use PermissionStatus::{Denied, Granted, Unknown};

    let cases = [
        (true, Accessible, Readable, Granted),
        (true, Accessible, EventDenied, Denied),
        (true, Accessible, NoEvent, Granted),
        (true, HidrawDenied, Readable, Denied),
        (true, HidrawDenied, EventDenied, Denied),
        (true, HidrawDenied, NoEvent, Denied),
        (true, NoHidraw, Readable, Unknown),
        (true, NoHidraw, EventDenied, Denied),
        (true, NoHidraw, NoEvent, Unknown),
        (false, Accessible, Readable, Denied),
        (false, Accessible, EventDenied, Denied),
        (false, Accessible, NoEvent, Denied),
        (false, HidrawDenied, Readable, Denied),
        (false, HidrawDenied, EventDenied, Denied),
        (false, HidrawDenied, NoEvent, Denied),
        (false, NoHidraw, Readable, Denied),
        (false, NoHidraw, EventDenied, Denied),
        (false, NoHidraw, NoEvent, Denied),
    ];
    for (uinput_read_write, hidraw, event, expected) in cases {
        let probes = Probes {
            uinput_read_write,
            hidraw,
            event,
        };
        assert_eq!(
            PermissionStatus::from(probes),
            expected,
            "probes: {probes:?}"
        );
    }
}

mod event_nodes {
    use std::io::ErrorKind;

    use crate::linux::{EventProbe, fold_event_access};

    /// The case Greptile flagged: one readable Logitech node must not mask a
    /// denied one, whichever order the walk happens to reach them in. Stopping
    /// at the first readable node would report `Granted` for a machine whose
    /// mouse the hook cannot open.
    #[test]
    fn a_denied_node_outweighs_a_readable_one_in_either_order() {
        let denied_first = [Err(ErrorKind::PermissionDenied), Ok(())];
        let readable_first = [Ok(()), Err(ErrorKind::PermissionDenied)];
        assert_eq!(fold_event_access(denied_first), EventProbe::Denied);
        assert_eq!(fold_event_access(readable_first), EventProbe::Denied);
    }

    #[test]
    fn all_readable_is_accessible() {
        assert_eq!(fold_event_access([Ok(()), Ok(())]), EventProbe::Readable);
    }

    #[test]
    fn no_logitech_nodes_is_unknown() {
        assert_eq!(fold_event_access([]), EventProbe::NonePresent);
    }

    /// A device unplugged mid-walk is neither evidence of access nor of its
    /// absence, so it must not decide the verdict on its own.
    #[test]
    fn non_permission_errors_are_ignored() {
        assert_eq!(
            fold_event_access([Err(ErrorKind::NotFound)]),
            EventProbe::NonePresent
        );
        assert_eq!(
            fold_event_access([Err(ErrorKind::NotFound), Ok(())]),
            EventProbe::Readable
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

/// Which `/dev/input` nodes count, over fixture sysfs attributes — the shape of
/// #1959's report: the receiver's mouse node carries a `uaccess` ACL, and the
/// agent's pass-through mouse, which mirrors that mouse's vendor and
/// capabilities, does not. And #2365's: an MX Keys node that clicks and moves,
/// which udev does not classify as a mouse, so the rules leave it root-owned.
mod event_node_selection {
    use std::io::ErrorKind;

    use openlogi_core::uinput::Device;

    use crate::linux::{EventNodes, EventProbe, UdevMouse, classify_event_nodes};

    const MOUSE_REL: &str = "1943\n";
    const MOUSE_KEY: &str = "ffff0000 0 0 0 0\n";

    /// One fixture node: its `/dev/input` name, sysfs `device/name`, udev's
    /// classification, and whether `open()` succeeds.
    #[derive(Clone, Copy)]
    struct Node {
        entry: &'static str,
        name: &'static str,
        udev: UdevMouse,
        readable: bool,
    }

    /// Every node is a Logitech mouse by vendor and capabilities; only the
    /// name, udev's classification and the access differ.
    struct Fixture<'a>(&'a [Node]);

    impl Fixture<'_> {
        fn find(&self, node: &str) -> Node {
            *self
                .0
                .iter()
                .find(|n| n.entry == node)
                .expect("fixture node")
        }
    }

    impl EventNodes for Fixture<'_> {
        fn attr(&self, node: &str, attr: &str) -> Option<String> {
            let value = match attr {
                "name" => format!("{}\n", self.find(node).name),
                "id/vendor" => "046d\n".to_owned(),
                "capabilities/rel" => MOUSE_REL.to_owned(),
                "capabilities/key" => MOUSE_KEY.to_owned(),
                _ => return None,
            };
            Some(value)
        }

        fn udev_mouse(&self, node: &str) -> UdevMouse {
            self.find(node).udev
        }

        fn open(&self, node: &str) -> Result<(), ErrorKind> {
            if self.find(node).readable {
                Ok(())
            } else {
                Err(ErrorKind::PermissionDenied)
            }
        }
    }

    fn classify(nodes: &[Node]) -> EventProbe {
        classify_event_nodes(nodes.iter().map(|n| n.entry), &Fixture(nodes))
    }

    const PHYSICAL: Node = Node {
        entry: "event7",
        name: "Logitech USB Receiver Mouse",
        udev: UdevMouse::Mouse,
        readable: true,
    };
    const PASS_THROUGH: Node = Node {
        entry: "event31",
        name: Device::PassThroughMouse.name(),
        udev: UdevMouse::Mouse,
        readable: false,
    };
    /// An MX Keys on a Unifying receiver: its node reports `REL_X`, `REL_Y`
    /// and `BTN_LEFT`, and systemd's hwdb sets `ID_INPUT_MOUSE=0` for it.
    const MX_KEYS: Node = Node {
        entry: "event3",
        name: "Logitech MX Keys",
        udev: UdevMouse::NotMouse,
        readable: false,
    };

    // Earlier releases' device names are covered where they are recognized,
    // in `openlogi_core::uinput`'s own tests.
    #[test]
    fn our_own_pass_through_mouse_is_ignored() {
        assert_eq!(classify(&[PHYSICAL, PASS_THROUGH]), EventProbe::Readable);
    }

    #[test]
    fn a_keyboard_udev_does_not_call_a_mouse_is_ignored() {
        assert_eq!(classify(&[MX_KEYS, PHYSICAL]), EventProbe::Readable);
        assert_eq!(classify(&[MX_KEYS]), EventProbe::NonePresent);
    }

    /// Without udev's database the capability test decides alone, so a denied
    /// node still reports `Denied` there.
    #[test]
    fn an_unrecorded_node_still_counts() {
        let unrecorded = Node {
            udev: UdevMouse::Unrecorded,
            ..MX_KEYS
        };
        assert_eq!(classify(&[unrecorded, PHYSICAL]), EventProbe::Denied);
    }

    #[test]
    fn an_unreadable_physical_mouse_is_still_denied() {
        let denied = Node {
            readable: false,
            ..PHYSICAL
        };
        assert_eq!(
            classify(&[denied, PASS_THROUGH, MX_KEYS]),
            EventProbe::Denied
        );
    }

    #[test]
    fn only_our_own_nodes_is_none_present() {
        assert_eq!(classify(&[PASS_THROUGH]), EventProbe::NonePresent);
    }

    /// Non-event entries (`mouse0`, `by-id`) are never candidates, even when
    /// they would otherwise look like a mouse.
    #[test]
    fn non_event_entries_are_skipped() {
        let mouse0 = Node {
            entry: "mouse0",
            readable: false,
            ..PHYSICAL
        };
        assert_eq!(classify(&[PHYSICAL, mouse0]), EventProbe::Readable);
    }
}

/// udev database records, trimmed to the lines that matter, in the format
/// `/run/udev/data/c13:<minor>` holds them.
mod udev_records {
    use crate::linux::UdevMouse;

    /// A Logitech mouse node behind a Unifying receiver.
    const MOUSE: &str = "I:4211732\n\
                         E:ID_INPUT=1\n\
                         E:ID_INPUT_MOUSE=1\n\
                         E:ID_BUS=usb\n\
                         G:uaccess\n\
                         G:seat\n";

    /// The MX Keys after systemd's hwdb override: a keyboard, not a mouse.
    const MX_KEYS: &str = "I:4211740\n\
                           E:ID_INPUT=1\n\
                           E:ID_INPUT_KEY=1\n\
                           E:ID_INPUT_KEYBOARD=1\n\
                           E:ID_INPUT_MOUSE=0\n\
                           E:ID_BUS=usb\n\
                           G:seat\n";

    #[test]
    fn a_mouse_record_is_a_mouse() {
        assert_eq!(UdevMouse::from_record(MOUSE), UdevMouse::Mouse);
    }

    #[test]
    fn an_overridden_keyboard_is_not_a_mouse() {
        assert_eq!(UdevMouse::from_record(MX_KEYS), UdevMouse::NotMouse);
    }

    /// `input_id` leaves the property out entirely for a node it does not
    /// call a mouse, and a longer name that merely starts with it is another
    /// property.
    #[test]
    fn a_record_without_the_property_is_not_a_mouse() {
        assert_eq!(
            UdevMouse::from_record("E:ID_INPUT=1\nE:ID_INPUT_KEYBOARD=1\n"),
            UdevMouse::NotMouse
        );
        assert_eq!(
            UdevMouse::from_record("E:ID_INPUT_MOUSE_DPI=1\n"),
            UdevMouse::NotMouse
        );
        assert_eq!(UdevMouse::from_record(""), UdevMouse::NotMouse);
    }
}

/// #1414: another package's rule set `/dev/uinput` to `0620`, so the `uaccess`
/// ACL granted write alone. Device creation opens the node read/write and
/// failed there, while a write-only probe still reported access.
mod uinput_node {
    use std::fs::{self, File, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use crate::linux::probe_uinput_at;

    /// A scratch file that stands in for `/dev/uinput`, removed on drop.
    struct Node(PathBuf);

    impl Node {
        fn path(name: &str) -> PathBuf {
            std::env::temp_dir().join(format!(
                "openlogi-uinput-probe-{}-{name}",
                std::process::id()
            ))
        }

        fn with_mode(name: &str, mode: u32) -> Self {
            let path = Self::path(name);
            fs::write(&path, b"").unwrap();
            fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
            Self(path)
        }
    }

    impl Drop for Node {
        fn drop(&mut self) {
            fs::remove_file(&self.0).ok();
        }
    }

    #[test]
    fn a_write_only_node_is_not_accessible() {
        let node = Node::with_mode("write-only", 0o200);
        // Root ignores the mode bits, so it has no write-only node to probe.
        if File::open(&node.0).is_ok() {
            return;
        }
        assert!(!probe_uinput_at(&node.0));
    }

    #[test]
    fn a_read_write_node_is_accessible() {
        let node = Node::with_mode("read-write", 0o600);
        assert!(probe_uinput_at(&node.0));
    }

    #[test]
    fn a_missing_node_is_not_accessible() {
        assert!(!probe_uinput_at(&Node::path("missing")));
    }
}
