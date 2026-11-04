//! Software scroll inversion standing in for a wheel's native inversion while
//! a reconnected mouse waits for the agent to write it again.
//!
//! A mouse that inverts its wheel natively (`0x2121`) forgets that across a
//! power cycle, and scrolls the moment the OS has it back. The agent can only
//! write the bit again after it has reached the device over HID++, so the
//! first scrolling after power-on went the wrong way. The capture layer can
//! reverse that scrolling in software
//! ([`openlogi_hook::set_interim_scroll_inversion`]) until the write is
//! confirmed — but only for a wheel that does lose the bit: one that keeps it
//! across a reconnect would be reversed twice, the wrong way, until the write
//! found it already set.
//!
//! So whether a wheel loses its inversion is learned, not assumed. The
//! evidence is the first wheel check after a reconnect: the wheel either still
//! held the inversion the agent had confirmed before it went away, or it did
//! not — the inversion bit alone, whatever happened to the resolution. A wheel enters interim inversion once it has been seen to lose it,
//! and stays armed even when a later reconnect finds the bit kept — a Bluetooth
//! drop keeps it where a power cycle loses it. The two mistakes are not alike:
//! arming a wheel that kept its inversion reverses it twice only until the
//! check reads the wheel, which then withdraws the reversal; not arming one
//! that lost it scrolls it the wrong way until the write lands as well.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};

use openlogi_hid::{DeviceRoute, ScrollWheelMode};
use openlogi_hook::WheelSource;

/// How long a wheel must have sent no reversed scroll for its native
/// inversion to take over: longer than the gap between a ratchet's detents
/// mid-scroll, so the switch lands between scrolls, not inside one.
const WHEEL_REST: Duration = Duration::from_millis(250);

/// The longest the native switch waits for a resting wheel. Past it, a wheel
/// still scrolling gets its native inversion anyway: the interim one cannot
/// stand in for it for ever.
const WHEEL_REST_LONGEST_WAIT: Duration = Duration::from_millis(1500);

// The wait runs inside the wheel write's timeout, which must leave the write
// itself room to land.
const _: () = assert!(
    WHEEL_REST_LONGEST_WAIT.as_millis() + 1000 < crate::hardware::WRITE_TIMEOUT.as_millis()
);

/// How much longer to wait for the wheel to rest, given how long interim
/// inversion has been idle; `None` once it has rested. A wheel interim
/// inversion never reversed is resting.
fn rest_wait(idle: Option<Duration>) -> Option<Duration> {
    WHEEL_REST.checked_sub(idle?).filter(|wait| !wait.is_zero())
}

/// One wheel as the inventory sees it, for [`InterimInversion::sync`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WheelPresence {
    pub(crate) source: WheelSource,
    /// Whether the agent writes native inversion to this wheel.
    pub(crate) inverts_natively: bool,
    pub(crate) online: bool,
}

/// The wheel attributed to scroll from `route`'s device: only a direct
/// device, whose HID node is the device itself. A receiver's node carries
/// every device paired to it, so reversing it would reverse them all.
pub(crate) fn wheel_source(route: &DeviceRoute) -> Option<WheelSource> {
    match *route {
        DeviceRoute::Direct {
            vendor_id,
            product_id,
        } => Some(WheelSource {
            vendor_id: u32::from(vendor_id),
            product_id: u32::from(product_id),
        }),
        _ => None,
    }
}

/// What a reconnect has shown about a wheel's native inversion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Retention {
    /// No reconnect observed yet.
    #[default]
    Unknown,
    /// A reconnect found the inversion gone. Final: see the module docs.
    Loses,
    /// Every reconnect found the inversion still set.
    Keeps,
}

/// Where a wheel's link stands, as far as its native inversion goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Link {
    /// Not seen since the agent started: whatever it is online with, the
    /// previous agent may have inverted it, so nothing is known.
    #[default]
    Unseen,
    /// Not online. `left_inverted`: its native inversion was confirmed when
    /// it went, so the first write after its return is evidence.
    Away { left_inverted: bool },
    /// Online, native inversion not known to be in effect: first seen online
    /// by this agent, or no longer inverted natively.
    Unconfirmed,
    /// Online again after the agent saw it go, its first check still to
    /// come. A wheel that loses its inversion has lost it now.
    Returned { left_inverted: bool },
    /// Online again, checked, and found without its inversion: the write
    /// that restores it is still to land.
    Restoring,
    /// Online, native inversion confirmed on this connection.
    Confirmed,
}

#[derive(Clone, Copy, Debug, Default)]
struct Unit {
    /// The agent writes native inversion to this wheel.
    inverts_natively: bool,
    retention: Retention,
    link: Link,
}

impl Unit {
    /// Reversed in software: a wheel known to lose its inversion, between a
    /// departure the agent saw and the write that restores it. Never one the
    /// agent found online at start — that one may still be inverted.
    fn interim(self) -> bool {
        self.inverts_natively
            && self.retention == Retention::Loses
            && matches!(
                self.link,
                Link::Away { .. } | Link::Returned { .. } | Link::Restoring
            )
    }

    fn go_offline(&mut self) {
        let left_inverted = match self.link {
            Link::Away { .. } => return,
            Link::Confirmed => true,
            Link::Unseen | Link::Unconfirmed | Link::Returned { .. } | Link::Restoring => false,
        };
        self.link = Link::Away { left_inverted };
    }

    fn go_online(&mut self) {
        self.link = match self.link {
            Link::Unseen => Link::Unconfirmed,
            Link::Away { left_inverted } => Link::Returned { left_inverted },
            online => online,
        };
    }

    /// The agent stopped writing native inversion: nothing it confirmed, and
    /// nothing a return could show, applies any more.
    fn stop_inverting(&mut self) {
        self.link = match self.link {
            Link::Unseen => Link::Unseen,
            Link::Away { .. } => Link::Away {
                left_inverted: false,
            },
            Link::Unconfirmed | Link::Returned { .. } | Link::Restoring | Link::Confirmed => {
                Link::Unconfirmed
            }
        };
    }

    /// A wheel write's check found `found` on the wheel, before writing.
    ///
    /// The inversion bit alone decides, whatever else the write still has to
    /// change: a wheel found inverted is withdrawn from interim inversion on
    /// the read, and one found without it stays reversed until the write
    /// lands.
    fn checked(&mut self, found: ScrollWheelMode) {
        if self.link
            == (Link::Returned {
                left_inverted: true,
            })
        {
            self.retention = match (self.retention, found.inverted) {
                (Retention::Loses, _) | (_, false) => Retention::Loses,
                (_, true) => Retention::Keeps,
            };
        }
        self.link = match self.link {
            away @ (Link::Unseen | Link::Away { .. }) => away,
            _ if self.inverts_natively && found.inverted => Link::Confirmed,
            Link::Returned { .. } | Link::Restoring => Link::Restoring,
            Link::Unconfirmed | Link::Confirmed => Link::Unconfirmed,
        };
    }

    /// A wheel write left `left` on the wheel.
    fn wrote(&mut self, left: ScrollWheelMode) {
        if matches!(self.link, Link::Unseen | Link::Away { .. }) {
            return;
        }
        self.link = if self.inverts_natively && left.inverted {
            Link::Confirmed
        } else {
            Link::Unconfirmed
        };
    }
}

/// The per-wheel state behind [`InterimInversion`]: pure transitions, and the
/// wheels they leave reversed in software.
#[derive(Debug, Default)]
struct Wheels {
    units: HashMap<WheelSource, Unit>,
}

impl Wheels {
    /// Fold in the inventory's view. A wheel missing from `present` has left
    /// the inventory, which is going offline as far as its inversion goes.
    fn sync(&mut self, present: &[WheelPresence]) {
        for (source, unit) in &mut self.units {
            if !present.iter().any(|wheel| wheel.source == *source) {
                unit.go_offline();
            }
        }
        for wheel in present {
            let unit = self.units.entry(wheel.source).or_default();
            unit.inverts_natively = wheel.inverts_natively;
            if !unit.inverts_natively {
                unit.stop_inverting();
            }
            if wheel.online {
                unit.go_online();
            } else {
                unit.go_offline();
            }
        }
    }

    /// A wheel write's check on `source` found `found`.
    fn checked(&mut self, source: WheelSource, found: ScrollWheelMode) {
        if let Some(unit) = self.units.get_mut(&source) {
            unit.checked(found);
        }
    }

    /// A wheel write to `source` left `left`.
    fn wrote(&mut self, source: WheelSource, left: ScrollWheelMode) {
        if let Some(unit) = self.units.get_mut(&source) {
            unit.wrote(left);
        }
    }

    /// The connections `sources` were known by have ended.
    fn went_away(&mut self, sources: &[WheelSource]) {
        for source in sources {
            if let Some(unit) = self.units.get_mut(source) {
                unit.go_offline();
            }
        }
    }

    fn interim(&self) -> Vec<WheelSource> {
        let mut sources: Vec<WheelSource> = self
            .units
            .iter()
            .filter(|(_, unit)| unit.interim())
            .map(|(source, _)| *source)
            .collect();
        sources.sort_unstable_by_key(|source| (source.vendor_id, source.product_id));
        sources
    }
}

impl Wheels {
    /// What reconnects have shown, by wheel: the part worth keeping across
    /// agent restarts.
    fn learned(&self) -> Vec<LearnedWheel> {
        let mut learned: Vec<LearnedWheel> = self
            .units
            .iter()
            .filter(|(_, unit)| unit.retention != Retention::Unknown)
            .map(|(source, unit)| LearnedWheel {
                vendor_id: source.vendor_id,
                product_id: source.product_id,
                retention: unit.retention,
            })
            .collect();
        learned.sort_unstable_by_key(|wheel| (wheel.vendor_id, wheel.product_id));
        learned
    }

    fn remember(&mut self, learned: &[LearnedWheel]) {
        for wheel in learned {
            let source = WheelSource {
                vendor_id: wheel.vendor_id,
                product_id: wheel.product_id,
            };
            self.units.entry(source).or_default().retention = wheel.retention;
        }
    }
}

/// One wheel's learned retention as kept on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LearnedWheel {
    vendor_id: u32,
    product_id: u32,
    retention: Retention,
}

/// The file the learned retentions live in, so a restarted agent needs no
/// reconnect to learn them again — that reconnect scrolls the wrong way
/// until the native write lands.
#[derive(Serialize, Deserialize)]
struct LearnedFile {
    version: u32,
    wheels: Vec<LearnedWheel>,
}

const LEARNED_FILE_VERSION: u32 = 1;

fn load_learned(path: &Path) -> Vec<LearnedWheel> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    match serde_json::from_slice::<LearnedFile>(&bytes) {
        Ok(file) if file.version == LEARNED_FILE_VERSION => file.wheels,
        _ => {
            tracing::warn!(?path, "learned wheel inversion unreadable — learning again");
            Vec::new()
        }
    }
}

fn save_learned(path: &Path, wheels: &[LearnedWheel]) -> std::io::Result<()> {
    let json = serde_json::to_vec(&LearnedFile {
        version: LEARNED_FILE_VERSION,
        wheels: wheels.to_vec(),
    })
    .map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = AtomicWriteFile::open(path)?;
    std::io::Write::write_all(&mut out, &json)?;
    out.commit()
}

/// The agent's interim inversion: [`Wheels`] plus what it last handed the
/// capture layer. Cloned into every wheel write so the write's own thread
/// withdraws a wheel the moment its native inversion is confirmed.
#[derive(Clone, Default)]
pub(crate) struct InterimInversion {
    inner: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    wheels: Wheels,
    published: Vec<WheelSource>,
    /// Where learned retentions are kept; `None` keeps them in memory only.
    file: Option<PathBuf>,
    saved: Vec<LearnedWheel>,
}

impl InterimInversion {
    /// Keep what reconnects teach in `path`, starting from what it holds.
    pub(crate) fn remember_in(&self, path: PathBuf) {
        let learned = load_learned(&path);
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        state.wheels.remember(&learned);
        state.saved = state.wheels.learned();
        state.file = Some(path);
    }

    /// Fold in the inventory's view of every wheel the agent writes to.
    pub(crate) fn sync(&self, present: &[WheelPresence]) {
        self.update(|wheels| wheels.sync(present));
    }

    /// A native wheel write's check on `route` found `found` on the wheel.
    pub(crate) fn checked(&self, route: &DeviceRoute, found: ScrollWheelMode) {
        if let Some(source) = wheel_source(route) {
            self.update(|wheels| wheels.checked(source, found));
        }
    }

    /// A native wheel write to `route` left `left` on the wheel.
    pub(crate) fn wrote(&self, route: &DeviceRoute, left: ScrollWheelMode) {
        if let Some(source) = wheel_source(route) {
            self.update(|wheels| wheels.wrote(source, left));
        }
    }

    /// Wheels whose previous connection ended without the inventory ever
    /// publishing them offline: a quick power cycle swaps a Bluetooth
    /// device's OS node between two reconciliations. Their return is a
    /// reconnect all the same.
    pub(crate) fn went_away(&self, routes: &[DeviceRoute]) {
        let sources: Vec<WheelSource> = routes.iter().filter_map(wheel_source).collect();
        if sources.is_empty() {
            return;
        }
        self.update(|wheels| wheels.went_away(&sources));
    }

    /// Wait, while interim inversion reverses `route`'s wheel, until that
    /// wheel rests — or [`WHEEL_REST_LONGEST_WAIT`] passes. Called between a
    /// check that found the native write due and the write itself, so a wheel
    /// that kept its inversion is never held reversed twice by the wait.
    ///
    /// The native write switches the wheel the moment it lands, and interim
    /// inversion is withdrawn only once the write's answer is back. Every
    /// report the wheel sends between the two is reversed twice. Mid-scroll
    /// that is many notches the wrong way — a free-spinning wheel sends
    /// hundreds a second — which a browser's smooth scrolling then animates
    /// into a visible reversal. A resting wheel sends none.
    pub(crate) async fn wait_for_rest(&self, route: &DeviceRoute) {
        if !self.reverses(route) {
            return;
        }
        let deadline = tokio::time::Instant::now() + WHEEL_REST_LONGEST_WAIT;
        while let Some(wait) = rest_wait(openlogi_hook::interim_scroll_idle()) {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                tracing::debug!("wheel still scrolling — switching its native inversion anyway");
                return;
            }
            tokio::time::sleep(wait.min(deadline.saturating_duration_since(now))).await;
        }
    }

    /// Whether interim inversion currently reverses `route`'s wheel.
    fn reverses(&self, route: &DeviceRoute) -> bool {
        wheel_source(route).is_some_and(|source| {
            self.inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .published
                .contains(&source)
        })
    }

    fn update(&self, change: impl FnOnce(&mut Wheels)) {
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        change(&mut state.wheels);
        let learned = state.wheels.learned();
        if learned != state.saved {
            if let Some(path) = &state.file
                && let Err(error) = save_learned(path, &learned)
            {
                tracing::warn!(%error, "could not keep the learned wheel inversion");
            }
            state.saved = learned;
        }
        // Where the capture layer cannot reverse scroll, nothing stands in.
        let interim = if openlogi_hook::SOFTWARE_SCROLL_INVERSION {
            state.wheels.interim()
        } else {
            Vec::new()
        };
        if interim != state.published {
            if interim.len() > state.published.len() {
                tracing::debug!(
                    ?interim,
                    "reversing a reconnected wheel until its native inversion is back"
                );
            } else {
                tracing::debug!(
                    ?interim,
                    "native inversion confirmed — interim inversion withdrawn"
                );
            }
            openlogi_hook::set_interim_scroll_inversion(interim.clone());
            state.published = interim;
        }
    }
}

#[cfg(test)]
mod tests {
    use openlogi_hid::{ScrollReportingTarget, ScrollResolution};

    use super::*;

    const MOUSE: WheelSource = WheelSource {
        vendor_id: 0x046d,
        product_id: 0xb023,
    };

    fn present(online: bool) -> WheelPresence {
        WheelPresence {
            source: MOUSE,
            inverts_natively: true,
            online,
        }
    }

    fn mode(inverted: bool) -> ScrollWheelMode {
        ScrollWheelMode {
            resolution: ScrollResolution::Low,
            inverted,
            target: ScrollReportingTarget::Native,
        }
    }

    /// A write that found the inversion gone and set it: found, then left.
    fn lost() -> (ScrollWheelMode, ScrollWheelMode) {
        (mode(false), mode(true))
    }

    /// A check that found the inversion still set.
    fn kept() -> (ScrollWheelMode, ScrollWheelMode) {
        (mode(true), mode(true))
    }

    /// A write's check, then the write landing.
    fn visit(wheels: &mut Wheels, (found, left): (ScrollWheelMode, ScrollWheelMode)) {
        wheels.checked(MOUSE, found);
        wheels.wrote(MOUSE, left);
    }

    /// The agent starts with the mouse online, confirms its inversion, and
    /// then sees it go away and come back.
    fn reconnected() -> Wheels {
        let mut wheels = Wheels::default();
        wheels.sync(&[present(true)]);
        visit(&mut wheels, kept());
        wheels.sync(&[present(false)]);
        wheels.sync(&[present(true)]);
        wheels
    }

    #[test]
    fn a_wheel_seen_to_lose_its_inversion_is_reversed_until_the_write_lands() {
        let mut wheels = reconnected();
        assert!(wheels.interim().is_empty(), "nothing learned yet");
        visit(&mut wheels, lost());
        assert!(wheels.interim().is_empty(), "the write landed");

        wheels.sync(&[]);
        assert_eq!(
            wheels.interim(),
            vec![MOUSE],
            "armed while it is away, so its first scroll back is reversed"
        );
        wheels.sync(&[present(true)]);
        assert_eq!(wheels.interim(), vec![MOUSE], "still waiting on the write");
        visit(&mut wheels, lost());
        assert!(wheels.interim().is_empty(), "withdrawn once confirmed");
    }

    #[test]
    fn a_wheel_that_keeps_its_inversion_is_never_reversed() {
        let mut wheels = reconnected();
        visit(&mut wheels, kept());
        for _ in 0..3 {
            wheels.sync(&[present(false)]);
            assert!(wheels.interim().is_empty());
            wheels.sync(&[present(true)]);
            visit(&mut wheels, kept());
        }
        assert_eq!(wheels.learned()[0].retention, Retention::Keeps);
    }

    /// The wheel kept its inversion but not its resolution: a write was due,
    /// and it still says nothing about losing the inversion.
    #[test]
    fn a_return_that_kept_the_inversion_but_not_the_resolution_keeps_it() {
        let mut wheels = reconnected();
        let found = ScrollWheelMode {
            resolution: ScrollResolution::High,
            ..mode(true)
        };
        wheels.checked(MOUSE, found);
        wheels.wrote(MOUSE, mode(true));
        assert_eq!(wheels.learned()[0].retention, Retention::Keeps);
        wheels.sync(&[present(false)]);
        assert!(wheels.interim().is_empty());
    }

    /// A Bluetooth drop keeps the bit where a power cycle loses it: a wheel
    /// once seen to lose it stays armed, and its reversal is withdrawn as soon
    /// as a reconnect's check finds the bit kept.
    #[test]
    fn a_wheel_seen_to_lose_its_inversion_stays_armed_when_one_return_keeps_it() {
        let mut wheels = reconnected();
        visit(&mut wheels, lost());
        wheels.sync(&[present(false)]);
        wheels.sync(&[present(true)]);
        visit(&mut wheels, kept());
        assert!(wheels.interim().is_empty(), "withdrawn on the check");

        wheels.sync(&[present(false)]);
        assert_eq!(wheels.interim(), vec![MOUSE], "armed for the next return");
    }

    /// A wheel that kept its inversion once and lost it later is armed from
    /// then on.
    #[test]
    fn a_wheel_that_kept_its_inversion_is_armed_once_it_loses_it() {
        let mut wheels = reconnected();
        visit(&mut wheels, kept());
        wheels.sync(&[present(false)]);
        wheels.sync(&[present(true)]);
        visit(&mut wheels, lost());
        wheels.sync(&[present(false)]);
        assert_eq!(wheels.interim(), vec![MOUSE]);
    }

    /// The agent's first write after it starts says nothing about what a
    /// reconnect does: the previous agent may have set the bit, or nobody.
    #[test]
    fn a_write_without_a_reconnect_behind_it_teaches_nothing() {
        let mut wheels = Wheels::default();
        wheels.sync(&[present(true)]);
        visit(&mut wheels, lost());
        wheels.sync(&[present(false)]);
        assert!(wheels.interim().is_empty());

        let mut first_seen_offline = Wheels::default();
        first_seen_offline.sync(&[present(false)]);
        first_seen_offline.sync(&[present(true)]);
        visit(&mut first_seen_offline, lost());
        first_seen_offline.sync(&[present(false)]);
        assert!(
            first_seen_offline.interim().is_empty(),
            "never confirmed inverted before it left, so its return proves nothing"
        );
    }

    /// Only native inversion the agent writes is stood in for: a wheel set to
    /// scroll as it reports, or whose wheel a capture session owns, is not.
    #[test]
    fn a_wheel_the_agent_does_not_invert_is_never_reversed() {
        let mut wheels = reconnected();
        visit(&mut wheels, lost());
        wheels.sync(&[WheelPresence {
            inverts_natively: false,
            ..present(false)
        }]);
        assert!(wheels.interim().is_empty());
    }

    /// A wheel known to lose its inversion that kept it this time, with
    /// another setting still to write: the read withdraws the reversal, so
    /// the wait before the write does not reverse it twice.
    #[test]
    fn a_check_that_finds_the_inversion_withdraws_before_any_write() {
        let mut wheels = reconnected();
        visit(&mut wheels, lost());
        wheels.sync(&[present(false)]);
        wheels.sync(&[present(true)]);
        assert_eq!(wheels.interim(), vec![MOUSE]);
        wheels.checked(
            MOUSE,
            ScrollWheelMode {
                resolution: ScrollResolution::High,
                ..mode(true)
            },
        );
        assert!(wheels.interim().is_empty(), "withdrawn on the read");
    }

    #[test]
    fn a_failed_write_leaves_the_wheel_reversed() {
        let mut wheels = reconnected();
        visit(&mut wheels, lost());
        wheels.sync(&[present(false)]);
        wheels.sync(&[present(true)]);
        wheels.checked(MOUSE, mode(false));
        // No write landed, so the wheel still scrolls as it reports and the
        // software reversal is what keeps it right.
        assert_eq!(wheels.interim(), vec![MOUSE]);
    }

    /// A quick power cycle swaps the node before the inventory sees the
    /// wheel offline; reported as gone, its return is a reconnect like any.
    #[test]
    fn a_wheel_reported_gone_between_snapshots_returns_like_any_other() {
        let mut wheels = reconnected();
        visit(&mut wheels, lost());
        // Seen online in every snapshot, but its node was swapped.
        wheels.went_away(&[MOUSE]);
        assert_eq!(
            wheels.interim(),
            vec![MOUSE],
            "armed the moment it is known gone"
        );
        wheels.sync(&[present(true)]);
        visit(&mut wheels, lost());
        assert!(wheels.interim().is_empty());
    }

    #[test]
    fn the_native_switch_waits_for_the_wheel_to_rest() {
        assert_eq!(rest_wait(None), None, "never reversed: nothing to wait for");
        assert_eq!(
            rest_wait(Some(Duration::from_millis(40))),
            Some(Duration::from_millis(210))
        );
        assert_eq!(rest_wait(Some(WHEEL_REST)), None);
    }

    /// What a reconnect taught survives the agent: a restarted one reverses
    /// the wheel on its very first return, with nothing to learn again.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn a_learned_retention_survives_a_restart() {
        const ROUTE: DeviceRoute = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb023,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wheel-inversion.json");
        let first = InterimInversion::default();
        first.remember_in(path.clone());
        first.sync(&[present(true)]);
        first.checked(&ROUTE, kept().0);
        first.wrote(&ROUTE, kept().1);
        first.sync(&[present(false)]);
        first.sync(&[present(true)]);
        first.checked(&ROUTE, lost().0);
        first.wrote(&ROUTE, lost().1);

        let restarted = InterimInversion::default();
        restarted.remember_in(path);
        restarted.sync(&[present(true)]);
        assert!(
            !restarted.reverses(&ROUTE),
            "online at start: the previous agent may have inverted it"
        );
        restarted.sync(&[present(false)]);
        assert!(restarted.reverses(&ROUTE), "its first return is covered");
    }

    #[test]
    fn an_unreadable_learned_file_is_learned_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wheel-inversion.json");
        std::fs::write(&path, b"not json").expect("write");
        assert!(load_learned(&path).is_empty());
        assert!(load_learned(&dir.path().join("missing.json")).is_empty());
    }

    #[test]
    fn only_a_direct_device_has_a_wheel_source_of_its_own() {
        assert_eq!(
            wheel_source(&DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb023,
            }),
            Some(MOUSE)
        );
    }
}
