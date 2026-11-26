//! What udev's database says a Linux event node is, and whether that lets the
//! node count as a mouse.
//!
//! systemd's `input_id` builtin and its hwdb classify every
//! `/dev/input/event*` node. `70-openlogi.rules` grants only the nodes they
//! call a mouse (`ID_INPUT_MOUSE=1`), and libinput treats only those as
//! pointers. A few Logitech keyboards report `REL_X`, `REL_Y` and `BTN_LEFT`
//! on their receiver node — the MX Keys and the Craft on a Unifying receiver —
//! so a capability test takes them for a mouse, and the hwdb sets
//! `ID_INPUT_MOUSE=0` for exactly that reason.
//!
//! Two decisions follow this classification and must agree: which nodes the
//! Linux hook grabs, and which nodes the input access probe expects the user
//! to read. The probe counted the MX Keys' root-owned node as a denial (#2365)
//! while the hook, where that node was readable, grabbed the keyboard and
//! re-injected every keystroke (#2367). The rule lives here rather than in
//! either consumer for the reason [`crate::uinput`] does: the probe's crate
//! should not link evdev and zbus to know it.

use std::fs;
use std::path::Path;

/// udev's answer to whether an event node is a mouse.
///
/// Ask [`UdevMouse::allows_mouse`] what the answer means for a node that
/// clicks and moves; do not compare the variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdevMouse {
    /// The record sets `ID_INPUT_MOUSE=1`.
    Mouse,
    /// The record does not: the `input_id` builtin, or an hwdb entry such as
    /// the MX Keys' `ID_INPUT_MOUSE=0`, classified the node as something else.
    NotMouse,
    /// udev keeps no record of the node that this process can read.
    Unrecorded,
}

impl UdevMouse {
    /// Read udev's record of the event node `/dev/input/<node>`, where `node`
    /// is the entry name, such as `event3`.
    #[must_use]
    pub fn of_event_node(node: &str) -> Self {
        Self::of_event_node_under(Path::new("/"), node)
    }

    /// [`Self::of_event_node`] with sysfs and udev's database under `root`
    /// instead of `/`, so the tests can lay them out in a temporary directory.
    ///
    /// udev keys its record of a character device by `c<major>:<minor>`, which
    /// sysfs `dev` holds as `13:67`.
    fn of_event_node_under(root: &Path, node: &str) -> Self {
        fs::read_to_string(root.join(format!("sys/class/input/{node}/dev")))
            .and_then(|dev| fs::read_to_string(root.join(format!("run/udev/data/c{}", dev.trim()))))
            .map_or(Self::Unrecorded, |record| Self::from_record(&record))
    }

    /// Classify a udev database record, whose `E:` lines are the node's
    /// properties.
    fn from_record(record: &str) -> Self {
        if record.lines().any(|line| line == "E:ID_INPUT_MOUSE=1") {
            Self::Mouse
        } else {
            Self::NotMouse
        }
    }

    /// Whether a node that clicks and moves may count as a mouse: the hook may
    /// grab it, and the probe expects it to be readable.
    ///
    /// Only [`UdevMouse::NotMouse`] rules a node out. Without a record (no
    /// udev, or a sandbox that hides its database) the caller's capability
    /// test decides alone, as it did before udev was consulted.
    ///
    /// udev writes a new node's record only after it has set the node's group
    /// and mode (`update_devnode` before `device_update_db` in systemd's
    /// `udev_event_execute_rules`), so for that moment a node is readable to
    /// the `input` group and still unrecorded. A hook rescan that lands there
    /// can grab such a keyboard. Treating an unrecorded node as not yet a
    /// mouse would close that gap, but nothing wakes the hook when the record
    /// appears, so every reconnected mouse that lost the same race would wait
    /// for the next heartbeat rescan instead.
    #[must_use]
    pub const fn allows_mouse(self) -> bool {
        !matches!(self, Self::NotMouse)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::UdevMouse;

    /// A Logitech mouse node behind a Unifying receiver, trimmed to the lines
    /// that matter, in the format `/run/udev/data/c13:<minor>` holds it.
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

    #[test]
    fn only_a_node_udev_calls_something_else_is_ruled_out() {
        assert!(UdevMouse::Mouse.allows_mouse());
        assert!(!UdevMouse::NotMouse.allows_mouse());
        assert!(UdevMouse::Unrecorded.allows_mouse());
    }

    /// Lay out one event node: its sysfs `dev`, and udev's record under that
    /// key when there is one.
    fn node(root: &Path, entry: &str, dev: &str, record: Option<&str>) {
        let sysfs = root.join("sys/class/input").join(entry);
        fs::create_dir_all(&sysfs).expect("sysfs dir");
        fs::write(sysfs.join("dev"), format!("{dev}\n")).expect("sysfs dev");
        if let Some(record) = record {
            let data = root.join("run/udev/data");
            fs::create_dir_all(&data).expect("udev data dir");
            fs::write(data.join(format!("c{dev}")), record).expect("udev record");
        }
    }

    /// The record is found through the node's device number, not its name:
    /// sysfs `dev` reads `13:67`, and udev files the record as `c13:67`.
    #[test]
    fn the_record_is_read_through_the_device_number() {
        let root = tempfile::tempdir().expect("tempdir");
        node(root.path(), "event3", "13:67", Some(MX_KEYS));
        node(root.path(), "event6", "13:70", Some(MOUSE));
        node(root.path(), "event9", "13:73", None);

        let of = |entry| UdevMouse::of_event_node_under(root.path(), entry);
        assert_eq!(of("event3"), UdevMouse::NotMouse);
        assert_eq!(of("event6"), UdevMouse::Mouse);
        assert_eq!(of("event9"), UdevMouse::Unrecorded);
        assert_eq!(of("event12"), UdevMouse::Unrecorded, "no sysfs entry");
    }
}
