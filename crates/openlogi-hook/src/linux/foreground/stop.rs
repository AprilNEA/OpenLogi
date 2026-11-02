//! Teardown signalling for the foreground observer worker: a stop flag a
//! backend can block on, paired with a wake pipe it can `poll` next to its
//! display connection.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use tracing::debug;

use super::lock_unpoisoned;
use crate::linux::{create_pipe, signal_pipe};

struct StopState {
    requested: AtomicBool,
    wait_lock: Mutex<()>,
    changed: Condvar,
    /// Async observers parked on [`StopToken::stopped`].
    wakers: Mutex<Vec<Waker>>,
}

impl StopState {
    fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            wait_lock: Mutex::new(()),
            changed: Condvar::new(),
            wakers: Mutex::new(Vec::new()),
        }
    }

    fn request(&self) {
        if self.requested.swap(true, Ordering::AcqRel) {
            return;
        }
        {
            let _guard = lock_unpoisoned(&self.wait_lock);
            self.changed.notify_all();
        }
        for waker in lock_unpoisoned(&self.wakers).drain(..) {
            waker.wake();
        }
    }

    fn poll_stopped(&self, cx: &mut Context<'_>) -> Poll<()> {
        if self.is_requested() {
            return Poll::Ready(());
        }
        let mut wakers = lock_unpoisoned(&self.wakers);
        // `request` sets the flag before draining under this lock, so a
        // request racing this poll is either seen here or drains our waker.
        if self.is_requested() {
            return Poll::Ready(());
        }
        if !wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn wait(&self) {
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

    /// Resolves once stop is requested, for observers that wait on async
    /// transports and must not tear their transport down to be woken.
    pub(super) async fn stopped(&self) {
        std::future::poll_fn(|cx| self.state.poll_stopped(cx)).await;
    }

    pub(super) fn wake_fd(&self) -> RawFd {
        self.wake.as_raw_fd()
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

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Wake, Waker};

    use super::stop_pair;

    #[test]
    fn stop_token_wakes_a_parked_async_observer() {
        struct CountingWaker(AtomicUsize);
        impl Wake for CountingWaker {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let (control, token) = stop_pair().expect("stop pipe must open");
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));
        let mut cx = Context::from_waker(&waker);
        let mut stopped = pin!(token.stopped());

        assert!(stopped.as_mut().poll(&mut cx).is_pending());
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);

        control.request();
        assert_eq!(
            counter.0.load(Ordering::SeqCst),
            1,
            "request must wake the parked observer"
        );
        assert!(stopped.as_mut().poll(&mut cx).is_ready());
    }

    #[test]
    fn stop_token_resolves_immediately_once_requested() {
        let (control, token) = stop_pair().expect("stop pipe must open");
        control.request();

        futures_lite::future::block_on(token.stopped());
    }
}
