//! The USB link each Logitech camera runs at, as enumeration sees it.
//!
//! Enumeration is the one place this crate already scans USB off the UI
//! thread, so it records every camera's link speed and what the camera says it
//! supports ([`observe_links`]); the UI asks later ([`crate::usb_link()`]) and
//! gets an answer from memory. The link speed is read from the registry on
//! every scan, so moving a camera to a faster port shows up. What a camera
//! supports does not change, so it is read from the device once and kept.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use objc2_core_foundation::{CFNumber, CFString};

use super::iokit::{self, BosRead, UsbInterface};
use super::{location_hint, service_location};
use crate::Camera;
use crate::usb_link::{UsbLink, UsbSpeed, declared_speed};

/// A camera enumeration found, with the speed its link runs at.
struct Attached {
    unique_id: String,
    location: u32,
    speed: UsbSpeed,
}

/// What scans have learned about what cameras support.
#[derive(Default)]
struct Observed {
    /// What each camera declared it supports, by `unique_id`, for every camera
    /// that has been asked. `None` is an answer — the camera declares nothing
    /// beyond high speed — so it is not asked again; a camera that could not
    /// be asked has no entry and is.
    declared: HashMap<String, Option<UsbSpeed>>,
}

impl Observed {
    /// The cameras in `attached` that have not been asked what they support.
    fn unasked<'a>(&self, attached: &'a [Attached]) -> Vec<&'a Attached> {
        attached
            .iter()
            .filter(|camera| !self.declared.contains_key(&camera.unique_id))
            .collect()
    }

    /// Take one scan's findings: remember what was `learned` about cameras'
    /// declared speeds, and return the links of `attached` as they stand now.
    /// Only the cameras in `attached` have one, so a camera that has gone is no
    /// longer reported.
    fn links(
        &mut self,
        attached: Vec<Attached>,
        learned: impl IntoIterator<Item = (String, Option<UsbSpeed>)>,
    ) -> HashMap<String, UsbLink> {
        self.declared.extend(learned);
        attached
            .into_iter()
            .map(|camera| {
                let capable = self.declared.get(&camera.unique_id).copied().flatten();
                let link = UsbLink {
                    speed: camera.speed,
                    capable,
                };
                (camera.unique_id, link)
            })
            .collect()
    }
}

static OBSERVED: LazyLock<Mutex<Observed>> = LazyLock::new(Mutex::default);

/// Held only to read or extend the map, never across USB I/O, so a scan does
/// not hold up whoever asks for a link.
fn observed() -> MutexGuard<'static, Observed> {
    OBSERVED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Record the link of every Logitech camera in `cameras`.
///
/// Enumeration calls this with [`crate::USB_QUIESCE`] held, so reading a
/// device here cannot land inside a control seize. A camera whose link cannot
/// be read is left out, and so reports no link rather than a wrong one.
pub(crate) fn observe_links(cameras: &[Camera]) {
    let speeds = link_speeds_by_location();
    let attached: Vec<Attached> = cameras
        .iter()
        .filter(|camera| camera.vendor_id == crate::LOGITECH_VID)
        .filter_map(|camera| {
            let location = location_hint(&camera.unique_id)?;
            Some(Attached {
                unique_id: camera.unique_id.clone(),
                location,
                speed: *speeds.get(&location)?,
            })
        })
        .collect();

    // Two statements, so the lock is gone before any device is read.
    let unasked = observed().unasked(&attached);
    let learned: Vec<_> = unasked
        .into_iter()
        .filter_map(|camera| {
            let declared = declared_speed_at(camera.location).ok()?;
            Some((camera.unique_id.clone(), declared))
        })
        .collect();
    let links = observed().links(attached, learned);
    crate::usb_link::remember_links(links);
}

/// The speed every attached USB device's link runs at, by location id.
///
/// Registry only — no device open. The registry's `UsbLinkSpeed` is the
/// negotiated rate in bits per second.
fn link_speeds_by_location() -> HashMap<u32, UsbSpeed> {
    let location_key = CFString::from_static_str("locationID");
    let speed_key = CFString::from_static_str("UsbLinkSpeed");
    iokit::usb_devices()
        .into_iter()
        .flatten()
        .filter_map(|service| {
            let location = service_location(&service, &location_key)?;
            let bits = iokit::registry_property(&service, &speed_key)?
                .downcast::<CFNumber>()
                .ok()?
                .as_i64()?;
            let speed = UsbSpeed::from_bits_per_second(u64::try_from(bits).ok()?)?;
            Some((location, speed))
        })
        .collect()
}

/// A device that could not be asked what it supports, so a later attempt may
/// still get an answer.
struct Unreachable;

/// What the Logitech device at `location` says it supports.
///
/// `Ok(None)` means it answered and declares nothing beyond high speed, which
/// is an answer: only [`Unreachable`] is worth asking again.
fn declared_speed_at(location: u32) -> Result<Option<UsbSpeed>, Unreachable> {
    for service in iokit::usb_devices().map_err(|_| Unreachable)? {
        let Some(interface) = UsbInterface::open(&service) else {
            continue;
        };
        if interface.location_id() != Some(location)
            || interface.vendor_id() != Some(crate::LOGITECH_VID)
        {
            continue;
        }
        return match interface.bos_descriptor() {
            BosRead::Descriptor(bos) => Ok(declared_speed(&bos)),
            BosRead::Absent => Ok(None),
            BosRead::Unreachable => Err(Unreachable),
        };
    }
    Err(Unreachable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attached(unique_id: &str, speed: UsbSpeed) -> Attached {
        Attached {
            unique_id: unique_id.to_owned(),
            location: 0x0117_1000,
            speed,
        }
    }

    fn learned(unique_id: &str, declared: Option<UsbSpeed>) -> (String, Option<UsbSpeed>) {
        (unique_id.to_owned(), declared)
    }

    #[test]
    fn a_camera_is_asked_what_it_supports_until_it_has_answered() {
        let mut observed = Observed::default();
        let cameras = [attached("a", UsbSpeed::High)];
        assert_eq!(observed.unasked(&cameras).len(), 1);

        // It could not be asked: no answer is recorded, so it is asked again.
        observed.links(vec![attached("a", UsbSpeed::High)], []);
        assert_eq!(observed.unasked(&cameras).len(), 1);

        observed.links(
            vec![attached("a", UsbSpeed::High)],
            [learned("a", Some(UsbSpeed::Super))],
        );
        assert!(observed.unasked(&cameras).is_empty());
    }

    #[test]
    fn declaring_nothing_is_an_answer_and_is_not_asked_again() {
        let mut observed = Observed::default();
        let links = observed.links(
            vec![attached("c920", UsbSpeed::High)],
            [learned("c920", None)],
        );

        assert!(
            observed
                .unasked(&[attached("c920", UsbSpeed::High)])
                .is_empty()
        );
        assert_eq!(
            links["c920"],
            UsbLink {
                speed: UsbSpeed::High,
                capable: None
            }
        );
    }

    #[test]
    fn the_speed_follows_each_scan_while_what_the_camera_supports_is_kept() {
        let mut observed = Observed::default();
        let links = observed.links(
            vec![attached("a", UsbSpeed::High)],
            [learned("a", Some(UsbSpeed::Super))],
        );
        assert_eq!(
            links["a"].faster_available(),
            Some(UsbSpeed::Super),
            "on a USB 2.0 port the camera could do more"
        );

        // Moved to a faster port; nothing is asked again.
        let links = observed.links(vec![attached("a", UsbSpeed::Super)], []);
        assert_eq!(links["a"].speed, UsbSpeed::Super);
        assert_eq!(links["a"].capable, Some(UsbSpeed::Super));
        assert_eq!(links["a"].faster_available(), None);
    }

    #[test]
    fn a_camera_that_left_the_scan_reports_no_link() {
        let mut observed = Observed::default();
        observed.links(
            vec![
                attached("a", UsbSpeed::High),
                attached("b", UsbSpeed::Super),
            ],
            [learned("a", Some(UsbSpeed::Super))],
        );
        let links = observed.links(vec![attached("b", UsbSpeed::Super)], []);

        assert!(!links.contains_key("a"));
        assert!(links.contains_key("b"));
    }
}
