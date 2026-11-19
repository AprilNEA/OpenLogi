//! The buttons HID++ capture sessions have taken over, so the OS hook can tell
//! who a press of one of them came from.
//!
//! Diverting a `0x1b04` control moves its presses out of the native HID report
//! and into HID++ notifications, so the OS normally never sees them. That
//! answers two questions about a press the hook does see:
//!
//! - From the diverted device itself, it is an echo of a press the session has
//!   already dispatched. Linux 7.3's `hid-logitech-hidpp` reads the HID++
//!   notifications as well, and for a Bluetooth Signature M650 it reports a
//!   diverted Back/Forward as evdev buttons (#2158).
//! - With no source attached — Windows' low-level hook never names one — it
//!   cannot be the selected device's press when that device has diverted the
//!   control, so another mouse sent it (#2381).

use std::collections::{BTreeSet, HashMap};

use openlogi_core::binding::ButtonId;
use openlogi_hid::DeviceRoute;
use openlogi_hook::EventDevice;

use crate::runtime::HidppSessionId;

/// The vendor and product ids a directly connected device reports both on its
/// HID++ route and as an OS input source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectDevice {
    vendor_id: u16,
    product_id: u16,
}

impl DirectDevice {
    /// The OS input identity of `route`. Only a direct route names one: a
    /// receiver route names the receiver and a slot, not the ids the OS
    /// reports for the paired device.
    fn of(route: &DeviceRoute) -> Option<Self> {
        match *route {
            DeviceRoute::Direct {
                vendor_id,
                product_id,
            } => Some(Self {
                vendor_id,
                product_id,
            }),
            _ => None,
        }
    }

    /// Whether `source` is this device's OS input source. Two connected units
    /// of one model report the same ids, so they cannot be told apart here.
    fn is_source_of(self, source: &EventDevice) -> bool {
        source.vendor_id == Some(u32::from(self.vendor_id))
            && source.product_id == Some(u32::from(self.product_id))
    }
}

/// The OS-hook buttons one capture session diverted on the device at `route`.
#[derive(Debug)]
struct Diversion {
    route: DeviceRoute,
    buttons: BTreeSet<ButtonId>,
}

/// The OS-hook buttons each live capture session has diverted, keyed by its
/// capture epoch. A session reports what arming wrote
/// ([`CapturedInput::ButtonDiverted`](openlogi_hid::CapturedInput::ButtonDiverted)),
/// and its entry is released when its task ends.
#[derive(Debug, Default)]
pub(crate) struct DivertedButtons(HashMap<u64, Diversion>);

impl DivertedButtons {
    /// Record that `session` diverted `button` on `route`. Only an OS-hook
    /// button can reach the hook at all, so any other diversion is not
    /// recorded.
    pub(crate) fn record(
        &mut self,
        session: &HidppSessionId,
        route: &DeviceRoute,
        button: ButtonId,
    ) {
        if !button.is_os_hook_button() {
            return;
        }
        self.0
            .entry(session.epoch())
            .or_insert_with(|| Diversion {
                route: route.clone(),
                buttons: BTreeSet::new(),
            })
            .buttons
            .insert(button);
    }

    /// Forget `session`'s diversions once its task has ended. Any capture
    /// epoch, current or stale, releases only its own entry.
    pub(crate) fn release(&mut self, session: &HidppSessionId) {
        self.0.remove(&session.epoch());
    }

    /// Whether a live capture session has diverted `button` on `source`, so
    /// that an OS edge for it is an echo of a press the session reports.
    pub(crate) fn owns(&self, source: &EventDevice, button: ButtonId) -> bool {
        self.0.values().any(|diversion| {
            diversion.buttons.contains(&button)
                && DirectDevice::of(&diversion.route)
                    .is_some_and(|device| device.is_source_of(source))
        })
    }

    /// Whether a live capture session has diverted `button` on the device at
    /// `route`, so none of that device's presses of it reach the OS.
    pub(crate) fn diverted_on(&self, route: &DeviceRoute, button: ButtonId) -> bool {
        self.0
            .values()
            .any(|diversion| diversion.route == *route && diversion.buttons.contains(&button))
    }
}
