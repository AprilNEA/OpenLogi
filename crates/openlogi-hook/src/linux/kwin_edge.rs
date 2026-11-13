//! Flow's edge source on a KDE Wayland session, through a companion KWin
//! script.
//!
//! A Wayland client cannot read the global cursor, so the polling probe in
//! [`super::probe`] refuses such a session. KWin can tell when the cursor
//! pushes into a screen edge, and a small script in this crate's
//! `kwin-script/` directory forwards that to the agent. This module is the
//! agent's end: it owns the `org.openlogi.Edge` name on the session bus and
//! turns each `Crossed` call into a [`KwinCrossing`].
//!
//! Nothing here captures, moves or blocks input. The script only reports,
//! so a fault on either side cannot take the keyboard or mouse away. With the
//! script not installed no call ever arrives and Flow has no outgoing edge.
//!
//! The service never blocks the caller: events queue on a channel and a wake
//! function tells the owner to look.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use tracing::{debug, warn};
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::interface;

use super::foreground::{SessionKind, detect_session_kind};
use super::outputs;
use super::placement::Placement;
use crate::{DesktopPoint, DisplayBounds, WarpError};

const NAME: &str = "org.openlogi.Edge";
const PATH: &str = "/org/openlogi/Edge";

/// Unread crossings a slow consumer may leave queued. The script fires about
/// once a second, so this is never reached in practice.
const QUEUE: usize = 16;

/// A screen side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KwinSide {
    /// The minimum-x side.
    Left,
    /// The maximum-x side.
    Right,
    /// The minimum-y side.
    Top,
    /// The maximum-y side.
    Bottom,
}

/// The cursor pushed into a screen edge.
#[derive(Clone, Debug, PartialEq)]
pub struct KwinCrossing {
    /// The edge it pushed into.
    pub side: KwinSide,
    /// Where the cursor was, in KWin's logical coordinates.
    pub position: DesktopPoint,
    /// Every screen, in the same coordinate space.
    pub displays: Vec<DisplayBounds>,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

/// The D-Bus object the script calls.
struct Service {
    events: mpsc::SyncSender<KwinCrossing>,
    wake: Wake,
}

#[interface(name = "org.openlogi.Edge")]
impl Service {
    /// The cursor pushed into an edge. All arguments are strings; see the
    /// script for the format. A call that does not parse is dropped.
    fn crossed(&self, side: &str, position: &str, displays: &str) {
        let (Some(side), Some(position)) = (parse_side(side), parse_point(position)) else {
            debug!(side, position, "ignoring a malformed Flow edge call");
            return;
        };
        let crossing = KwinCrossing {
            side,
            position,
            displays: parse_displays(displays),
        };
        if self.events.try_send(crossing).is_ok() {
            (self.wake)();
        } else {
            debug!("the Flow edge queue is full; dropped a crossing");
        }
    }
}

/// The agent's end of the KWin script. Dropping it gives the bus name back and
/// removes the cursor placement device.
pub struct KwinEdge {
    _connection: Connection,
    events: Mutex<mpsc::Receiver<KwinCrossing>>,
    /// `None` when the device could not be created, as without access to
    /// `/dev/uinput`. Edge crossings still work; the cursor is just not placed.
    placement: Mutex<Option<Placement>>,
}

impl KwinEdge {
    /// Take the `org.openlogi.Edge` name. `wake` runs on a bus thread after
    /// each crossing is queued, so it must not block.
    ///
    /// `None` unless this is a KDE session on Wayland, or when the name is
    /// already taken, as by a second agent.
    #[must_use]
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        if detect_session_kind() != SessionKind::Wayland || !is_kde() {
            debug!("the KWin edge script is only used on a KDE Wayland session");
            return None;
        }
        let (events, receiver) = mpsc::sync_channel(QUEUE);
        let service = Service {
            events,
            wake: Arc::new(wake),
        };
        let connection = Builder::session()
            .and_then(|builder| builder.name(NAME))
            .and_then(|builder| builder.serve_at(PATH, service))
            .and_then(Builder::build)
            .map_err(|error| warn!(%error, "could not serve {NAME} for the KWin edge script"))
            .ok()?;
        // Created now, not per placement: a position sent before the
        // compositor has picked up a new device is lost.
        let placement = Placement::create()
            .map_err(|error| warn!(%error, "Flow cannot create its cursor placement device"))
            .ok();
        Some(Self {
            _connection: connection,
            events: Mutex::new(receiver),
            placement: Mutex::new(placement),
        })
    }

    /// The next queued crossing, if any.
    #[must_use]
    pub fn poll(&self) -> Option<KwinCrossing> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_recv()
            .ok()
    }

    /// The screens as the compositor reports them now, in the same logical
    /// space as a crossing's position. Empty when the compositor does not
    /// say.
    #[must_use]
    pub fn displays(&self) -> Vec<DisplayBounds> {
        outputs::logical_displays().unwrap_or_default()
    }

    /// Put the cursor at `to`.
    ///
    /// # Errors
    ///
    /// [`WarpError::OutsideDisplays`] when no screen contains `to`, and
    /// [`WarpError::Rejected`] when the screen layout is unknown, the
    /// placement device does not exist, or writing to it fails.
    pub fn warp(&self, to: DesktopPoint) -> Result<(), WarpError> {
        let displays = self.displays();
        if displays.is_empty() {
            return Err(rejected("the screen layout is unknown"));
        }
        self.placement
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .ok_or_else(|| rejected("there is no cursor placement device"))?
            .move_to(to, &displays)
    }
}

fn rejected(reason: &str) -> WarpError {
    WarpError::Rejected(std::io::Error::other(reason.to_owned()))
}

/// Whether `XDG_CURRENT_DESKTOP` names KDE, as in `KDE` or `KDE:Plasma`.
fn is_kde() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktops| {
        desktops
            .split(':')
            .any(|desktop| desktop.eq_ignore_ascii_case("kde"))
    })
}

fn parse_side(text: &str) -> Option<KwinSide> {
    match text {
        "left" => Some(KwinSide::Left),
        "right" => Some(KwinSide::Right),
        "top" => Some(KwinSide::Top),
        "bottom" => Some(KwinSide::Bottom),
        _ => None,
    }
}

/// `"x,y"`, both finite.
fn parse_point(text: &str) -> Option<DesktopPoint> {
    let (x, y) = text.split_once(',')?;
    let (x, y) = (x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?);
    (x.is_finite() && y.is_finite()).then_some(DesktopPoint { x, y })
}

/// `"x,y,w,h;x,y,w,h;…"`. An entry that does not parse is skipped.
fn parse_displays(text: &str) -> Vec<DisplayBounds> {
    text.split(';')
        .filter_map(|entry| {
            let mut numbers = entry.split(',').map(|part| part.trim().parse::<f64>());
            let (x, y, width, height) = (
                numbers.next()?.ok()?,
                numbers.next()?.ok()?,
                numbers.next()?.ok()?,
                numbers.next()?.ok()?,
            );
            let finite = [x, y, width, height].iter().all(|value| value.is_finite());
            (numbers.next().is_none() && finite).then_some(DisplayBounds {
                x,
                y,
                width,
                height,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
