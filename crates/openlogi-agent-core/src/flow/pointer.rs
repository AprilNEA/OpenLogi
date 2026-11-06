//! Flow's pointer thread: the one place that reads and moves the cursor.
//!
//! The OS hook callback must never wait, so it only marks the pointer as
//! moved and wakes this thread through a [`PointerWaker`]. The thread then
//! reads one pointer state per wake, feeds the edge detector, and reports each
//! crossing to the Flow runtime without waiting for it. The hook reports
//! motion before the desktop applies it, so the thread reads once more
//! [`MOTION_SETTLE`] after the last motion. A cursor resting on an edge sends
//! no motion, so the thread also wakes itself when a pending dwell is due.
//! Every
//! [`GEOMETRY_REFRESH`] it reads the display layout again and replaces the
//! edges only when the layout changed. It also places the cursor when a
//! handoff arrives, so one thread reads and moves the cursor in one coordinate
//! space.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

use openlogi_flow::generated as proto;
use openlogi_hook::{
    DesktopPoint as HookPoint, DisplayBounds, PointerProbe, PointerState, WarpError,
};
use tokio::sync::mpsc as async_mpsc;
use tracing::{debug, warn};

use super::edge::{
    ArmedSides, DesktopPoint, DisplayRect, EdgeCrossing, EdgeDetector, EdgeDetectorParams,
    EdgeSide, ExposedEdges,
};

/// How often the thread reads the display layout again.
pub(crate) const GEOMETRY_REFRESH: Duration = Duration::from_secs(2);
/// How long after the last motion the thread reads the pointer once more.
/// The hook reports motion before the desktop applies it, and on Linux before
/// it sends the movement back through uinput, so the read a motion wakes can
/// see the old position. This read sees where the cursor stopped.
const MOTION_SETTLE: Duration = Duration::from_millis(50);
/// How far inside the display an arriving cursor lands. It exceeds the
/// detector's 1 px contact tolerance, so an arrival cannot cross straight back.
const ENTRY_INSET: f64 = 2.0;

/// What the pointer thread watches for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PointerSettings {
    /// The sides with a peer beyond them. [`ArmedSides::NONE`] stops detection.
    pub(crate) sides: ArmedSides,
    /// Whether Control must be held for a crossing.
    pub(crate) require_modifier: bool,
}

enum Command {
    Configure(PointerSettings),
    Place(proto::EntryPoint),
    Stop,
}

/// The pointer operations the thread needs. [`PointerProbe`] provides them
/// outside tests.
pub(crate) trait Pointer {
    /// Read the cursor and the held modifiers.
    fn state(&mut self) -> Option<PointerState>;
    /// Read the active displays.
    fn displays(&mut self) -> Vec<DisplayBounds>;
    /// Move the cursor.
    fn warp(&mut self, to: HookPoint) -> Result<(), WarpError>;
}

impl Pointer for PointerProbe {
    fn state(&mut self) -> Option<PointerState> {
        Self::state(self)
    }

    fn displays(&mut self) -> Vec<DisplayBounds> {
        Self::displays(self)
    }

    fn warp(&mut self, to: HookPoint) -> Result<(), WarpError> {
        Self::warp(self, to)
    }
}

/// Wakes the pointer thread after cursor motion. It never blocks, so the OS
/// hook callback may call it.
#[derive(Clone, Debug)]
pub(crate) struct PointerWaker {
    moved: Arc<AtomicBool>,
    thread: Thread,
}

impl PointerWaker {
    /// Record that the cursor moved.
    pub(crate) fn moved(&self) {
        // Only the first motion since the last sample needs to wake the
        // thread; later ones fold into the same sample.
        if !self.moved.swap(true, Ordering::AcqRel) {
            self.thread.unpark();
        }
    }
}

/// The running pointer thread. Dropping the handle asks the thread to stop
/// without waiting for it; [`Self::stop`] also waits.
pub(crate) struct PointerThread {
    commands: mpsc::Sender<Command>,
    waker: PointerWaker,
    join: Option<JoinHandle<()>>,
}

impl PointerThread {
    /// Start the thread on this session's pointer. Crossings go to
    /// `crossings`; one that does not fit is dropped.
    pub(crate) fn spawn(crossings: async_mpsc::Sender<EdgeCrossing>) -> std::io::Result<Self> {
        Self::spawn_with(PointerProbe::connect, crossings)
    }

    /// Start the thread on this session's pointer, or with `probe` off on
    /// no pointer at all, as in a session that has no probe. Such a thread
    /// exits at once and drops `crossings`.
    pub(crate) fn start(
        probe: bool,
        crossings: async_mpsc::Sender<EdgeCrossing>,
    ) -> std::io::Result<Self> {
        if probe {
            Self::spawn(crossings)
        } else {
            Self::spawn_with(|| None::<PointerProbe>, crossings)
        }
    }

    /// Start the thread on the pointer `connect` opens on the new thread.
    fn spawn_with<P, F>(
        connect: F,
        crossings: async_mpsc::Sender<EdgeCrossing>,
    ) -> std::io::Result<Self>
    where
        P: Pointer,
        F: FnOnce() -> Option<P> + Send + 'static,
    {
        let (commands, receiver) = mpsc::channel();
        let moved = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&moved);
        let join = thread::Builder::new()
            .name("openlogi-flow-pointer".to_owned())
            .spawn(move || {
                let Some(pointer) = connect() else {
                    warn!("Flow cannot read the pointer in this session; edge handoff is off");
                    return;
                };
                run(pointer, &receiver, &flag, &crossings);
            })?;
        let waker = PointerWaker {
            moved,
            thread: join.thread().clone(),
        };
        Ok(Self {
            commands,
            waker,
            join: Some(join),
        })
    }

    /// The waker the OS hook callback calls on motion.
    pub(crate) fn waker(&self) -> PointerWaker {
        self.waker.clone()
    }

    /// Replace what the thread watches for.
    pub(crate) fn configure(&self, settings: PointerSettings) {
        self.send(Command::Configure(settings));
    }

    /// Place the cursor at a handoff's entry point.
    pub(crate) fn place(&self, entry: proto::EntryPoint) {
        self.send(Command::Place(entry));
    }

    /// Stop the thread and wait for it. Blocks for at most one pointer query.
    pub(crate) fn stop(mut self) {
        self.send(Command::Stop);
        if let Some(join) = self.join.take()
            && join.join().is_err()
        {
            warn!("Flow pointer thread panicked");
        }
    }

    fn send(&self, command: Command) {
        // A thread that could not open the pointer has exited; its commands
        // have nowhere to go.
        let _ = self.commands.send(command);
        self.waker.thread.unpark();
    }
}

impl Drop for PointerThread {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.send(Command::Stop);
        }
    }
}

fn run<P: Pointer>(
    pointer: P,
    commands: &mpsc::Receiver<Command>,
    moved: &AtomicBool,
    crossings: &async_mpsc::Sender<EdgeCrossing>,
) {
    let epoch = Instant::now();
    let mut state = PointerLoop::new(pointer, Duration::ZERO);
    let mut settle_at = None;
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Configure(settings)) => state.configure(settings),
                Ok(Command::Place(entry)) => state.place(&entry),
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => break,
            }
        }
        let now = epoch.elapsed();
        state.refresh(now);
        let motion = moved.swap(false, Ordering::AcqRel);
        let settled = settle_at.is_some_and(|due| due <= now);
        // Each motion pushes the follow-up read back, so a burst of motion
        // ends with one read after the desktop has caught up.
        if motion {
            settle_at = Some(now + MOTION_SETTLE);
        } else if settled {
            settle_at = None;
        }
        // A cursor resting on an edge sends no motion, so a due dwell is
        // sampled without one.
        let dwell_due = state.dwell_deadline().is_some_and(|due| due <= now);
        if (motion || settled || dwell_due)
            && let Some(crossing) = state.sample(now)
            && crossings.try_send(crossing).is_err()
        {
            debug!("the Flow runtime is busy; dropped an edge crossing");
        }
        // A wake that lands between the swap and the park leaves a token, so
        // the park returns at once.
        let wake_at = [state.dwell_deadline(), settle_at]
            .into_iter()
            .flatten()
            .fold(state.next_refresh(), Duration::min);
        thread::park_timeout(wake_at.saturating_sub(epoch.elapsed()));
    }
}

/// The pointer thread's state between wakes. Time is a duration from the
/// thread's own epoch, passed in so tests control it.
struct PointerLoop<P> {
    pointer: P,
    params: EdgeDetectorParams,
    settings: PointerSettings,
    edges: ExposedEdges,
    detector: EdgeDetector,
    gate_open: bool,
    next_refresh: Duration,
}

impl<P: Pointer> PointerLoop<P> {
    fn new(mut pointer: P, now: Duration) -> Self {
        let edges = exposed_edges(&pointer.displays());
        let params = EdgeDetectorParams::default();
        Self {
            detector: EdgeDetector::new(edges.clone(), ArmedSides::NONE, params),
            pointer,
            params,
            settings: PointerSettings::default(),
            edges,
            gate_open: true,
            next_refresh: now + GEOMETRY_REFRESH,
        }
    }

    fn configure(&mut self, settings: PointerSettings) {
        if settings.sides != self.settings.sides {
            self.detector = EdgeDetector::new(self.edges.clone(), settings.sides, self.params);
        }
        self.settings = settings;
    }

    const fn next_refresh(&self) -> Duration {
        self.next_refresh
    }

    /// When the detector's contact in progress is next due a sample.
    fn dwell_deadline(&self) -> Option<Duration> {
        self.detector.dwell_deadline()
    }

    /// Read the display layout again when the refresh is due.
    fn refresh(&mut self, now: Duration) {
        if now < self.next_refresh {
            return;
        }
        self.next_refresh = now + GEOMETRY_REFRESH;
        let edges = exposed_edges(&self.pointer.displays());
        // No displays means the window system did not answer; keep the last
        // layout rather than stop detection.
        if edges.segments().is_empty() || edges == self.edges {
            return;
        }
        self.edges = edges;
        self.detector.set_edges(self.edges.clone());
    }

    /// Read the pointer once and feed the detector.
    fn sample(&mut self, now: Duration) -> Option<EdgeCrossing> {
        if self.settings.sides == ArmedSides::NONE {
            return None;
        }
        let Some(state) = self.pointer.state() else {
            // Without a position the approach cannot continue, and a pending
            // dwell must not keep the thread waking for it.
            self.detector.set_edges(self.edges.clone());
            return None;
        };
        let open = !self.settings.require_modifier || state.modifiers.control;
        let was_open = std::mem::replace(&mut self.gate_open, open);
        if !open {
            if was_open {
                // Forget an approach made with Control held, so releasing and
                // pressing it again cannot complete a stale dwell.
                self.detector.set_edges(self.edges.clone());
            }
            return None;
        }
        self.detector.update(
            DesktopPoint {
                x: state.position.x,
                y: state.position.y,
            },
            now,
        )
    }

    /// Move the cursor to a handoff's entry point.
    fn place(&mut self, entry: &proto::EntryPoint) {
        let Some(target) = entry_position(&self.edges, entry) else {
            warn!("no exposed display edge for the Flow entry point; the cursor stays");
            return;
        };
        match self.pointer.warp(HookPoint {
            x: target.x,
            y: target.y,
        }) {
            // The jump is not motion: start the next approach from here.
            Ok(()) => self.detector.set_edges(self.edges.clone()),
            Err(error) => warn!(%error, "Flow could not place the cursor"),
        }
    }
}

fn exposed_edges(displays: &[DisplayBounds]) -> ExposedEdges {
    let rects: Vec<_> = displays
        .iter()
        .filter_map(|display| DisplayRect::new(display.x, display.y, display.width, display.height))
        .collect();
    ExposedEdges::from_displays(&rects)
}

/// Where an entry point lands on this host's exposed edges.
///
/// `t` is mapped with the detector's rule: a proportion of the side's
/// cumulative exposed length, in [`ExposedEdges::for_side`] order. The point
/// lands [`ENTRY_INSET`] inside the display and at least as far from either
/// end of its segment, so it touches no edge: at a corner, the neighboring
/// edge could otherwise start a dwell toward another peer.
fn entry_position(edges: &ExposedEdges, entry: &proto::EntryPoint) -> Option<DesktopPoint> {
    let side = match entry.side.as_known()? {
        proto::Side::Left => EdgeSide::Left,
        proto::Side::Right => EdgeSide::Right,
        proto::Side::Top => EdgeSide::Top,
        proto::Side::Bottom => EdgeSide::Bottom,
        proto::Side::Unspecified => return None,
    };
    if !entry.t.is_finite() {
        return None;
    }
    let segments: Vec<_> = edges.for_side(side).copied().collect();
    let total: f64 = segments
        .iter()
        .map(|segment| segment.end() - segment.start())
        .sum();
    if total <= 0.0 {
        return None;
    }
    let mut remaining = entry.t.clamp(0.0, 1.0) * total;
    let mut chosen = *segments.last()?;
    let mut offset = chosen.end() - chosen.start();
    for segment in &segments {
        let length = segment.end() - segment.start();
        if remaining <= length {
            chosen = *segment;
            offset = remaining;
            break;
        }
        remaining -= length;
    }
    let along = if chosen.end() - chosen.start() > 2.0 * ENTRY_INSET {
        (chosen.start() + offset).clamp(chosen.start() + ENTRY_INSET, chosen.end() - ENTRY_INSET)
    } else {
        f64::midpoint(chosen.start(), chosen.end())
    };
    let across = match side {
        EdgeSide::Left | EdgeSide::Top => chosen.coordinate() + ENTRY_INSET,
        EdgeSide::Right | EdgeSide::Bottom => chosen.coordinate() - ENTRY_INSET,
    };
    Some(match side {
        EdgeSide::Left | EdgeSide::Right => DesktopPoint {
            x: across,
            y: along,
        },
        EdgeSide::Top | EdgeSide::Bottom => DesktopPoint {
            x: along,
            y: across,
        },
    })
}

#[cfg(test)]
mod tests;
