//! Click debounce for worn switches (#2182).
//!
//! A worn mouse switch bounces: one physical click reaches the host as press,
//! release, press, release within a few milliseconds, and the OS reads a
//! double-click. A device with a [`ClickDebounce`] window drops a press that
//! follows the same button's release sooner than the window, and drops that
//! press's release with it, so the OS sees one press and one release.
//!
//! The filter never delays an event and never drops a release it let the
//! press of through, so a button cannot stay down at the OS. The cost of that
//! rule is a bounce in the middle of a hold: the OS has already seen the
//! release, so a drag ends there.
//!
//! The hook knows a device only by the vendor and product ids of its events,
//! and two devices can share them: two mice behind one receiver on macOS, or
//! two mice of one model. Ids that another clicking device without a window
//! also uses are not filtered, so a device that did not opt in never loses a
//! click.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use openlogi_core::binding::ButtonId;
use openlogi_core::config::ClickDebounce;
use openlogi_core::device::ReceiverInfo;
use openlogi_hook::{EventDevice, EventDisposition};

/// The vendor and product ids one device's button events carry when the OS
/// hook sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InputSource {
    vendor_id: u16,
    product_id: u16,
}

impl InputSource {
    /// The sources a paired device's button events can arrive under.
    ///
    /// The first is the HID node the host enumerated: the device itself for
    /// a direct attachment (its synthetic receiver mirrors its own ids), and
    /// the receiver otherwise, because macOS attributes a receiver's clicks to
    /// the receiver. The second is the pairing's wireless product id, because
    /// Linux `hid-logitech-dj` gives each paired device its own input node
    /// under that id. Devices that share one receiver share its first source.
    pub(crate) fn of_pairing(receiver: &ReceiverInfo, wpid: Option<u16>) -> Vec<Self> {
        let node = Self {
            vendor_id: receiver.vendor_id,
            product_id: receiver.product_id,
        };
        let mut sources = vec![node];
        if let Some(wpid) = wpid
            && wpid != node.product_id
        {
            sources.push(Self {
                vendor_id: receiver.vendor_id,
                product_id: wpid,
            });
        }
        sources
    }

    /// The source the platform reported for an event, when it reported both
    /// ids and both fit the HID 16-bit range.
    fn of_event(device: &EventDevice) -> Option<Self> {
        Some(Self {
            vendor_id: u16::try_from(device.vendor_id?).ok()?,
            product_id: u16::try_from(device.product_id?).ok()?,
        })
    }
}

/// The input sources whose clicks are filtered, each with its window. The
/// orchestrator builds it from the online devices; the hook callback reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DebouncedSources(Vec<(InputSource, ClickDebounce)>);

/// Each item is one device's claim on one source: the device's window, or
/// `None` for a device that can click but has no window. A source is filtered
/// only when every claim on it has a window, and then with the longest of
/// them: the source cannot tell those devices' clicks apart, and each of them
/// asked for at least that much filtering.
impl FromIterator<(InputSource, Option<ClickDebounce>)> for DebouncedSources {
    fn from_iter<I: IntoIterator<Item = (InputSource, Option<ClickDebounce>)>>(claims: I) -> Self {
        let mut merged: Vec<(InputSource, Option<ClickDebounce>)> = Vec::new();
        for (source, window) in claims {
            match merged.iter_mut().find(|(known, _)| *known == source) {
                Some((_, current)) => *current = current.zip(window).map(|(a, b)| a.max(b)),
                None => merged.push((source, window)),
            }
        }
        Self(
            merged
                .into_iter()
                .filter_map(|(source, window)| Some((source, window?)))
                .collect(),
        )
    }
}

impl DebouncedSources {
    /// The window for `source`, or `None` when its clicks are not filtered.
    fn window_for(&self, source: InputSource) -> Option<Duration> {
        self.0
            .iter()
            .find(|(known, _)| *known == source)
            .map(|(_, window)| window.duration())
    }
}

/// What the filter last did with one button of one source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonEdge {
    /// A press went through to the OS.
    Pressed,
    /// A press was dropped as bounce; its release must be dropped too.
    Swallowed,
    /// The button was released at this time.
    Released(Instant),
}

/// Per-source, per-button state of the click filter on one hook-callback
/// thread. Sans-I/O: the caller passes the time of each event.
#[derive(Debug, Default)]
pub(crate) struct ClickDebouncer(HashMap<(InputSource, ButtonId), ButtonEdge>);

impl ClickDebouncer {
    /// Decide one button edge from `device`. A source without a window passes
    /// every edge, except the release of a press this filter dropped before
    /// the window was withdrawn, so the OS never sees an unpaired release.
    pub(crate) fn filter(
        &mut self,
        sources: Option<&DebouncedSources>,
        device: Option<&EventDevice>,
        id: ButtonId,
        pressed: bool,
        now: Instant,
    ) -> EventDisposition {
        let Some(source) = device.and_then(InputSource::of_event) else {
            return EventDisposition::PassThrough;
        };
        let window = sources.and_then(|sources| sources.window_for(source));
        let key = (source, id);
        let last = self.0.get(&key).copied();
        let (next, disposition) = if pressed {
            press(last, window, now)
        } else {
            release(last, window, now)
        };
        match next {
            Some(edge) => self.0.insert(key, edge),
            None => self.0.remove(&key),
        };
        disposition
    }
}

/// A press is bounce when the same button was released less than `window`
/// ago. Without a window the filter keeps no state.
fn press(
    last: Option<ButtonEdge>,
    window: Option<Duration>,
    now: Instant,
) -> (Option<ButtonEdge>, EventDisposition) {
    let Some(window) = window else {
        return (None, EventDisposition::PassThrough);
    };
    match last {
        Some(ButtonEdge::Released(at)) if now.saturating_duration_since(at) < window => {
            (Some(ButtonEdge::Swallowed), EventDisposition::Suppress)
        }
        _ => (Some(ButtonEdge::Pressed), EventDisposition::PassThrough),
    }
}

/// A release follows its press: dropped when its press was dropped, passed
/// otherwise. Either way it starts the window for the next press, so a
/// bounce that repeats is dropped each time.
fn release(
    last: Option<ButtonEdge>,
    window: Option<Duration>,
    now: Instant,
) -> (Option<ButtonEdge>, EventDisposition) {
    let disposition = if last == Some(ButtonEdge::Swallowed) {
        EventDisposition::Suppress
    } else {
        EventDisposition::PassThrough
    };
    (window.map(|_| ButtonEdge::Released(now)), disposition)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use openlogi_core::binding::ButtonId;
    use openlogi_core::config::ClickDebounce;
    use openlogi_core::device::ReceiverInfo;
    use openlogi_hook::{EventDevice, EventDisposition};

    use super::{ClickDebouncer, DebouncedSources, InputSource};

    const PASS: EventDisposition = EventDisposition::PassThrough;
    const DROP: EventDisposition = EventDisposition::Suppress;

    fn receiver(product_id: u16) -> ReceiverInfo {
        ReceiverInfo {
            name: "USB Receiver".to_owned(),
            vendor_id: 0x046d,
            product_id,
            unique_id: None,
        }
    }

    fn event_device(product_id: u16) -> EventDevice {
        EventDevice {
            vendor_id: Some(0x046d),
            product_id: Some(u32::from(product_id)),
            product_name: None,
        }
    }

    fn window(ms: u8) -> ClickDebounce {
        ClickDebounce::try_new(ms).expect("valid test window")
    }

    /// A G903 on a Lightspeed receiver with a 30 ms window.
    fn g903() -> DebouncedSources {
        InputSource::of_pairing(&receiver(0xc539), Some(0x4087))
            .into_iter()
            .map(|source| (source, Some(window(30))))
            .collect()
    }

    /// Feed `edges` (pressed, milliseconds after the start) from `device` and
    /// collect the verdicts.
    fn run(
        sources: &DebouncedSources,
        device: &EventDevice,
        edges: &[(bool, u64)],
    ) -> Vec<EventDisposition> {
        let start = Instant::now();
        let mut filter = ClickDebouncer::default();
        edges
            .iter()
            .map(|&(pressed, ms)| {
                filter.filter(
                    Some(sources),
                    Some(device),
                    ButtonId::LeftClick,
                    pressed,
                    start + Duration::from_millis(ms),
                )
            })
            .collect()
    }

    #[test]
    fn a_release_bounce_is_dropped_with_its_release() {
        // The pattern from #2182: one click arrives as two inside 20 ms.
        let verdicts = run(
            &g903(),
            &event_device(0xc539),
            &[(true, 0), (false, 60), (true, 66), (false, 72)],
        );
        assert_eq!(verdicts, [PASS, PASS, DROP, DROP]);
    }

    #[test]
    fn a_repeated_bounce_is_dropped_each_time() {
        let verdicts = run(
            &g903(),
            &event_device(0xc539),
            &[
                (true, 0),
                (false, 60),
                (true, 63),
                (false, 66),
                (true, 70),
                (false, 74),
            ],
        );
        assert_eq!(verdicts, [PASS, PASS, DROP, DROP, DROP, DROP]);
    }

    #[test]
    fn a_deliberate_double_click_passes() {
        let verdicts = run(
            &g903(),
            &event_device(0xc539),
            &[(true, 0), (false, 70), (true, 140), (false, 210)],
        );
        assert_eq!(verdicts, [PASS; 4]);
    }

    #[test]
    fn the_wireless_product_id_is_the_same_device() {
        // Linux names a receiver's paired device by its wireless product id.
        let verdicts = run(
            &g903(),
            &event_device(0x4087),
            &[(true, 0), (false, 60), (true, 66), (false, 72)],
        );
        assert_eq!(verdicts, [PASS, PASS, DROP, DROP]);
    }

    #[test]
    fn another_device_is_not_filtered() {
        let verdicts = run(
            &g903(),
            &event_device(0xb034),
            &[(true, 0), (false, 60), (true, 66), (false, 72)],
        );
        assert_eq!(verdicts, [PASS; 4]);
    }

    #[test]
    fn an_unattributed_event_is_not_filtered() {
        let sources = g903();
        let start = Instant::now();
        let mut filter = ClickDebouncer::default();
        let verdicts: Vec<_> = [(true, 0), (false, 60), (true, 66), (false, 72)]
            .iter()
            .map(|&(pressed, ms)| {
                filter.filter(
                    Some(&sources),
                    None,
                    ButtonId::LeftClick,
                    pressed,
                    start + Duration::from_millis(ms),
                )
            })
            .collect();
        assert_eq!(verdicts, [PASS; 4]);
    }

    #[test]
    fn each_button_has_its_own_window() {
        let sources = g903();
        let device = event_device(0xc539);
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut filter = ClickDebouncer::default();
        let left = ButtonId::LeftClick;
        let right = ButtonId::RightClick;
        assert_eq!(
            filter.filter(Some(&sources), Some(&device), left, true, at(0)),
            PASS
        );
        assert_eq!(
            filter.filter(Some(&sources), Some(&device), left, false, at(50)),
            PASS
        );
        // A right press right after a left release is a different switch.
        assert_eq!(
            filter.filter(Some(&sources), Some(&device), right, true, at(55)),
            PASS
        );
    }

    #[test]
    fn a_dropped_press_keeps_its_release_dropped_after_the_filter_is_withdrawn() {
        let sources = g903();
        let device = event_device(0xc539);
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut filter = ClickDebouncer::default();
        let left = ButtonId::LeftClick;
        assert_eq!(
            filter.filter(Some(&sources), Some(&device), left, true, at(0)),
            PASS
        );
        assert_eq!(
            filter.filter(Some(&sources), Some(&device), left, false, at(60)),
            PASS
        );
        assert_eq!(
            filter.filter(Some(&sources), Some(&device), left, true, at(66)),
            DROP
        );
        // The user turns the filter off before the bounce's release arrives.
        let none = DebouncedSources::default();
        assert_eq!(
            filter.filter(Some(&none), Some(&device), left, false, at(72)),
            DROP
        );
        assert_eq!(
            filter.filter(Some(&none), Some(&device), left, true, at(74)),
            PASS
        );
    }

    #[test]
    fn a_lost_release_does_not_drop_the_next_press() {
        // The bounce's release never arrived; the next press is a new click.
        let verdicts = run(
            &g903(),
            &event_device(0xc539),
            &[
                (true, 0),
                (false, 60),
                (true, 66),
                (true, 400),
                (false, 460),
            ],
        );
        assert_eq!(verdicts, [PASS, PASS, DROP, PASS, PASS]);
    }

    #[test]
    fn a_shared_source_uses_the_longer_window() {
        let node = InputSource::of_pairing(&receiver(0xc548), None)[0];
        let sources: DebouncedSources = [20, 60, 40]
            .into_iter()
            .map(|ms| (node, Some(window(ms))))
            .collect();
        let verdicts = run(
            &sources,
            &event_device(0xc548),
            &[(true, 0), (false, 60), (true, 110), (false, 170)],
        );
        assert_eq!(verdicts, [PASS, PASS, DROP, DROP]);
    }

    #[test]
    fn a_source_another_device_clicks_through_without_a_window_is_not_filtered() {
        // Three G903s on three Lightspeed receivers; the second has no window.
        let sources: DebouncedSources = [Some(window(30)), None, Some(window(60))]
            .into_iter()
            .flat_map(|window| {
                InputSource::of_pairing(&receiver(0xc539), Some(0x4087))
                    .into_iter()
                    .map(move |source| (source, window))
            })
            .collect();
        assert_eq!(sources, DebouncedSources::default());
        for product_id in [0xc539, 0x4087] {
            let verdicts = run(
                &sources,
                &event_device(product_id),
                &[(true, 0), (false, 60), (true, 66), (false, 72)],
            );
            assert_eq!(verdicts, [PASS; 4]);
        }
    }

    #[test]
    fn a_direct_device_has_one_source() {
        let direct = ReceiverInfo {
            name: "MX Master 3S".to_owned(),
            vendor_id: 0x046d,
            product_id: 0xb034,
            unique_id: None,
        };
        assert_eq!(InputSource::of_pairing(&direct, Some(0xb034)).len(), 1);
        assert_eq!(
            InputSource::of_pairing(&receiver(0xc539), Some(0x4087)).len(),
            2
        );
    }
}
