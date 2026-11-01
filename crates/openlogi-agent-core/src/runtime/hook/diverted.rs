//! The buttons HID++ capture sessions have taken over on directly connected
//! devices, so the OS hook can tell an echo of a diverted press from a press
//! only the hook sees (#2158).
//!
//! Diverting a `0x1b04` control moves its presses out of the native HID report
//! and into HID++ notifications, so the OS normally never sees them. Linux
//! 7.3's `hid-logitech-hidpp` reads those notifications as well, and for a
//! Bluetooth Signature M650 it reports a diverted Back/Forward as evdev
//! buttons. The hook then receives a press the capture session has already
//! dispatched.

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

/// The hook-reachable buttons one capture session diverted on one direct device.
#[derive(Debug)]
struct Diversion {
    device: DirectDevice,
    buttons: BTreeSet<ButtonId>,
}

/// The hook-reachable buttons each live capture session has diverted, keyed by its
/// capture epoch. A session reports what arming wrote
/// ([`CapturedInput::ButtonDiverted`](openlogi_hid::CapturedInput::ButtonDiverted)),
/// and its entry is released when its task ends.
#[derive(Debug, Default)]
pub(crate) struct DivertedButtons(HashMap<u64, Diversion>);

impl DivertedButtons {
    /// Record that `session` diverted `button` on `route`. Only a button the
    /// hook acts on ([`ButtonId::may_reach_os_hook`]), on a direct route, can
    /// reach the hook as an echo, so any other diversion is not recorded.
    pub(crate) fn record(
        &mut self,
        session: &HidppSessionId,
        route: &DeviceRoute,
        button: ButtonId,
    ) {
        let Some(device) = DirectDevice::of(route) else {
            return;
        };
        if !button.may_reach_os_hook() {
            return;
        }
        self.0
            .entry(session.epoch())
            .or_insert_with(|| Diversion {
                device,
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
            diversion.buttons.contains(&button) && diversion.device.is_source_of(source)
        })
    }
}
