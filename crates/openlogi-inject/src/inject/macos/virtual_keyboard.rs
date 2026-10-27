//! The virtual HID keyboard OpenLogi's key synthesis types through.
//!
//! A `CGEventPost`ed key enters the window server as software input. A key
//! reported by an `IOHIDUserDevice` enters the HID event system where a USB
//! keyboard's does, so the system processes it as it processes hardware
//! input, from the flags the key event carries to the keyboard settings
//! that apply to it.
//! Creating the device needs the restricted
//! `com.apple.developer.hid.virtual.device` entitlement, granted only
//! together with a provisioning profile (Logi Options+ ships one for its
//! agent). Without it, creation fails and every key keeps the `CGEvent`
//! path.
//!
//! The device exists while a [`VirtualKeyboard`] lives. While it does, a key
//! edge goes through it when the key has a HID usage and the six-key
//! rollover has room. Otherwise, and for an up edge whose down edge the
//! device did not carry, the caller posts the `CGEvent` as before, so a key
//! is always released on the path that pressed it. A report the device
//! rejects removes the device for the rest of the process. The removal
//! reports every key released, so the keys held chords still own go down
//! again as `CGEvent`s, where their release will go.
//!
//! The device holds a key down only while a held chord owns it: a tap
//! reports the key's release before it returns, and removal reports every
//! key released first.

#[expect(
    unsafe_code,
    reason = "IOHIDUserDevice creation and report delivery are unsafe IOKit calls"
)]
mod device;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use core_graphics::event::CGEventFlags;

use super::KeyPhase;
use super::keycode::{hid_usage_to_macos, macos_to_hid_usage};
use device::Device;

/// The device's `Product` property. The hook reads it to recognise the
/// device's keys as OpenLogi's own output.
const PRODUCT: &str = "OpenLogi Virtual Keyboard";

/// A boot-protocol keyboard input report without a report ID: the modifier
/// bitmap, a reserved byte, then [`KEY_SLOTS`] key slots. It declares no
/// LED output report, because the device registers no handler to receive
/// one.
const REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xa1, 0x01, // Collection (Application)
    0x05, 0x07, //   Usage Page (Keyboard/Keypad)
    0x19, 0xe0, //   Usage Minimum (Left Control)
    0x29, 0xe7, //   Usage Maximum (Right GUI)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x08, //   Report Count (8)
    0x81, 0x02, //   Input (Data, Variable, Absolute): the modifier bitmap
    0x95, 0x01, //   Report Count (1)
    0x75, 0x08, //   Report Size (8)
    0x81, 0x01, //   Input (Constant): the reserved byte
    0x95, 0x06, //   Report Count (6)
    0x75, 0x08, //   Report Size (8)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, //   Logical Maximum (255)
    0x05, 0x07, //   Usage Page (Keyboard/Keypad)
    0x19, 0x00, //   Usage Minimum (0)
    0x2a, 0xff, 0x00, //   Usage Maximum (255)
    0x81, 0x00, //   Input (Data, Array, Absolute): the key slots
    0xc0, // End Collection
];

/// Keys a report can carry at once besides the modifiers.
const KEY_SLOTS: usize = 6;
/// Bytes in one input report.
const REPORT_LEN: usize = 2 + KEY_SLOTS;

/// The usages of the modifier bitmap's bits, Left Control through Right GUI.
const MODIFIER_USAGES: std::ops::RangeInclusive<u8> = 0xe0..=0xe7;

/// Modifier bitmap bits, as the usages `0xe0..=0xe3` number them.
const LEFT_CONTROL: u8 = 1 << 0;
const LEFT_SHIFT: u8 = 1 << 1;
const LEFT_OPTION: u8 = 1 << 2;
const LEFT_COMMAND: u8 = 1 << 3;

/// Each `CGEvent` modifier flag and the modifier bitmap bit of its left key.
const MODIFIER_FLAGS: [(CGEventFlags, u8); 4] = [
    (CGEventFlags::CGEventFlagControl, LEFT_CONTROL),
    (CGEventFlags::CGEventFlagShift, LEFT_SHIFT),
    (CGEventFlags::CGEventFlagAlternate, LEFT_OPTION),
    (CGEventFlags::CGEventFlagCommand, LEFT_COMMAND),
];

/// Whether a keyboard whose HID `Product` property is `product` is
/// OpenLogi's virtual keyboard. Its keys are OpenLogi's own output, which
/// the hook must not remap again, as with a `CGEvent` stamped with
/// [`SYNTHETIC_EVENT_USER_DATA`](crate::SYNTHETIC_EVENT_USER_DATA).
#[must_use]
pub fn is_virtual_keyboard_product(product: &str) -> bool {
    product == PRODUCT
}

/// Routes OpenLogi's key synthesis through a virtual HID keyboard for as
/// long as it lives.
///
/// The device is process-wide. Dropping the value reports every key it holds
/// released and then removes the device; key synthesis after that posts
/// `CGEvent`s again. Keep it alive until every worker that can press a key
/// has stopped.
#[derive(Debug)]
#[must_use = "dropping the value removes the virtual keyboard again"]
pub struct VirtualKeyboard {
    /// The installation this value removes on drop. A device that failed
    /// and was replaced by a later installation is not this value's to
    /// remove.
    id: u64,
}

impl VirtualKeyboard {
    /// Create the device and route key synthesis through it.
    ///
    /// Returns `None` when the process may not create a virtual HID device
    /// (it lacks the entitlement) or when a [`VirtualKeyboard`] is already
    /// installed. Key synthesis then keeps posting `CGEvent`s.
    pub fn install() -> Option<Self> {
        let mut slot = lock_slot();
        if slot.is_some() {
            tracing::warn!("a virtual HID keyboard is already installed");
            return None;
        }
        let Some(device) = Device::create(PRODUCT, REPORT_DESCRIPTOR) else {
            tracing::info!(
                "virtual HID keyboard unavailable (no com.apple.developer.hid.virtual.device \
                 entitlement?); synthesized keys use CGEventPost"
            );
            return None;
        };
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        *slot = Some(Installed {
            id,
            keyboard: Keyboard::new(device),
        });
        tracing::info!("virtual HID keyboard installed; synthesized keys use it");
        Some(Self { id })
    }

    /// Whether this installation's device still carries key synthesis. It
    /// stops for good once the device rejects a report and is removed.
    #[must_use]
    pub fn is_active(&self) -> bool {
        holds(lock_slot().as_ref(), self.id)
    }
}

impl Drop for VirtualKeyboard {
    fn drop(&mut self) {
        let mut slot = lock_slot();
        if holds(slot.as_ref(), self.id) {
            // Dropping the keyboard under the lock releases its keys before
            // any other edge can see the slot empty.
            drop(slot.take());
            tracing::info!("virtual HID keyboard removed");
        }
    }
}

/// Whether `slot` holds the installation `id`. A device that failed and was
/// replaced by a later installation is not that installation's.
fn holds<S: Sink>(slot: Option<&Installed<S>>, id: u64) -> bool {
    slot.is_some_and(|installed| installed.id == id)
}

/// Offer one edge of the key with virtual keycode `vk` to the installed
/// device. `true` when the device carried it; on `false` the caller posts
/// the edge as a `CGEvent`.
pub(super) fn edge(vk: u16, phase: KeyPhase) -> bool {
    let Some(usage) = macos_to_hid_usage(vk) else {
        return false;
    };
    let mut slot = lock_slot();
    let offered = offer(&mut slot, |keyboard| keyboard.edge(usage, phase));
    settle(offered, slot)
}

/// Offer a press and release of `vk` with `flags`' modifiers to the
/// installed device. `true` when the device carried it; on `false` the
/// caller posts the pair as `CGEvent`s.
pub(super) fn tap(vk: u16, flags: CGEventFlags) -> bool {
    let Some(usage) = macos_to_hid_usage(vk) else {
        return false;
    };
    let modifiers = modifier_bits(flags);
    let mut slot = lock_slot();
    let offered = offer(&mut slot, |keyboard| keyboard.tap(usage, modifiers));
    settle(offered, slot)
}

/// Press the keys `offered` stranded again as `CGEvent`s, then release the
/// slot. Holding the slot until then keeps every other edge, including the
/// release of a stranded key, behind the press it must follow.
fn settle(offered: Offer, slot: MutexGuard<'_, Option<Installed<Device>>>) -> bool {
    for (vk, flags) in repress_edges(offered.stranded) {
        super::post_cg_key_phase(vk, flags, KeyPhase::Down);
    }
    drop(slot);
    offered.carried
}

/// The `CGEvent` down edges that press `stranded` again, as a keyboard
/// would: each modifier with the flags of those down so far, then each key
/// with all of them.
fn repress_edges(stranded: Pressed) -> Vec<(u16, CGEventFlags)> {
    let mut flags = CGEventFlags::CGEventFlagNull;
    let mut edges = Vec::new();
    for usage in stranded.usages() {
        let Some(vk) = hid_usage_to_macos(usage) else {
            continue;
        };
        if let Some(bit) = modifier_bit(usage) {
            flags |= modifier_flags(bit);
        }
        edges.push((vk, flags));
    }
    edges
}

/// The process's device, while one is installed.
static SLOT: Mutex<Option<Installed<Device>>> = Mutex::new(None);

/// The identity the next installation takes.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn lock_slot() -> MutexGuard<'static, Option<Installed<Device>>> {
    // The slot only records which keys the device reports down; a panic
    // elsewhere leaves that record as consistent as the device.
    SLOT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An installed device and the installation it belongs to.
struct Installed<S: Sink> {
    id: u64,
    keyboard: Keyboard<S>,
}

/// What became of an input offered to the slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
struct Offer {
    /// Whether the device carried the input. If not, the caller posts it as
    /// a `CGEvent`.
    carried: bool,
    /// Keys that other inputs hold and that the removal of a failed device
    /// released. They must go down again as `CGEvent`s, because their
    /// release now goes there.
    stranded: Pressed,
}

impl Offer {
    const fn routed(carried: bool) -> Self {
        Self {
            carried,
            stranded: Pressed::EMPTY,
        }
    }
}

/// Run `apply` against the installed keyboard. A keyboard whose device
/// rejected a report is removed, which reports its keys released.
fn offer<S: Sink>(
    slot: &mut Option<Installed<S>>,
    apply: impl FnOnce(&mut Keyboard<S>) -> Routed,
) -> Offer {
    let Some(installed) = slot.as_mut() else {
        return Offer::routed(false);
    };
    match apply(&mut installed.keyboard) {
        Routed::Sent => Offer::routed(true),
        Routed::Declined => Offer::routed(false),
        Routed::Failed {
            delivered,
            stranded,
            error: SendError(status),
        } => {
            tracing::warn!(
                status = format_args!("{status:#010x}"),
                "virtual HID keyboard rejected a report; removing it, synthesized keys use \
                 CGEventPost again"
            );
            drop(slot.take());
            Offer {
                carried: delivered,
                stranded,
            }
        }
    }
}

/// The modifier bitmap bits for `flags`' Command, Shift, Control and Option.
fn modifier_bits(flags: CGEventFlags) -> u8 {
    MODIFIER_FLAGS
        .into_iter()
        .filter(|&(flag, _)| flags.contains(flag))
        .fold(0, |bits, (_, bit)| bits | bit)
}

/// The `CGEvent` flags of the modifier bitmap `bits`, the inverse of
/// [`modifier_bits`].
fn modifier_flags(bits: u8) -> CGEventFlags {
    MODIFIER_FLAGS
        .into_iter()
        .filter(|&(_, bit)| bits & bit != 0)
        .fold(CGEventFlags::CGEventFlagNull, |flags, (flag, _)| {
            flags | flag
        })
}

/// One input report, laid out as [`REPORT_DESCRIPTOR`] declares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Report([u8; REPORT_LEN]);

impl Report {
    fn bytes(&self) -> &[u8; REPORT_LEN] {
        &self.0
    }
}

/// The keys the device reports as down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pressed {
    modifiers: u8,
    /// Key usages in press order; `0` marks a free slot, and free slots
    /// follow the used ones.
    keys: [u8; KEY_SLOTS],
}

impl Pressed {
    /// No key down.
    const EMPTY: Self = Self {
        modifiers: 0,
        keys: [0; KEY_SLOTS],
    };

    fn holds(self, usage: u8) -> bool {
        match modifier_bit(usage) {
            Some(bit) => self.modifiers & bit != 0,
            None => self.keys.contains(&usage),
        }
    }

    /// Report `usage` down. `false`, leaving the state unchanged, when all
    /// key slots are taken.
    fn press(&mut self, usage: u8) -> bool {
        if let Some(bit) = modifier_bit(usage) {
            self.modifiers |= bit;
            return true;
        }
        if self.keys.contains(&usage) {
            return true;
        }
        match self.keys.iter_mut().find(|slot| **slot == 0) {
            Some(slot) => {
                *slot = usage;
                true
            }
            None => false,
        }
    }

    /// Report `usage` up, keeping the remaining keys in press order.
    fn release(&mut self, usage: u8) {
        if let Some(bit) = modifier_bit(usage) {
            self.modifiers &= !bit;
            return;
        }
        let mut keys = [0; KEY_SLOTS];
        for (slot, key) in keys.iter_mut().zip(
            self.keys
                .into_iter()
                .filter(|&key| key != 0 && key != usage),
        ) {
            *slot = key;
        }
        self.keys = keys;
    }

    fn is_empty(self) -> bool {
        self == Self::EMPTY
    }

    /// The usages down: the modifiers in bitmap order, then the keys in
    /// press order.
    fn usages(self) -> impl Iterator<Item = u8> {
        MODIFIER_USAGES
            .filter(move |&usage| self.holds(usage))
            .chain(self.keys.into_iter().filter(|&key| key != 0))
    }

    fn report(self) -> Report {
        let mut bytes = [0; REPORT_LEN];
        bytes[0] = self.modifiers;
        bytes[2..].copy_from_slice(&self.keys);
        Report(bytes)
    }
}

/// The modifier bitmap bit of `usage`, if it is a modifier.
fn modifier_bit(usage: u8) -> Option<u8> {
    MODIFIER_USAGES
        .contains(&usage)
        .then(|| 1 << (usage - MODIFIER_USAGES.start()))
}

/// Where reports go: the device, or a recorder in tests.
trait Sink {
    fn send(&self, report: Report) -> Result<(), SendError>;
}

/// The device rejected a report with this `IOReturn`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SendError(i32);

/// What the keyboard did with an edge or tap offered to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Routed {
    /// The device carried it.
    Sent,
    /// Not the device's to carry; the caller posts a `CGEvent`.
    Declined,
    /// A report failed, so the keyboard must be removed. `delivered` is
    /// whether the offered input still took effect: removal releases every
    /// key, which completes an up edge, but a down edge that never reached
    /// the device must go out as a `CGEvent`. `stranded` is the keys other
    /// inputs held before this one, which the removal releases under them.
    Failed {
        delivered: bool,
        stranded: Pressed,
        error: SendError,
    },
}

/// A device and the keys it reports as down.
///
/// Dropping it reports every key released, so no removal path leaves a key
/// held.
struct Keyboard<S: Sink> {
    sink: S,
    pressed: Pressed,
    /// Whether the device is known to report [`Self::pressed`]; `false`
    /// after a failed send, when the device's state is unknown.
    synced: bool,
}

impl<S: Sink> Keyboard<S> {
    fn new(sink: S) -> Self {
        Self {
            sink,
            pressed: Pressed::EMPTY,
            synced: true,
        }
    }

    /// One edge of a held key.
    fn edge(&mut self, usage: u8, phase: KeyPhase) -> Routed {
        let mut stranded = self.pressed;
        stranded.release(usage);
        let mut next = self.pressed;
        match phase {
            KeyPhase::Down => {
                if !next.press(usage) {
                    return Routed::Declined;
                }
            }
            KeyPhase::Up => {
                if !self.pressed.holds(usage) {
                    return Routed::Declined;
                }
                next.release(usage);
            }
        }
        match self.report(next) {
            Ok(()) => Routed::Sent,
            Err(error) => Routed::Failed {
                delivered: phase == KeyPhase::Up,
                stranded,
                error,
            },
        }
    }

    /// Press and release `usage` with the `modifiers` bitmap held.
    ///
    /// Modifiers a held chord already holds stay down afterwards; the rest
    /// are pressed before the key and released after it, as a keyboard
    /// reports them. A key the device already holds is declined, because
    /// tapping it would release it under the chord that holds it.
    fn tap(&mut self, usage: u8, modifiers: u8) -> Routed {
        if self.pressed.holds(usage) {
            return Routed::Declined;
        }
        let before = self.pressed;
        let mut modified = before;
        modified.modifiers |= modifiers;
        let mut down = modified;
        if !down.press(usage) {
            return Routed::Declined;
        }
        if let Err(error) = self.report(modified).and_then(|()| self.report(down)) {
            return Routed::Failed {
                delivered: false,
                stranded: before,
                error,
            };
        }
        if let Err(error) = self.report(modified).and_then(|()| self.report(before)) {
            return Routed::Failed {
                delivered: true,
                stranded: before,
                error,
            };
        }
        Routed::Sent
    }

    /// Send `next` as the device's state, unless it already is.
    fn report(&mut self, next: Pressed) -> Result<(), SendError> {
        if self.synced && next == self.pressed {
            return Ok(());
        }
        self.pressed = next;
        self.synced = false;
        self.sink.send(next.report())?;
        self.synced = true;
        Ok(())
    }
}

impl<S: Sink> Drop for Keyboard<S> {
    fn drop(&mut self) {
        if self.synced && self.pressed.is_empty() {
            return;
        }
        if let Err(SendError(status)) = self.report(Pressed::EMPTY) {
            tracing::warn!(
                status = format_args!("{status:#010x}"),
                "virtual HID keyboard could not report its keys released before removal"
            );
        }
    }
}
