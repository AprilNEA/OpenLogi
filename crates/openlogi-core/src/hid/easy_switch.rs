//! Easy-Switch host slots — pure data, no I/O.
//!
//! The HID++ `0x1815 HostsInfo` reads and the rename that produce and change
//! an [`EasySwitchHosts`] live in `openlogi_device::write::easy_switch`.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::error::WriteError;

/// How a host slot reaches its computer.
///
/// Logi Options+ 2.8 shows Bluetooth classic and Bluetooth Low Energy alike,
/// so they share one variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostConnection {
    /// The device reports no bus for the slot.
    Unknown,
    /// A Unifying (eQuad) receiver.
    Unifying,
    /// A USB cable.
    Usb,
    /// Bluetooth, classic or Low Energy.
    Bluetooth,
    /// A Logi Bolt receiver.
    Bolt,
}

/// An operating system a host slot can record.
///
/// The values Logi Options+ 2.8 writes to and reads from `0x1815`. "No OS
/// recorded" is the absence of a [`HostOsInfo`], not a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostOsKind {
    /// Windows.
    Windows,
    /// Windows Embedded.
    WindowsEmbedded,
    /// Linux.
    Linux,
    /// ChromeOS.
    ChromeOs,
    /// Android.
    Android,
    /// macOS.
    MacOs,
    /// iOS or iPadOS.
    Ios,
    /// webOS.
    WebOs,
    /// Tizen.
    Tizen,
}

impl fmt::Display for HostOsKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Windows => "Windows",
            Self::WindowsEmbedded => "Windows Embedded",
            Self::Linux => "Linux",
            Self::ChromeOs => "ChromeOS",
            Self::Android => "Android",
            Self::MacOs => "macOS",
            Self::Ios => "iOS",
            Self::WebOs => "webOS",
            Self::Tizen => "Tizen",
        })
    }
}

/// The OS and version the computer on a slot recorded in the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostOsInfo {
    /// The operating system.
    pub kind: HostOsKind,
    /// Major version.
    pub major: u8,
    /// Minor version.
    pub minor: u16,
    /// Patch version.
    pub patch: u16,
}

impl fmt::Display for HostOsInfo {
    /// The OS and `major.minor`, as Logi Options+ shows it; a version of
    /// `0.0` is left out.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.major == 0 && self.minor == 0 {
            write!(f, "{}", self.kind)
        } else {
            write!(f, "{} {}.{}", self.kind, self.major, self.minor)
        }
    }
}

/// One Easy-Switch channel of a multi-host device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EasySwitchSlot {
    /// Zero-based slot index; the device labels it `index + 1`.
    pub index: u8,
    /// Whether a computer is paired on the slot.
    pub paired: bool,
    /// How the slot reaches its computer.
    pub connection: HostConnection,
    /// The friendly name the device stores for the slot's computer. `None`
    /// when the slot is empty, stores no name, or the device cannot report
    /// names.
    pub name: Option<String>,
    /// The longest name, in UTF-8 bytes, the device stores for the slot.
    pub name_max_len: u8,
    /// The OS the slot's computer recorded. `None` when the slot is empty,
    /// records none, or the device cannot report it.
    pub os: Option<HostOsInfo>,
}

/// The Easy-Switch channels of a multi-host device, read from `0x1815`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EasySwitchHosts {
    /// The slot the device is talking to now, when it names one.
    pub current: Option<u8>,
    /// Every slot, in index order.
    pub slots: Vec<EasySwitchSlot>,
    /// Whether the device lets software rename a slot's computer.
    pub can_rename: bool,
}

impl EasySwitchHosts {
    /// The slot at `index`, if the device has one.
    #[must_use]
    pub fn slot(&self, index: u8) -> Option<&EasySwitchSlot> {
        self.slots.iter().find(|slot| slot.index == index)
    }

    /// The name to write when `name` is asked for slot `index`: `name`
    /// without surrounding whitespace, checked the way the device layer
    /// checks it before writing. A device that cannot rename fails with
    /// [`WriteError::HostRenameUnsupported`], an empty or missing slot with
    /// [`WriteError::HostSlotEmpty`], and a name that is empty, holds a NUL
    /// or is longer than the slot stores with
    /// [`WriteError::HostNameRejected`].
    pub fn name_for<'a>(&self, index: u8, name: &'a str) -> Result<&'a str, WriteError> {
        if !self.can_rename {
            return Err(WriteError::HostRenameUnsupported);
        }
        let slot = self
            .slot(index)
            .filter(|slot| slot.paired)
            .ok_or(WriteError::HostSlotEmpty { slot: index })?;
        let name = name.trim();
        if name.is_empty() || name.contains('\0') || name.len() > usize::from(slot.name_max_len) {
            return Err(WriteError::HostNameRejected {
                max_len: slot.name_max_len,
            });
        }
        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EasySwitchHosts, EasySwitchSlot, HostConnection, HostOsInfo, HostOsKind, WriteError,
    };

    fn slot(index: u8, paired: bool) -> EasySwitchSlot {
        EasySwitchSlot {
            index,
            paired,
            connection: HostConnection::Bluetooth,
            name: paired.then(|| "Studio".to_owned()),
            name_max_len: 8,
            os: None,
        }
    }

    fn hosts(can_rename: bool) -> EasySwitchHosts {
        EasySwitchHosts {
            current: Some(0),
            slots: vec![slot(0, true), slot(1, false)],
            can_rename,
        }
    }

    #[test]
    fn a_name_is_trimmed_and_must_fit_the_slot() {
        let hosts = hosts(true);
        assert_eq!(hosts.name_for(0, "  Laptop \n"), Ok("Laptop"));
        assert_eq!(hosts.name_for(0, "12345678"), Ok("12345678"));
        for rejected in ["", "   ", "123456789", "a\0b"] {
            assert_eq!(
                hosts.name_for(0, rejected),
                Err(WriteError::HostNameRejected { max_len: 8 }),
                "{rejected:?}"
            );
        }
        // Eight bytes, not eight characters: "ü" is two bytes in UTF-8.
        assert_eq!(
            hosts.name_for(0, "üüüüü"),
            Err(WriteError::HostNameRejected { max_len: 8 })
        );
    }

    #[test]
    fn only_a_paired_slot_of_a_renaming_device_takes_a_name() {
        assert_eq!(
            hosts(false).name_for(0, "Laptop"),
            Err(WriteError::HostRenameUnsupported)
        );
        assert_eq!(
            hosts(true).name_for(1, "Laptop"),
            Err(WriteError::HostSlotEmpty { slot: 1 })
        );
        assert_eq!(
            hosts(true).name_for(2, "Laptop"),
            Err(WriteError::HostSlotEmpty { slot: 2 })
        );
    }

    #[test]
    fn an_os_shows_major_and_minor_unless_both_are_zero() {
        let os = |major, minor| HostOsInfo {
            kind: HostOsKind::MacOs,
            major,
            minor,
            patch: 3,
        };
        assert_eq!(os(15, 2).to_string(), "macOS 15.2");
        assert_eq!(os(10, 0).to_string(), "macOS 10.0");
        assert_eq!(os(0, 0).to_string(), "macOS");
    }
}
