//! The pure edge-zone state machine behind the Flow watcher.
//!
//! Ported from the polling shape of logi-gate's event tap: classify the
//! cursor into a per-display zone, debounce with a cooldown, and stay spent
//! until the cursor leaves the zone so one push cannot fire twice. No I/O —
//! every input arrives as an argument, so the whole machine is deterministic
//! under test.

use std::time::{Duration, Instant};

use openlogi_core::config::{FlowPlacements, FlowSide};
use openlogi_hook::{CursorPosition, DisplayBounds};

/// How deep a corner reaches along both axes, in display points. Wider than
/// [`EDGE_DEPTH`] so diagonal pushes land in the corner rather than whichever
/// edge classifies first.
const CORNER_DEPTH: f64 = 3.0;

/// How deep an edge reaches, in display points. The cursor clamps to the last
/// point row/column of a display, so 1 point means "pushed against the edge".
const EDGE_DEPTH: f64 = 1.0;

/// Screen zone a trigger binds: one of the four edges or four corners of a
/// display, in that display's own coordinate space. Internal vocabulary —
/// the config speaks in [`FlowSide`]s, which [`triggers_for`] expands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeZone {
    /// The left edge, excluding the corners.
    Left,
    /// The right edge, excluding the corners.
    Right,
    /// The top edge, excluding the corners.
    Top,
    /// The bottom edge, excluding the corners.
    Bottom,
    /// The top-left corner.
    TopLeft,
    /// The top-right corner.
    TopRight,
    /// The bottom-left corner.
    BottomLeft,
    /// The bottom-right corner.
    BottomRight,
}

/// One zone → host binding, expanded from a side placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneTrigger {
    /// The screen zone that arms this trigger.
    pub(crate) zone: EdgeZone,
    /// Zero-based host slot to switch to.
    pub(crate) host: u8,
}

/// Expand side placements into zone triggers: a side covers its edge plus
/// both adjacent corners (left ⇒ top-left + left + bottom-left).
///
/// Expansion order follows [`FlowSide::ALL`] with each side's edge first, so
/// a corner shared by two mapped sides (top-right when both right and top are
/// mapped) deterministically belongs to the earlier side in `ALL` order —
/// `EdgeStateMachine::observe` resolves a zone via first match.
#[must_use]
pub fn triggers_for(placements: &FlowPlacements) -> Vec<ZoneTrigger> {
    let mut triggers = Vec::new();
    for (side, host) in placements.iter() {
        let zones = match side {
            FlowSide::Left => [EdgeZone::Left, EdgeZone::TopLeft, EdgeZone::BottomLeft],
            FlowSide::Right => [EdgeZone::Right, EdgeZone::TopRight, EdgeZone::BottomRight],
            FlowSide::Top => [EdgeZone::Top, EdgeZone::TopLeft, EdgeZone::TopRight],
            FlowSide::Bottom => [
                EdgeZone::Bottom,
                EdgeZone::BottomLeft,
                EdgeZone::BottomRight,
            ],
        };
        for zone in zones {
            if !triggers
                .iter()
                .any(|existing: &ZoneTrigger| existing.zone == zone)
            {
                triggers.push(ZoneTrigger { zone, host });
            }
        }
    }
    triggers
}

/// A zone binding the cursor currently occupies. The identity `observe`
/// compares ticks by — sliding onto the same zone of a *different* display
/// restarts the dwell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OccupiedZone {
    display: u64,
    zone: EdgeZone,
    host: u8,
}

/// What one [`EdgeStateMachine::observe`] tick concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeObservation {
    /// Not inside any bound zone (re-arms the spent latch).
    Idle,
    /// Inside a bound zone that can still fire — a fire is imminent, so the
    /// watcher should start acquiring the exclusive receiver lease now.
    /// Never reported while spent: a spent zone cannot fire, and holding the
    /// receiver there would kill capture while the cursor merely rests on
    /// the edge.
    Pending {
        /// The host the imminent fire would switch to.
        host: u8,
    },
    /// Dwell and cooldown satisfied — perform the switch. The spent latch is
    /// now set until the cursor leaves every bound zone.
    Fire {
        /// Zero-based host slot to switch to.
        host: u8,
    },
}

/// Dwell/cooldown/re-arm state across `observe` ticks.
#[derive(Debug, Default)]
pub struct EdgeStateMachine {
    /// The bound zone the cursor was inside last tick, with when it entered.
    current: Option<(OccupiedZone, Instant)>,
    /// Set on fire; cleared only when the cursor leaves every bound zone, so
    /// dwelling on the edge after a switch cannot fire again by itself.
    spent: bool,
    /// When the machine last fired, for the cooldown debounce.
    last_fire: Option<Instant>,
}

impl EdgeStateMachine {
    /// Feed one cursor sample.
    ///
    /// A tick blocked only by the cooldown stays [`EdgeObservation::Pending`]
    /// and fires once the cooldown expires (the cursor is still being held
    /// against the edge, which is the same intent); a tick after a fire stays
    /// spent until the cursor leaves the zone.
    pub fn observe(
        &mut self,
        point: CursorPosition,
        displays: &[DisplayBounds],
        triggers: &[ZoneTrigger],
        dwell: Duration,
        cooldown: Duration,
        now: Instant,
    ) -> EdgeObservation {
        let Some(occupied) = bound_zone_at(point, displays, triggers) else {
            // Left every bound zone: re-arm.
            self.current = None;
            self.spent = false;
            return EdgeObservation::Idle;
        };
        let entered_at = match self.current {
            Some((zone, entered_at)) if zone == occupied => entered_at,
            // A new zone restarts the dwell. Moving straight from one bound
            // zone into another (corner → edge) keeps `spent`: only leaving
            // re-arms, so one push fires at most once.
            _ => {
                self.current = Some((occupied, now));
                now
            }
        };
        if self.spent {
            return EdgeObservation::Idle;
        }
        if now.duration_since(entered_at) < dwell
            || self
                .last_fire
                .is_some_and(|last| now.duration_since(last) < cooldown)
        {
            return EdgeObservation::Pending {
                host: occupied.host,
            };
        }
        self.spent = true;
        self.last_fire = Some(now);
        EdgeObservation::Fire {
            host: occupied.host,
        }
    }
}

/// The bound zone containing `point`, if any: the display under the cursor,
/// the zone classified in that display's local coordinates, and the first
/// trigger mapped to it.
fn bound_zone_at(
    point: CursorPosition,
    displays: &[DisplayBounds],
    triggers: &[ZoneTrigger],
) -> Option<OccupiedZone> {
    let display = displays
        .iter()
        .find(|display| display.contains(point.x, point.y))?;
    let zone = classify(
        point.x - display.origin.0,
        point.y - display.origin.1,
        display.size.0,
        display.size.1,
    )?;
    let host = triggers.iter().find(|trigger| trigger.zone == zone)?.host;
    Some(OccupiedZone {
        display: display.id,
        zone,
        host,
    })
}

/// Classify a display-local point into a zone. Corners first — a corner point
/// also satisfies both of its edge conditions.
fn classify(x: f64, y: f64, width: f64, height: f64) -> Option<EdgeZone> {
    let left = x < CORNER_DEPTH;
    let right = x >= width - CORNER_DEPTH;
    let top = y < CORNER_DEPTH;
    let bottom = y >= height - CORNER_DEPTH;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(EdgeZone::TopLeft),
        (_, true, true, _) => Some(EdgeZone::TopRight),
        (true, _, _, true) => Some(EdgeZone::BottomLeft),
        (_, true, _, true) => Some(EdgeZone::BottomRight),
        _ if y < EDGE_DEPTH => Some(EdgeZone::Top),
        _ if y >= height - EDGE_DEPTH => Some(EdgeZone::Bottom),
        _ if x < EDGE_DEPTH => Some(EdgeZone::Left),
        _ if x >= width - EDGE_DEPTH => Some(EdgeZone::Right),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DWELL: Duration = Duration::from_millis(50);
    const COOLDOWN: Duration = Duration::from_millis(500);

    fn display(id: u64, x: f64, y: f64, w: f64, h: f64) -> DisplayBounds {
        DisplayBounds {
            id,
            origin: (x, y),
            size: (w, h),
        }
    }

    fn single_display() -> Vec<DisplayBounds> {
        vec![display(1, 0.0, 0.0, 1920.0, 1080.0)]
    }

    fn right_to_host_1() -> Vec<ZoneTrigger> {
        let mut placements = FlowPlacements::default();
        placements.set(FlowSide::Right, Some(1));
        triggers_for(&placements)
    }

    fn at(x: f64, y: f64) -> CursorPosition {
        CursorPosition { x, y }
    }

    /// Drive one sample through a machine with the default tunables.
    fn tick(
        machine: &mut EdgeStateMachine,
        point: CursorPosition,
        now: Instant,
    ) -> EdgeObservation {
        machine.observe(
            point,
            &single_display(),
            &right_to_host_1(),
            DWELL,
            COOLDOWN,
            now,
        )
    }

    #[test]
    fn a_side_covers_its_edge_and_both_corners() {
        let mut placements = FlowPlacements::default();
        placements.set(FlowSide::Left, Some(2));
        let triggers = triggers_for(&placements);
        let zones: Vec<_> = triggers.iter().map(|trigger| trigger.zone).collect();
        assert_eq!(
            zones,
            [EdgeZone::Left, EdgeZone::TopLeft, EdgeZone::BottomLeft]
        );
        assert!(triggers.iter().all(|trigger| trigger.host == 2));
    }

    #[test]
    fn a_shared_corner_belongs_to_the_earlier_side_deterministically() {
        // Right and top are both mapped; top-right could be either. `ALL`
        // order (left, right, top, bottom) makes it right's.
        let mut placements = FlowPlacements::default();
        placements.set(FlowSide::Right, Some(1));
        placements.set(FlowSide::Top, Some(2));
        let triggers = triggers_for(&placements);
        let top_right = triggers
            .iter()
            .find(|trigger| trigger.zone == EdgeZone::TopRight)
            .expect("shared corner must stay bound");
        assert_eq!(top_right.host, 1);
        // Both sides keep their own edges.
        assert!(
            triggers
                .iter()
                .any(|trigger| trigger.zone == EdgeZone::Top && trigger.host == 2)
        );
        assert_eq!(triggers.len(), 5, "3 + 3 zones minus the shared corner");
    }

    #[test]
    fn pending_precedes_fire_through_the_dwell() {
        let mut machine = EdgeStateMachine::default();
        let start = Instant::now();
        assert_eq!(
            tick(&mut machine, at(1919.5, 500.0), start),
            EdgeObservation::Pending { host: 1 }
        );
        assert_eq!(
            tick(&mut machine, at(1919.5, 500.0), start + DWELL / 2),
            EdgeObservation::Pending { host: 1 }
        );
        assert_eq!(
            tick(&mut machine, at(1919.5, 500.0), start + DWELL),
            EdgeObservation::Fire { host: 1 }
        );
    }

    #[test]
    fn zero_dwell_fires_on_entry() {
        let mut machine = EdgeStateMachine::default();
        let now = Instant::now();
        let fired = machine.observe(
            at(1919.5, 500.0),
            &single_display(),
            &right_to_host_1(),
            Duration::ZERO,
            COOLDOWN,
            now,
        );
        assert_eq!(fired, EdgeObservation::Fire { host: 1 });
    }

    #[test]
    fn stays_spent_and_silent_until_the_cursor_leaves_the_zone() {
        let mut machine = EdgeStateMachine::default();
        let start = Instant::now();
        tick(&mut machine, at(1919.5, 500.0), start);
        assert_eq!(
            tick(&mut machine, at(1919.5, 500.0), start + DWELL),
            EdgeObservation::Fire { host: 1 }
        );
        // Dwelling on — far past every dwell and cooldown — must stay Idle,
        // not Pending: this is what stops a returned device from bouncing
        // straight back (and what keeps the receiver un-held) while the
        // cursor still sits on the edge.
        assert_eq!(
            tick(
                &mut machine,
                at(1919.5, 500.0),
                start + DWELL + COOLDOWN * 10
            ),
            EdgeObservation::Idle
        );
    }

    #[test]
    fn leaving_and_reentering_rearms_but_respects_the_cooldown() {
        let mut machine = EdgeStateMachine::default();
        let start = Instant::now();
        tick(&mut machine, at(1919.5, 500.0), start);
        assert_eq!(
            tick(&mut machine, at(1919.5, 500.0), start + DWELL),
            EdgeObservation::Fire { host: 1 }
        );
        // Leave, re-enter, dwell again — all inside the cooldown window.
        let reentry = start + DWELL + Duration::from_millis(100);
        assert_eq!(
            tick(&mut machine, at(900.0, 500.0), reentry),
            EdgeObservation::Idle
        );
        tick(&mut machine, at(1919.5, 500.0), reentry);
        assert_eq!(
            tick(&mut machine, at(1919.5, 500.0), reentry + DWELL),
            EdgeObservation::Pending { host: 1 },
            "cooldown must debounce a quick second push"
        );
        // Held against the edge past the cooldown: the intent stands; fire.
        let after_cooldown = start + DWELL + COOLDOWN;
        assert_eq!(
            tick(&mut machine, at(1919.5, 500.0), after_cooldown),
            EdgeObservation::Fire { host: 1 }
        );
    }

    #[test]
    fn unmapped_zones_are_idle() {
        let mut machine = EdgeStateMachine::default();
        let start = Instant::now();
        // The left edge has no trigger: idle, ever.
        assert_eq!(
            tick(&mut machine, at(0.0, 500.0), start),
            EdgeObservation::Idle
        );
        assert_eq!(
            tick(&mut machine, at(0.0, 500.0), start + DWELL * 5),
            EdgeObservation::Idle
        );
    }

    #[test]
    fn leaving_the_display_entirely_rearms() {
        let mut machine = EdgeStateMachine::default();
        let start = Instant::now();
        tick(&mut machine, at(1919.5, 500.0), start);
        assert_eq!(
            tick(&mut machine, at(1919.5, 500.0), start + DWELL),
            EdgeObservation::Fire { host: 1 }
        );
        // Off every display (mid-switch the sample can be anywhere).
        assert_eq!(
            tick(&mut machine, at(-5000.0, -5000.0), start + DWELL),
            EdgeObservation::Idle
        );
        let back = start + DWELL + COOLDOWN;
        tick(&mut machine, at(1919.5, 500.0), back);
        assert_eq!(
            tick(&mut machine, at(1919.5, 500.0), back + DWELL),
            EdgeObservation::Fire { host: 1 }
        );
    }

    #[test]
    fn corners_win_over_their_edges() {
        assert_eq!(
            classify(1919.5, 0.5, 1920.0, 1080.0),
            Some(EdgeZone::TopRight)
        );
        assert_eq!(
            classify(0.5, 1079.5, 1920.0, 1080.0),
            Some(EdgeZone::BottomLeft)
        );
        // Past the corner reach, the edge classifies as itself.
        assert_eq!(
            classify(1919.5, 500.0, 1920.0, 1080.0),
            Some(EdgeZone::Right)
        );
        assert_eq!(classify(500.0, 0.5, 1920.0, 1080.0), Some(EdgeZone::Top));
        assert_eq!(classify(500.0, 500.0, 1920.0, 1080.0), None);
    }

    #[test]
    fn a_corner_push_switches_like_its_edge() {
        // The user pushes into the top-right corner with only Right mapped:
        // the corner expansion is what fires.
        let mut machine = EdgeStateMachine::default();
        let start = Instant::now();
        tick(&mut machine, at(1919.5, 1.0), start);
        assert_eq!(
            tick(&mut machine, at(1919.5, 1.0), start + DWELL),
            EdgeObservation::Fire { host: 1 }
        );
    }

    #[test]
    fn switching_zones_without_leaving_does_not_refire() {
        let mut machine = EdgeStateMachine::default();
        let start = Instant::now();
        let displays = single_display();
        let triggers = right_to_host_1();
        machine.observe(
            at(1919.5, 500.0),
            &displays,
            &triggers,
            DWELL,
            COOLDOWN,
            start,
        );
        assert_eq!(
            machine.observe(
                at(1919.5, 500.0),
                &displays,
                &triggers,
                DWELL,
                COOLDOWN,
                start + DWELL
            ),
            EdgeObservation::Fire { host: 1 }
        );
        // Slide up the edge into the corner without ever leaving: spent.
        let later = start + DWELL + COOLDOWN * 2;
        machine.observe(
            at(1919.5, 1.0),
            &displays,
            &triggers,
            DWELL,
            COOLDOWN,
            later,
        );
        assert_eq!(
            machine.observe(
                at(1919.5, 1.0),
                &displays,
                &triggers,
                DWELL,
                COOLDOWN,
                later + DWELL
            ),
            EdgeObservation::Idle
        );
    }

    #[test]
    fn dwell_restarts_when_the_zone_changes() {
        let mut placements = FlowPlacements::default();
        placements.set(FlowSide::Right, Some(1));
        placements.set(FlowSide::Bottom, Some(2));
        let triggers = triggers_for(&placements);
        let mut machine = EdgeStateMachine::default();
        let start = Instant::now();
        let displays = single_display();
        machine.observe(
            at(1919.5, 500.0),
            &displays,
            &triggers,
            DWELL,
            COOLDOWN,
            start,
        );
        // Almost through the right-edge dwell, then jump to the bottom edge:
        // the bottom dwell starts fresh.
        let jump = start
            + DWELL
                .checked_sub(Duration::from_millis(10))
                .expect("DWELL > 10ms");
        machine.observe(
            at(900.0, 1079.5),
            &displays,
            &triggers,
            DWELL,
            COOLDOWN,
            jump,
        );
        assert_eq!(
            machine.observe(
                at(900.0, 1079.5),
                &displays,
                &triggers,
                DWELL,
                COOLDOWN,
                start + DWELL
            ),
            EdgeObservation::Pending { host: 2 },
            "the old zone's dwell must not carry over"
        );
        assert_eq!(
            machine.observe(
                at(900.0, 1079.5),
                &displays,
                &triggers,
                DWELL,
                COOLDOWN,
                jump + DWELL
            ),
            EdgeObservation::Fire { host: 2 }
        );
    }
}
