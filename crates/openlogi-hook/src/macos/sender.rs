//! Which device produced a `CGEvent`: the IOHID sender-id lookup behind the
//! event, and the IOKit registry walk that turns that id into device facts.
//!
//! Both rest on symbols no crate binds, so their `extern` declarations live
//! here, next to their only callers.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use core_foundation::base::{CFTypeRef, TCFType as _};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::CGEvent;
use foreign_types_shared::ForeignType as _;

use tracing::{debug, warn};

use crate::EventDevice;

/// Opaque `IOHIDEventRef` — the HID event backing a `CGEvent`.
type IOHIDEventRef = *mut std::ffi::c_void;

// Device-of-origin lookup. `CGEventCopyIOHIDEvent` (CoreGraphics) returns the
// HID event behind a CGEvent; `IOHIDEventGetSenderID` (IOKit) yields the
// registry id of the producing service. These are undocumented but long-stable
// symbols (Mac Mouse Fix / Karabiner use them) — the only reliable way to tell a
// hi-res mouse wheel from a trackpad, which carry identical CGEvent phase flags.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCopyIOHIDEvent(event: *const std::ffi::c_void) -> IOHIDEventRef;
}
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventGetSenderID(event: IOHIDEventRef) -> u64;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}

/// The registry id of the device that produced `event`, via its backing
/// IOHIDEvent. `None` for events with no HID backing (e.g. synthetic ones).
pub(super) fn event_sender_id(event: &CGEvent) -> Option<u64> {
    // SAFETY: `event.as_ptr()` is the live CGEventRef; `CGEventCopyIOHIDEvent`
    // returns a +1-retained IOHIDEvent (or null) which we release below.
    let hid = unsafe { CGEventCopyIOHIDEvent(event.as_ptr().cast()) };
    if hid.is_null() {
        return None;
    }
    // SAFETY: `hid` is a live IOHIDEvent for the duration of the call.
    let sender = unsafe { IOHIDEventGetSenderID(hid) };
    // SAFETY: balance the +1 retain from `CGEventCopyIOHIDEvent`.
    unsafe { CFRelease(hid) };
    Some(sender)
}

/// IOKit registry walk to read a device's HID usage page. `IORegistryEntryIDMatching`
/// builds a matching dict for the service id; `IOServiceGetMatchingService` resolves
/// it (and releases the dict); `IORegistryEntrySearchCFProperty` reads a property,
/// searching parents so the usage page on the owning `IOHIDDevice` is found.
type IoObjectT = u32;
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegistryEntryIDMatching(entry_id: u64) -> *mut std::ffi::c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *const std::ffi::c_void) -> IoObjectT;
    fn IORegistryEntrySearchCFProperty(
        entry: IoObjectT,
        plane: *const std::ffi::c_char,
        key: CFStringRef,
        allocator: CFTypeRef,
        options: u32,
    ) -> CFTypeRef;
    fn IOObjectRelease(object: IoObjectT) -> i32;
}

const IO_REGISTRY_ITERATE_RECURSIVELY: u32 = 1;
const IO_REGISTRY_ITERATE_PARENTS: u32 = 2;

/// Resolve `sender_id` to its IO service, or `None`. Caller must
/// `IOObjectRelease` the result.
fn open_service(sender_id: u64) -> Option<IoObjectT> {
    // SAFETY: returns a +1 matching dict; `IOServiceGetMatchingService` consumes it.
    let matching = unsafe { IORegistryEntryIDMatching(sender_id) };
    if matching.is_null() {
        return None;
    }
    // SAFETY: `matching` is a valid +1 dict (consumed here); 0 = default main port.
    let service = unsafe { IOServiceGetMatchingService(0, matching) };
    (service != 0).then_some(service)
}

/// Read property `key` off `service` (searching parents), as a `+1` CFTypeRef the
/// caller owns. `None` if absent.
fn service_property(service: IoObjectT, key: &str) -> Option<CFTypeRef> {
    let cf_key = CFString::new(key);
    let plane = c"IOService";
    // SAFETY: `service` is live; `cf_key` is a valid CFStringRef; null allocator is
    // the documented default; returns a +1 CF value (or null).
    let prop = unsafe {
        IORegistryEntrySearchCFProperty(
            service,
            plane.as_ptr(),
            cf_key.as_concrete_TypeRef(),
            std::ptr::null(),
            IO_REGISTRY_ITERATE_RECURSIVELY | IO_REGISTRY_ITERATE_PARENTS,
        )
    };
    (!prop.is_null()).then_some(prop)
}

/// Cached device facts derived from the IOKit sender id behind a scroll event.
#[derive(Clone, Default)]
pub(super) struct SenderDeviceInfo {
    pub(super) event_device: EventDevice,
    pub(super) is_trackpad: bool,
}

/// Device facts for the registry id `sender_id`, read from the IOKit registry.
/// A trackpad presents a *mouse* HID interface for scrolling, so usage can't
/// separate it from a real wheel; product identity stays stable across wheel
/// modes, unlike CGEvent phase.
fn read_sender_device_info(sender_id: u64) -> SenderDeviceInfo {
    let Some(service) = open_service(sender_id) else {
        return SenderDeviceInfo::default();
    };
    let string_prop = |k| {
        // SAFETY: a String property is a +1 CFString; wrap takes ownership.
        service_property(service, k)
            .map(|p| unsafe { CFString::wrap_under_create_rule(p.cast()) }.to_string())
    };
    let num_prop = |k| {
        service_property(service, k)
            .and_then(|p| {
                // SAFETY: `service_property` only yields a non-null, +1 CF
                // value, and every key passed below is published by IOKit as
                // a CFNumber, so the cast keeps the type; the create rule
                // hands that retain to the wrapper, which releases it on drop.
                unsafe { CFNumber::wrap_under_create_rule(p.cast()) }.to_i64()
            })
            .and_then(|n| u32::try_from(n).ok())
    };
    let product_name = string_prop("Product");
    let info = SenderDeviceInfo {
        is_trackpad: product_name
            .as_deref()
            .is_some_and(|p| p.to_lowercase().contains("trackpad")),
        event_device: EventDevice {
            vendor_id: num_prop("VendorID").or_else(|| num_prop("idVendor")),
            product_id: num_prop("ProductID").or_else(|| num_prop("idProduct")),
            product_name,
        },
    };
    // SAFETY: `service` is a live io_object_t we own.
    unsafe { IOObjectRelease(service) };
    info
}

/// How long the tap callback waits for a sender it has not seen before.
/// Long enough for the registry walk on a settled device, far inside the
/// callback watchdog's budget.
const NEW_SENDER_WAIT: Duration = Duration::from_millis(25);

/// A registry walk slower than this is logged: it is what the tap would have
/// stalled for had it walked the registry itself.
const SLOW_LOOKUP: Duration = Duration::from_millis(50);

/// Device facts for the registry id `sender_id`, or `None` while its registry
/// walk has not answered.
///
/// Called from the tap callback, which must never block, so the walk runs on
/// a resolver thread. A device that reconnects comes back under a new sender
/// id, and IOKit is still publishing it then: the walk has taken past the
/// callback watchdog's budget there, and the watchdog exited the agent. The
/// callback waits [`NEW_SENDER_WAIT`] for a new sender, and while the walk is
/// still out treats its events as unattributed. Cached per id, because
/// identity never changes under one id.
pub(super) fn sender_device_info(sender_id: u64) -> Option<SenderDeviceInfo> {
    thread_local! {
        static SENDERS: RefCell<Option<Senders>> = const { RefCell::new(None) };
    }
    SENDERS.with_borrow_mut(|senders| {
        senders
            .get_or_insert_with(|| Senders::spawn(read_sender_device_info))
            .get(sender_id, NEW_SENDER_WAIT)
    })
}

type Lookup = fn(u64) -> SenderDeviceInfo;

/// What the tap thread knows of each sender, and the resolver thread that
/// walks the registry for the ones it does not.
struct Senders {
    known: HashMap<u64, SenderDeviceInfo>,
    pending: HashSet<u64>,
    /// `None` when the resolver could not be started: the walk then runs on
    /// the calling thread, as it did before there was one.
    resolver: Option<Resolver>,
    lookup: Lookup,
}

struct Resolver {
    requests: mpsc::Sender<u64>,
    results: mpsc::Receiver<(u64, SenderDeviceInfo)>,
}

impl Senders {
    fn spawn(lookup: Lookup) -> Self {
        let (requests, request_rx) = mpsc::channel::<u64>();
        let (result_tx, results) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("openlogi-hook-senders".into())
            .spawn(move || {
                for sender in request_rx {
                    let started = Instant::now();
                    let info = lookup(sender);
                    let elapsed = started.elapsed();
                    if elapsed >= SLOW_LOOKUP {
                        debug!(sender, ?elapsed, "slow IOKit sender lookup");
                    }
                    if result_tx.send((sender, info)).is_err() {
                        return;
                    }
                }
            });
        let resolver = match spawned {
            Ok(_) => Some(Resolver { requests, results }),
            Err(error) => {
                warn!(%error, "sender resolver thread failed to start — resolving on the tap");
                None
            }
        };
        Self {
            known: HashMap::new(),
            pending: HashSet::new(),
            resolver,
            lookup,
        }
    }

    fn get(&mut self, sender: u64, wait: Duration) -> Option<SenderDeviceInfo> {
        let Some(resolver) = &self.resolver else {
            let lookup = self.lookup;
            return Some(
                self.known
                    .entry(sender)
                    .or_insert_with(|| lookup(sender))
                    .clone(),
            );
        };
        while let Ok((id, info)) = resolver.results.try_recv() {
            self.pending.remove(&id);
            self.known.insert(id, info);
        }
        if let Some(info) = self.known.get(&sender) {
            return Some(info.clone());
        }
        if !self.pending.insert(sender) {
            // Already asked: an earlier event waited for it once.
            return None;
        }
        if resolver.requests.send(sender).is_err() {
            self.pending.remove(&sender);
            self.resolver = None;
            return self.get(sender, wait);
        }
        let deadline = Instant::now() + wait;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok((id, info)) = resolver.results.recv_timeout(left) else {
                break;
            };
            self.pending.remove(&id);
            self.known.insert(id, info);
            if id == sender {
                break;
            }
        }
        self.known.get(&sender).cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Senders `>= SLOW` answer only once the test opens the gate.
    const SLOW: u64 = 100;
    static GATE: Mutex<()> = Mutex::new(());

    fn lookup(sender: u64) -> SenderDeviceInfo {
        if sender >= SLOW {
            drop(GATE.lock());
        }
        SenderDeviceInfo {
            event_device: EventDevice {
                vendor_id: Some(0x046d),
                product_id: u32::try_from(sender).ok(),
                product_name: None,
            },
            is_trackpad: false,
        }
    }

    #[test]
    fn a_new_sender_is_resolved_within_the_wait_and_then_cached() {
        let mut senders = Senders::spawn(lookup);
        let info = senders.get(1, Duration::from_secs(5)).expect("resolved");
        assert_eq!(info.event_device.product_id, Some(1));
        assert!(senders.known.contains_key(&1));
        assert!(senders.pending.is_empty());
    }

    /// A walk that outlasts the wait leaves the event unattributed, and later
    /// events from that sender do not wait again: the tap stalls once, for a
    /// bounded time, however long IOKit takes.
    #[test]
    fn a_slow_walk_never_holds_the_tap_past_one_bounded_wait() {
        let gate = GATE.lock().expect("gate");
        let mut senders = Senders::spawn(lookup);
        assert!(senders.get(SLOW, Duration::from_millis(20)).is_none());

        let started = Instant::now();
        assert!(senders.get(SLOW, Duration::from_secs(5)).is_none());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a pending sender is not waited for again"
        );

        drop(gate);
        let deadline = Instant::now() + Duration::from_secs(5);
        let info = loop {
            if let Some(info) = senders.get(SLOW, Duration::ZERO) {
                break info;
            }
            assert!(Instant::now() < deadline, "the walk's answer is picked up");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(
            info.event_device.product_id,
            Some(u32::try_from(SLOW).expect("u32"))
        );
    }
}
