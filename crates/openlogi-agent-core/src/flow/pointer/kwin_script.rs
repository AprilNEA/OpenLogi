//! Flow's pointer thread on a KDE Wayland session.
//!
//! The session's cursor cannot be read, so there is nothing to poll and no
//! edge detector to feed. A KWin script (`openlogi-hook`'s `kwin-script/`)
//! tells the agent instead when the cursor pushes into a screen edge, with the
//! cursor position and the screen layout. This thread turns such a call into
//! the same [`EdgeCrossing`] the polling thread reports, when a peer is armed
//! beyond that edge.
//!
//! Nothing here captures input. The script cannot say which modifier keys are
//! held, so a layout that requires Control gets no crossings. KWin itself
//! limits how often an edge fires, in place of the polling thread's dwell and
//! cooldown.
//!
//! When a handoff arrives, the cursor is placed the way the polling thread
//! places it, at the entry point on the matching edge. A KWin script cannot
//! move the cursor, so [`KwinEdge`] does it with a virtual absolute pointer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;

use openlogi_flow::generated as proto;
use openlogi_hook::{
    DesktopPoint as HookPoint, DisplayBounds, KwinCrossing, KwinEdge, KwinSide, WarpError,
};
use tokio::sync::mpsc as async_mpsc;
use tracing::{debug, info, warn};

use super::{Command, PointerSettings, entry_position, exposed_edges};
use crate::flow::edge::{
    DesktopPoint, EdgeCrossing, EdgeSegment, EdgeSide, ExposedEdges, crossing_through,
};

/// What the thread needs from the session. [`KwinEdge`] provides it outside
/// tests.
pub(super) trait EdgeScript {
    /// The next queued crossing, if any.
    fn poll(&mut self) -> Option<KwinCrossing>;
    /// The screens as the compositor reports them now.
    fn displays(&mut self) -> Vec<DisplayBounds>;
    /// Put the cursor at `to`.
    fn warp(&mut self, to: HookPoint) -> Result<(), WarpError>;
}

impl EdgeScript for KwinEdge {
    fn poll(&mut self) -> Option<KwinCrossing> {
        Self::poll(self)
    }

    fn displays(&mut self) -> Vec<DisplayBounds> {
        Self::displays(self)
    }

    fn warp(&mut self, to: HookPoint) -> Result<(), WarpError> {
        Self::warp(self, to)
    }
}

/// Start listening for the script, waking the calling thread for every
/// crossing. `None` unless this is a KDE Wayland session.
pub(super) fn connect() -> Option<KwinEdge> {
    let pointer_thread = thread::current();
    let edge = KwinEdge::start(move || pointer_thread.unpark())?;
    info!("Flow is waiting for the OpenLogi KWin edge script to report screen edges");
    Some(edge)
}

pub(super) fn run<S: EdgeScript>(
    mut script: S,
    commands: &mpsc::Receiver<Command>,
    moved: &AtomicBool,
    crossings: &async_mpsc::Sender<EdgeCrossing>,
) {
    let mut state = ScriptLoop::default();
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Configure(settings)) => state.configure(settings),
                Ok(Command::Place(entry)) => place(&mut script, &entry),
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => break,
            }
        }
        // The hook's motion wakes carry nothing here: crossings come from
        // the script.
        moved.store(false, Ordering::Release);
        while let Some(call) = script.poll() {
            if let Some(crossing) = state.crossed(&call)
                && crossings.try_send(crossing).is_err()
            {
                debug!("the Flow runtime is busy; dropped an edge crossing");
            }
        }
        // A wake that lands between the last poll and the park leaves a
        // token, so the park returns at once.
        thread::park();
    }
}

/// Move the cursor to a handoff's entry point on this host's exposed edges.
fn place<S: EdgeScript>(script: &mut S, entry: &proto::EntryPoint) {
    let edges = exposed_edges(&script.displays());
    let Some(target) = entry_position(&edges, entry) else {
        warn!("no exposed display edge for the Flow entry point; the cursor stays");
        return;
    };
    match script.warp(HookPoint {
        x: target.x,
        y: target.y,
    }) {
        Ok(()) => debug!(x = target.x, y = target.y, "Flow placed the cursor"),
        Err(error) => warn!(%error, "Flow could not place the cursor"),
    }
}

/// The thread's state between wakes.
#[derive(Default)]
struct ScriptLoop {
    settings: PointerSettings,
    /// Whether the modifier limitation has been reported.
    warned_modifier: bool,
}

impl ScriptLoop {
    fn configure(&mut self, settings: PointerSettings) {
        self.settings = settings;
    }

    /// The crossing a call from the script means, if a peer is armed beyond
    /// that edge.
    fn crossed(&mut self, call: &KwinCrossing) -> Option<EdgeCrossing> {
        let side = edge_side(call.side);
        if !self.settings.sides.contains(side) {
            debug!(
                ?side,
                "the cursor pushed into an edge with no peer; ignored"
            );
            return None;
        }
        if self.settings.require_modifier {
            if !self.warned_modifier {
                self.warned_modifier = true;
                warn!(
                    "Flow edge handoff cannot require Control on a Wayland session; no edge crosses"
                );
            }
            return None;
        }
        let edges = exposed_edges(&call.displays);
        let position = DesktopPoint {
            x: call.position.x,
            y: call.position.y,
        };
        let index = nearest_segment(&edges, side, position)?;
        crossing_through(&edges, index, position)
    }
}

fn edge_side(side: KwinSide) -> EdgeSide {
    match side {
        KwinSide::Left => EdgeSide::Left,
        KwinSide::Right => EdgeSide::Right,
        KwinSide::Top => EdgeSide::Top,
        KwinSide::Bottom => EdgeSide::Bottom,
    }
}

/// The index in [`ExposedEdges::segments`] of the segment on `side` that lies
/// closest to `position` along the edge. `None` when the layout exposes no
/// such side.
fn nearest_segment(edges: &ExposedEdges, side: EdgeSide, position: DesktopPoint) -> Option<usize> {
    edges
        .segments()
        .iter()
        .enumerate()
        .filter(|(_, segment)| segment.side() == side)
        .min_by(|(_, left), (_, right)| gap(**left, position).total_cmp(&gap(**right, position)))
        .map(|(index, _)| index)
}

/// How far `position` lies from `segment` along its edge; zero within it.
fn gap(segment: EdgeSegment, position: DesktopPoint) -> f64 {
    let along = segment.along(position);
    (along.clamp(segment.start(), segment.end()) - along).abs()
}

#[cfg(test)]
mod tests;
