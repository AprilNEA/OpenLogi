//! The speed a camera's USB link runs at, and the speed the camera could use.
//!
//! A UVC camera lists only the modes its link has the bandwidth for. On a
//! USB 2.0 port, hub or cable a camera built for USB 3 therefore reports fewer
//! resolutions and frame rates than it delivers on a SuperSpeed link, and
//! nothing in that list says so.
//!
//! The camera does say what it is built for. A device that supports SuperSpeed
//! has to declare it in its BOS descriptor (USB 3.2 §9.6.2) however it
//! enumerated, so comparing that with the speed the link actually runs at
//! answers "could this camera do more?" for any model, with no table to
//! maintain. What it cannot answer is *which* modes are hidden: the camera
//! only lists what fits the link it has.
//!
//! Where each platform gets both numbers is its own: macOS reads the registry
//! and the BOS descriptor itself, Windows asks the hub driver, which has read
//! them. Each records what a scan found with `remember_links`, and
//! [`usb_link()`] reads it back.

use std::collections::HashMap;
use std::fmt;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

/// A USB signalling speed, slowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsbSpeed {
    /// USB 1.0 low speed, 1.5 Mbps.
    Low,
    /// USB 1.1 full speed, 12 Mbps.
    Full,
    /// USB 2.0 high speed, 480 Mbps.
    High,
    /// USB 3.0 SuperSpeed, 5 Gbps.
    Super,
    /// USB 3.1 SuperSpeed+, 10 Gbps and up.
    SuperPlus,
}

impl fmt::Display for UsbSpeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Low => "USB 1.0 (1.5 Mbps)",
            Self::Full => "USB 1.1 (12 Mbps)",
            Self::High => "USB 2.0 (480 Mbps)",
            Self::Super => "USB 3.0 (5 Gbps)",
            Self::SuperPlus => "USB 3.1 (10 Gbps)",
        })
    }
}

#[cfg(any(test, target_os = "macos"))]
impl UsbSpeed {
    /// The speed class of a link running at `bits_per_second`, as the OS
    /// reports it. `None` for a rate below low speed, which is no USB link.
    pub(crate) const fn from_bits_per_second(bits_per_second: u64) -> Option<Self> {
        Some(match bits_per_second {
            10_000_000_000.. => Self::SuperPlus,
            5_000_000_000.. => Self::Super,
            480_000_000.. => Self::High,
            12_000_000.. => Self::Full,
            1_500_000.. => Self::Low,
            _ => return None,
        })
    }
}

/// How a camera is attached: the speed its link runs at, and the highest one
/// the camera says it supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbLink {
    /// The speed the link runs at now, which is what bounds the modes the
    /// camera lists.
    pub speed: UsbSpeed,
    /// The highest speed the camera declares it supports. `None` when it
    /// declares none, which includes a camera that is plainly USB 2.0 only,
    /// and when it could not be asked.
    pub capable: Option<UsbSpeed>,
}

impl UsbLink {
    /// The faster speed the camera supports, when the link runs slower than
    /// that: the camera can do more than the link lets it report.
    #[must_use]
    pub fn faster_available(self) -> Option<UsbSpeed> {
        self.capable.filter(|capable| *capable > self.speed)
    }
}

/// `bDescriptorType` of a BOS descriptor.
#[cfg(any(test, target_os = "macos"))]
const DT_BOS: u8 = 0x0F;
/// `bDescriptorType` of a device capability inside a BOS.
#[cfg(any(test, target_os = "macos"))]
const DT_DEVICE_CAPABILITY: u8 = 0x10;
/// `bDevCapabilityType` of the SuperSpeed USB capability.
#[cfg(any(test, target_os = "macos"))]
const CAP_SUPERSPEED_USB: u8 = 0x03;
/// `bDevCapabilityType` of the SuperSpeedPlus USB capability.
#[cfg(any(test, target_os = "macos"))]
const CAP_SUPERSPEED_PLUS: u8 = 0x0A;
/// The 5 Gbps bit of a SuperSpeed capability's `wSpeedsSupported`.
#[cfg(any(test, target_os = "macos"))]
const SPEEDS_SUPERSPEED: u16 = 1 << 3;

/// The fastest speed the BOS descriptor `bos` declares, when it declares one
/// beyond high speed.
///
/// A device without a SuperSpeed capability tops out at high speed, which its
/// link already shows, so that is `None` rather than a claim. Malformed or
/// truncated input also yields what was readable before the damage: this
/// reads a device's answer, and a short read must not read as a verdict.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn declared_speed(bos: &[u8]) -> Option<UsbSpeed> {
    let header = bos.get(..5)?;
    if header[1] != DT_BOS {
        return None;
    }
    let total = usize::from(u16::from_le_bytes([header[2], header[3]]));
    let mut rest = bos.get(usize::from(header[0])..total.min(bos.len()))?;

    let mut fastest = None;
    // A descriptor shorter than its own three-byte head ends the walk, so a
    // zero length cannot loop.
    while let Some(capability) = rest
        .first()
        .and_then(|&length| rest.get(..usize::from(length)))
        .filter(|capability| capability.len() >= 3)
    {
        rest = &rest[capability.len()..];
        if capability[1] != DT_DEVICE_CAPABILITY {
            continue;
        }
        let speed = match capability[2] {
            CAP_SUPERSPEED_PLUS => Some(UsbSpeed::SuperPlus),
            CAP_SUPERSPEED_USB => capability
                .get(4..6)
                .map(|speeds| u16::from_le_bytes([speeds[0], speeds[1]]))
                .filter(|speeds| speeds & SPEEDS_SUPERSPEED != 0)
                .map(|_| UsbSpeed::Super),
            _ => None,
        };
        fastest = fastest.max(speed);
    }
    fastest
}

/// Each camera's link as the latest scan found it, by `unique_id`. The one
/// place a platform's findings land, so reading them is the same everywhere.
static LINKS: LazyLock<Mutex<HashMap<String, UsbLink>>> = LazyLock::new(Mutex::default);

/// Held only to read or swap the map, never across I/O, so a UI thread asking
/// for a link does not wait on a scan.
fn links() -> MutexGuard<'static, HashMap<String, UsbLink>> {
    LINKS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Record what a scan found, replacing what earlier scans did: a camera that
/// is no longer attached reports no link.
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
pub(crate) fn remember_links(found: HashMap<String, UsbLink>) {
    *links() = found;
}

/// How the camera with `unique_id` is attached, as the latest
/// [`crate::enumerate_cameras`] found it.
///
/// Answers from that scan and does no USB I/O of its own, so it is cheap
/// enough for a UI thread. `None` when no scan has seen the camera or its
/// link could not be read, and always on Linux, which cannot tell yet.
#[must_use]
pub fn usb_link(unique_id: &str) -> Option<UsbLink> {
    links().get(unique_id).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Logitech StreamCam's BOS, read from the device: a USB 2.0 extension
    /// capability, then SuperSpeed supporting full, high and 5 Gbps speeds.
    const STREAMCAM_BOS: [u8; 22] = [
        0x05, 0x0f, 0x16, 0x00, 0x02, // BOS header: 22 bytes, 2 capabilities
        0x07, 0x10, 0x02, 0x02, 0x00, 0x00, 0x00, // USB 2.0 extension
        0x0a, 0x10, 0x03, 0x00, 0x0e, 0x00, 0x02, 0x0a, 0x00, 0x01, // SuperSpeed
    ];

    /// A BOS with one capability of `body` after the three-byte head.
    fn bos_with(capability_type: u8, body: &[u8]) -> Vec<u8> {
        let capability_len = u8::try_from(3 + body.len()).expect("a short capability");
        let total = 5 + usize::from(capability_len);
        let mut bos = vec![
            0x05,
            0x0f,
            u8::try_from(total).expect("a short BOS"),
            0x00,
            0x01,
        ];
        bos.extend([capability_len, 0x10, capability_type]);
        bos.extend(body);
        bos
    }

    #[test]
    fn a_streamcam_declares_superspeed() {
        assert_eq!(declared_speed(&STREAMCAM_BOS), Some(UsbSpeed::Super));
    }

    #[test]
    fn superspeed_plus_outranks_superspeed_whatever_the_order() {
        let mut bos = STREAMCAM_BOS.to_vec();
        bos.extend([
            0x0c, 0x10, 0x0a, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        bos[2] = u8::try_from(bos.len()).expect("a short BOS");
        bos[4] = 3;
        assert_eq!(declared_speed(&bos), Some(UsbSpeed::SuperPlus));

        let plus_first = bos_with(
            0x0a,
            &[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        );
        assert_eq!(declared_speed(&plus_first), Some(UsbSpeed::SuperPlus));
    }

    #[test]
    fn a_usb2_only_device_declares_nothing_beyond_high_speed() {
        let usb2_extension = &STREAMCAM_BOS[..12];
        let mut bos = usb2_extension.to_vec();
        bos[2] = 12;
        bos[4] = 1;
        assert_eq!(declared_speed(&bos), None);
    }

    #[test]
    fn a_superspeed_capability_without_the_5_gbps_bit_declares_nothing() {
        // Full and high speed only (bits 1 and 2).
        let bos = bos_with(0x03, &[0x00, 0x06, 0x00, 0x01, 0x0a, 0x00, 0x00]);
        assert_eq!(declared_speed(&bos), None);
    }

    #[test]
    fn damaged_input_is_read_as_far_as_it_goes_and_never_loops() {
        assert_eq!(declared_speed(&[]), None);
        assert_eq!(
            declared_speed(&[0x05, 0x0f, 0x05]),
            None,
            "shorter than a header"
        );
        assert_eq!(
            declared_speed(&[0x05, 0x02, 0x16, 0x00, 0x02, 0x0a, 0x10, 0x03]),
            None,
            "not a BOS descriptor"
        );

        // A header that promises more than was read: the readable part counts.
        assert_eq!(
            declared_speed(&STREAMCAM_BOS[..STREAMCAM_BOS.len() - 4]),
            None
        );
        let mut longer_claim = STREAMCAM_BOS;
        longer_claim[2] = 0xff;
        assert_eq!(declared_speed(&longer_claim), Some(UsbSpeed::Super));

        // A zero-length descriptor ends the walk instead of repeating it.
        let mut zero_length = STREAMCAM_BOS;
        zero_length[5] = 0x00;
        assert_eq!(declared_speed(&zero_length), None);

        // A header longer than the buffer, and a total shorter than the header.
        assert_eq!(declared_speed(&[0x20, 0x0f, 0x16, 0x00, 0x02]), None);
        assert_eq!(declared_speed(&[0x05, 0x0f, 0x03, 0x00, 0x02]), None);
    }

    #[test]
    fn a_link_rate_is_classed_by_the_speed_it_reaches() {
        let class = UsbSpeed::from_bits_per_second;
        assert_eq!(class(1_500_000), Some(UsbSpeed::Low));
        assert_eq!(class(12_000_000), Some(UsbSpeed::Full));
        assert_eq!(class(480_000_000), Some(UsbSpeed::High));
        assert_eq!(class(5_000_000_000), Some(UsbSpeed::Super));
        assert_eq!(class(10_000_000_000), Some(UsbSpeed::SuperPlus));
        assert_eq!(class(20_000_000_000), Some(UsbSpeed::SuperPlus));
        assert_eq!(class(0), None);
        assert_eq!(class(1_499_999), None);
    }

    #[test]
    fn a_camera_is_limited_only_when_it_supports_more_than_the_link_gives() {
        let link = |speed, capable| UsbLink { speed, capable };
        assert_eq!(
            link(UsbSpeed::High, Some(UsbSpeed::Super)).faster_available(),
            Some(UsbSpeed::Super)
        );
        assert_eq!(
            link(UsbSpeed::Super, Some(UsbSpeed::Super)).faster_available(),
            None,
            "already at the best it supports"
        );
        assert_eq!(
            link(UsbSpeed::SuperPlus, Some(UsbSpeed::Super)).faster_available(),
            None,
            "a faster link than it declared is no limit"
        );
        assert_eq!(
            link(UsbSpeed::High, None).faster_available(),
            None,
            "nothing declared, nothing to claim"
        );
    }

    #[test]
    fn a_scan_replaces_what_the_last_one_found() {
        let link = UsbLink {
            speed: UsbSpeed::High,
            capable: Some(UsbSpeed::Super),
        };
        remember_links(HashMap::from([("store-a".to_owned(), link)]));
        assert_eq!(usb_link("store-a"), Some(link));
        assert_eq!(usb_link("store-unseen"), None);

        remember_links(HashMap::new());
        assert_eq!(usb_link("store-a"), None, "a camera that left reports none");
    }

    #[test]
    fn speeds_read_the_way_users_name_them() {
        assert_eq!(UsbSpeed::High.to_string(), "USB 2.0 (480 Mbps)");
        assert_eq!(UsbSpeed::Super.to_string(), "USB 3.0 (5 Gbps)");
    }
}
