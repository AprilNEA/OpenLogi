//! Worker ownership and non-blocking producer capability for smooth scrolling.

use std::collections::HashSet;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use openlogi_core::scroll::ScrollDelta;
use tracing::warn;

use super::{ScrollEngine, ScrollFrame, ScrollSource, WheelDelta};
use crate::runtime::HidppSessionId;

/// OS-hook callbacks must fail open rather than wait for the worker.
const INPUT_QUEUE_CAPACITY: usize = 128;
/// Bounds graceful process shutdown if platform injection stops returning.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

struct ScrollInput {
    generation: u64,
    source: ScrollSource,
    impulse: WheelDelta,
    at: Instant,
}

enum ScrollCommand {
    Input(ScrollInput),
    CancelSource(ScrollSource),
    Wake,
}

struct ShutdownRequest {
    done: mpsc::SyncSender<()>,
}

/// Lossless fallback for overflow cancellation and graceful shutdown.
enum ScrollControl {
    CancelOverflowSource(ScrollSource),
    Shutdown(ShutdownRequest),
}

/// Cloneable, non-owning capability for physical input producers.
///
/// Submission is always non-blocking. `false` asks each producer to use its
/// source-appropriate direct-output fallback.
#[derive(Clone)]
pub struct ScrollInputHandle {
    commands: mpsc::SyncSender<ScrollCommand>,
    controls: mpsc::Sender<ScrollControl>,
    generation: Arc<AtomicU64>,
    accepting: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
}

impl ScrollInputHandle {
    /// Queue one ordinary wheel impulse from the current OS-hook thread.
    ///
    /// Pixel input, zero/non-finite distance, a disabled preference, a full
    /// queue, or an unavailable worker are rejected so the callback fails open.
    #[must_use]
    pub fn try_hook_scroll(&self, delta: ScrollDelta) -> bool {
        self.try_scroll(ScrollSource::current_hook(), delta)
    }

    /// Queue one diverted thumb-wheel impulse from an active HID++ session.
    ///
    /// Rejection tells the already-diverted caller to inject the distance
    /// directly; unlike an OS hook, there is no physical event to pass through.
    #[must_use]
    pub(crate) fn try_hidpp_scroll(&self, session: &HidppSessionId, delta: ScrollDelta) -> bool {
        self.try_scroll(ScrollSource::Hidpp(session.clone()), delta)
    }

    fn try_scroll(&self, source: ScrollSource, delta: ScrollDelta) -> bool {
        if !self.accepting.load(Ordering::Acquire) || !self.enabled.load(Ordering::Relaxed) {
            return false;
        }
        let Ok(impulse) = WheelDelta::try_from(delta) else {
            return false;
        };
        let input = ScrollInput {
            generation: self.generation.load(Ordering::Acquire),
            source,
            impulse,
            at: Instant::now(),
        };
        match self.commands.try_send(ScrollCommand::Input(input)) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => {
                warn!("smooth-scroll queue full — input rejected");
                false
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                warn!("smooth-scroll worker unavailable — input rejected");
                false
            }
        }
    }

    /// Invalidate every accepted OS-hook animation without blocking.
    pub fn cancel_hooks(&self) {
        self.cancel_all();
    }

    /// Cancel output belonging to one HID++ capture-session incarnation.
    pub(crate) fn cancel_hidpp_session(&self, session: &HidppSessionId) {
        let source = ScrollSource::Hidpp(session.clone());
        match self
            .commands
            .try_send(ScrollCommand::CancelSource(source.clone()))
        {
            Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                if self
                    .controls
                    .send(ScrollControl::CancelOverflowSource(source))
                    .is_ok()
                {
                    self.wake();
                }
            }
        }
    }

    fn cancel_all(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.wake();
    }

    fn wake(&self) {
        let _ = self.commands.try_send(ScrollCommand::Wake);
    }

    fn stop_accepting(&self) {
        self.accepting.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}

/// Unique owner of the smooth-scroll worker and its graceful shutdown.
pub struct ScrollRuntime {
    input: ScrollInputHandle,
    controls: mpsc::Sender<ScrollControl>,
    worker: Option<JoinHandle<()>>,
}

impl ScrollRuntime {
    /// Start the dedicated animation worker using the live opt-in setting.
    pub fn spawn(enabled: Arc<AtomicBool>) -> io::Result<Self> {
        Self::spawn_with(enabled, ScrollFrame::post)
    }

    pub(super) fn spawn_with(
        enabled: Arc<AtomicBool>,
        mut emit: impl FnMut(ScrollFrame) + Send + 'static,
    ) -> io::Result<Self> {
        let (commands, command_rx) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        let (controls, control_rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let input = ScrollInputHandle {
            commands,
            controls: controls.clone(),
            generation: Arc::clone(&generation),
            accepting: Arc::new(AtomicBool::new(true)),
            enabled: Arc::clone(&enabled),
        };
        let worker = thread::Builder::new()
            .name("openlogi-scroll".into())
            .spawn(move || {
                run_worker(&command_rx, &control_rx, &generation, &enabled, &mut emit);
            })?;
        Ok(Self {
            input,
            controls,
            worker: Some(worker),
        })
    }

    /// Clone the non-owning input capability for physical input producers.
    #[must_use]
    pub fn input(&self) -> ScrollInputHandle {
        self.input.clone()
    }

    /// Reject new input, cancel active output, and join the worker.
    pub fn shutdown(&mut self) {
        let _ = self.shutdown_with_timeout(SHUTDOWN_TIMEOUT);
    }

    fn shutdown_with_timeout(&mut self, timeout: Duration) -> bool {
        let Some(worker) = self.worker.take() else {
            return true;
        };
        self.input.stop_accepting();
        let (done, wait) = mpsc::sync_channel(0);
        if self
            .controls
            .send(ScrollControl::Shutdown(ShutdownRequest { done }))
            .is_err()
        {
            let _ = worker.join();
            return false;
        }
        // Send the wake only after the shutdown request is visible. Otherwise
        // an idle worker could consume it first and block again on `commands`.
        self.input.wake();
        if wait.recv_timeout(timeout).is_err() {
            warn!("smooth-scroll worker did not shut down before the deadline");
            return false;
        }
        if worker.join().is_err() {
            warn!("smooth-scroll worker panicked during shutdown");
            return false;
        }
        true
    }
}

impl Drop for ScrollRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_worker(
    commands: &mpsc::Receiver<ScrollCommand>,
    controls: &mpsc::Receiver<ScrollControl>,
    shared_generation: &AtomicU64,
    enabled: &AtomicBool,
    emit: &mut impl FnMut(ScrollFrame),
) {
    let mut engine = ScrollEngine::default();
    // An overflow-cancelled incarnation stays tombstoned so accepted input that
    // was already queued when control overtook the saturated queue is ignored.
    let mut overflow_cancelled_sources = HashSet::new();
    let mut generation = shared_generation.load(Ordering::Acquire);
    loop {
        while let Ok(control) = controls.try_recv() {
            match control {
                ScrollControl::CancelOverflowSource(source) => {
                    engine.cancel_source(&source, emit);
                    overflow_cancelled_sources.insert(source);
                }
                ScrollControl::Shutdown(request) => {
                    engine.cancel_all(emit);
                    let _ = request.done.send(());
                    return;
                }
            }
        }

        let current_generation = shared_generation.load(Ordering::Acquire);
        if current_generation != generation || !enabled.load(Ordering::Relaxed) {
            engine.cancel_all(emit);
            generation = current_generation;
        }

        let command = engine.next_deadline().map_or_else(
            || {
                commands
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            },
            |deadline| commands.recv_timeout(deadline.saturating_duration_since(Instant::now())),
        );
        match command {
            Ok(ScrollCommand::Input(input))
                if input.generation == generation
                    && enabled.load(Ordering::Relaxed)
                    && !overflow_cancelled_sources.contains(&input.source) =>
            {
                engine.impulse(input.source, input.impulse, input.at, emit);
            }
            Ok(ScrollCommand::CancelSource(source)) => engine.cancel_source(&source, emit),
            Ok(ScrollCommand::Input(_) | ScrollCommand::Wake) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => engine.advance_due(Instant::now(), emit),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                engine.cancel_all(emit);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standalone_input(
        capacity: usize,
        enabled: bool,
    ) -> (
        ScrollInputHandle,
        mpsc::Receiver<ScrollCommand>,
        mpsc::Receiver<ScrollControl>,
    ) {
        let (commands, receiver) = mpsc::sync_channel(capacity);
        let (controls, control_rx) = mpsc::channel();
        (
            ScrollInputHandle {
                commands,
                controls,
                generation: Arc::new(AtomicU64::new(0)),
                accepting: Arc::new(AtomicBool::new(true)),
                enabled: Arc::new(AtomicBool::new(enabled)),
            },
            receiver,
            control_rx,
        )
    }

    #[test]
    fn callback_submission_rejects_ineligible_input_and_fails_open_when_full() {
        let (disabled, _commands, _controls) = standalone_input(1, false);
        assert!(!disabled.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));

        let (input, _commands, _controls) = standalone_input(1, true);
        assert!(!input.try_hook_scroll(ScrollDelta::pixels(0.0, 1.0)));
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
        assert!(!input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
    }

    #[test]
    fn hidpp_cancellation_targets_its_session() {
        let (input, commands, _controls) = standalone_input(1, true);
        let session = HidppSessionId::new("mouse-a", 7);
        input.cancel_hidpp_session(&session);

        let ScrollCommand::CancelSource(ScrollSource::Hidpp(cancelled)) =
            commands.recv().expect("queued cancellation")
        else {
            panic!("expected HID++ source cancellation");
        };
        assert_eq!(cancelled, session);
        assert_eq!(input.generation.load(Ordering::Acquire), 0);
    }

    #[test]
    fn cancellation_overtakes_a_full_queue_without_discarding_another_source() {
        let (input, commands, controls) = standalone_input(2, true);
        let cancelled = HidppSessionId::new("mouse-a", 7);
        let survivor = HidppSessionId::new("mouse-b", 3);
        assert!(input.try_hidpp_scroll(&cancelled, ScrollDelta::wheel_ticks(1.0, 0.0)));
        assert!(input.try_hidpp_scroll(&survivor, ScrollDelta::wheel_ticks(0.0, 1.0)));

        input.cancel_hidpp_session(&cancelled);
        assert_eq!(
            input.generation.load(Ordering::Acquire),
            0,
            "source-local cancellation must not invalidate unrelated accepted input"
        );

        let generation = Arc::clone(&input.generation);
        let enabled = Arc::clone(&input.enabled);
        let (emitted, frames) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_worker(&commands, &controls, &generation, &enabled, &mut |frame| {
                emitted.send(frame).expect("frame receiver remains open");
            });
        });

        let mut output = Vec::new();
        loop {
            let frame = frames
                .recv_timeout(Duration::from_secs(1))
                .expect("surviving source completes");
            output.push(frame);
            if frame.phase == openlogi_inject::SmoothScrollPhase::Ended {
                break;
            }
        }

        let (done, wait) = mpsc::sync_channel(0);
        input
            .controls
            .send(ScrollControl::Shutdown(ShutdownRequest { done }))
            .expect("worker control channel remains open");
        input.wake();
        wait.recv_timeout(Duration::from_secs(1))
            .expect("worker acknowledges shutdown");
        worker.join().expect("worker exits cleanly");

        let total = output
            .iter()
            .fold(WheelDelta::ZERO, |sum, frame| sum.plus(frame.delta));
        assert!(total.x.abs() < f64::EPSILON, "cancelled source emitted");
        assert!((total.y - 1.0).abs() < f64::EPSILON);
        assert!(
            output
                .iter()
                .all(|frame| frame.phase != openlogi_inject::SmoothScrollPhase::Cancelled)
        );
    }

    #[test]
    fn idle_worker_shutdown_wakes_after_publishing_its_request() {
        let mut runtime = ScrollRuntime::spawn_with(Arc::new(AtomicBool::new(false)), |_| {})
            .expect("spawn scroll worker");
        assert!(runtime.shutdown_with_timeout(Duration::from_millis(100)));
    }

    #[test]
    fn generation_invalidation_cancels_started_output_out_of_band() {
        let enabled = Arc::new(AtomicBool::new(true));
        let (frames, received) = mpsc::channel();
        let mut runtime = ScrollRuntime::spawn_with(enabled, move |frame| {
            frames
                .send(frame)
                .expect("test frame receiver remains open");
        })
        .expect("spawn scroll worker");
        let input = runtime.input();
        assert!(input.try_hook_scroll(ScrollDelta::wheel_ticks(0.0, 1.0)));
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("first animation frame")
                .phase,
            openlogi_inject::SmoothScrollPhase::Began
        );

        input.cancel_hooks();
        loop {
            let phase = received
                .recv_timeout(Duration::from_secs(1))
                .expect("cancellation frame")
                .phase;
            if phase == openlogi_inject::SmoothScrollPhase::Cancelled {
                break;
            }
            assert_eq!(phase, openlogi_inject::SmoothScrollPhase::Changed);
        }
        runtime.shutdown();
    }
}
