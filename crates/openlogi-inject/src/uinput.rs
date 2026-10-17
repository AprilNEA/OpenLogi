//! The uinput devices OpenLogi creates on Linux, and how to recognize them.
//!
//! Every uinput creator and the Linux hook must make two decisions the same
//! way, so both live here. Each device name carries one private prefix that
//! [`is_own_device`] matches, so the hook never grabs a device OpenLogi
//! created. Each name also avoids the words GNOME Shell's X11 backend reads
//! as a device type.
//!
//! That backend (mutter 48 and earlier, `create_device` in
//! `src/backends/x11/meta-seat-x11.c`) guesses the type of a device that
//! reports none from its lowercased name. A name that contains "pen" or
//! "wacom" becomes a stylus, and mutter then applies the stylus button map,
//! which swaps the middle and right buttons (#1842). "eraser", "cursor",
//! " pad", and "touchpad" select other tablet or touchpad types. "OpenLogi"
//! contains "pen", so no device name can carry the product name.

use std::io;

use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};

/// Prefix of every uinput device name OpenLogi creates.
const PREFIX: &str = "OL ";

/// Prefix the same devices carried in earlier releases. A development agent
/// can run next to an older installed agent and must not grab its devices.
const LEGACY_PREFIX: &str = "OpenLogi ";

/// A uinput device OpenLogi creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    /// The pass-through mouse that re-injects the events of one grabbed mouse.
    PassThroughMouse,
    /// The device that synthesizes the keys, clicks, and scrolls of bound
    /// actions.
    ActionInjector,
}

impl Device {
    /// Every variant, so the naming tests cannot miss a new device.
    #[cfg(test)]
    const ALL: [Self; 2] = [Self::PassThroughMouse, Self::ActionInjector];

    /// The name the device registers with the kernel, and that libinput, the
    /// display server, and `xinput` show.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PassThroughMouse => "OL virtual mouse",
            Self::ActionInjector => "OL action injector",
        }
    }
}

/// Start a uinput builder for `device` that already carries its name.
///
/// # Errors
///
/// Returns the error from opening `/dev/uinput`.
pub fn builder(device: Device) -> io::Result<VirtualDeviceBuilder<'static>> {
    Ok(VirtualDevice::builder()?.name(device.name()))
}

/// Whether `name` belongs to a uinput device that OpenLogi created, in this
/// release or an earlier one.
#[must_use]
pub fn is_own_device(name: &str) -> bool {
    name.starts_with(PREFIX) || name.starts_with(LEGACY_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The substrings mutter 48's X11 backend looks for in a lowercased
    /// device name to choose a tablet or touchpad type.
    const MUTTER_TYPE_WORDS: [&str; 6] = ["eraser", "cursor", " pad", "wacom", "pen", "touchpad"];

    #[test]
    fn every_device_name_is_recognized_as_own() {
        for device in Device::ALL {
            assert!(
                is_own_device(device.name()),
                "{device:?} ({:?}) would be grabbed by the hook",
                device.name()
            );
        }
    }

    #[test]
    fn no_device_name_reads_as_a_tablet_or_touchpad_to_mutter() {
        for device in Device::ALL {
            let name = device.name().to_ascii_lowercase();
            for word in MUTTER_TYPE_WORDS {
                assert!(
                    !name.contains(word),
                    "{device:?} ({name:?}) contains {word:?}, which GNOME Shell on X11 \
                     reads as a tablet or touchpad"
                );
            }
        }
    }

    #[test]
    fn devices_from_earlier_releases_are_recognized_as_own() {
        assert!(is_own_device("OpenLogi virtual mouse"));
        assert!(is_own_device("OpenLogi action injector"));
    }

    #[test]
    fn physical_and_foreign_devices_are_not_own() {
        for name in [
            "Logitech MX Master 3S",
            "MX Master 3S",
            "Logitech USB Receiver Mouse",
            "OLED Touch Panel",
            "OL",
            "",
        ] {
            assert!(!is_own_device(name), "{name:?} is not an OpenLogi device");
        }
    }
}
