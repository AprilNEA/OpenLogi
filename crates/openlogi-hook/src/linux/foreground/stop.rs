//! Teardown signalling for the foreground observer worker: a stop flag a
//! backend can block on, paired with a wake pipe it can `poll` next to its
//! display connection.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use tracing::debug;

use super::lock_unpoisoned;
use crate::linux::{create_pipe, signal_pipe};

pub(super) struct StopState {
    requested: AtomicBool,
    wait_lock: Mutex<()>,
    changed: Condvar,
}

impl StopState {
    fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            wait_lock: Mutex::new(()),
            changed: Condvar::new(),
        }
    }

    fn request(&self) {
        if self.requested.swap(true, Ordering::AcqRel) {
            return;
        }
        let _guard = lock_unpoisoned(&self.wait_lock);
        self.changed.notify_all();
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    pub(super) fn wait(&self) {
        let guard = lock_unpoisoned(&self.wait_lock);
        drop(
            self.changed
                .wait_while(guard, |()| !self.is_requested())
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }

    fn wait_timeout(&self, timeout: Duration) -> bool {
        if self.is_requested() {
            return true;
        }
        let guard = lock_unpoisoned(&self.wait_lock);
        let (_guard, _) = self
            .changed
            .wait_timeout_while(guard, timeout, |()| !self.is_requested())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.is_requested()
    }
}

pub(super) struct StopControl {
    state: Arc<StopState>,
    wake: OwnedFd,
}

impl StopControl {
    pub(super) fn request(&self) {
        self.state.request();
        signal_pipe(&self.wake);
    }
}

pub(super) struct StopToken {
    state: Arc<StopState>,
    wake: OwnedFd,
}

impl StopToken {
    pub(super) fn is_requested(&self) -> bool {
        self.state.is_requested()
    }

    pub(super) fn wait(&self) {
        self.state.wait();
    }

    pub(super) fn wait_timeout(&self, timeout: Duration) -> bool {
        self.state.wait_timeout(timeout)
    }

    pub(super) fn wake_fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    pub(super) fn state(&self) -> Arc<StopState> {
        Arc::clone(&self.state)
    }
}

pub(super) fn stop_pair() -> io::Result<(StopControl, StopToken)> {
    let (read, write) = create_pipe()?;
    let state = Arc::new(StopState::new());
    Ok((
        StopControl {
            state: Arc::clone(&state),
            wake: write,
        },
        StopToken { state, wake: read },
    ))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PollResult {
    SourceReady,
    StopRequested,
    DeadlineReached,
    Error,
}

/// Wait for a native display fd, observer teardown, or an optional deadline.
/// A deadline is used only for targeted reconnect/drain work; steady-state
/// event delivery passes `None` and blocks indefinitely.
pub(super) fn poll_source_or_stop(
    source_fd: Option<RawFd>,
    stop_fd: RawFd,
    deadline: Option<Instant>,
) -> PollResult {
    let mut fds = [
        libc::pollfd {
            fd: stop_fd,
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        },
        libc::pollfd {
            fd: source_fd.unwrap_or(-1),
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        },
    ];

    loop {
        let timeout = deadline.map_or(-1, |deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                0
            } else {
                i32::try_from(remaining.as_millis().max(1).min(i32::MAX as u128))
                    .unwrap_or(i32::MAX)
            }
        });
        // SAFETY: `fds` is a live two-element pollfd array for the whole call;
        // poll writes only each element's `revents` field.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
        if result > 0 {
            if fds[0].revents != 0 {
                return PollResult::StopRequested;
            }
            if fds[1].revents != 0 {
                return PollResult::SourceReady;
            }
            continue;
        }
        if result == 0 {
            return PollResult::DeadlineReached;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            debug!("frontmost: native event poll failed: {error}");
            return PollResult::Error;
        }
    }
}
