//! The thread that owns the per-mouse device threads and hooks a mouse again
//! when its event node is re-created.
//!
//! A mouse that powers off, drops its Bluetooth link, or sleeps through a
//! suspend loses its event node. Its device thread sees the hang-up and
//! exits, and the node that comes back — often at the same path — belongs to
//! a new device that nothing has grabbed (#1863). The supervisor rescans
//! `/dev/input` and starts a device thread for every hookable mouse that has
//! none.
//!
//! Three things wake it: an inotify event on `/dev/input`, a device thread
//! exiting, and [`RESCAN_HEARTBEAT`]. inotify is freshness, not a
//! completeness proof — the watch can fail to install — so the heartbeat
//! stays as the backstop.

use std::fs;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use evdev::Device;
use tracing::{error, info, warn};

use crate::{EventDisposition, HookError, HookEvent};

/// How long a re-created node can wait to be hooked when neither inotify nor
/// a device-thread exit announces it.
const RESCAN_HEARTBEAT: Duration = Duration::from_secs(30);

type Callback = Arc<dyn Fn(HookEvent) -> EventDisposition + Send + Sync>;

/// The handle [`super::Backend`] keeps while the hook runs.
pub(super) struct Running {
    stop: Arc<AtomicBool>,
    /// Write end of the supervisor's wake pipe.
    wake_tx: Arc<OwnedFd>,
    thread: thread::JoinHandle<()>,
}

impl Running {
    /// Whether the supervisor thread has not exited.
    pub(super) fn is_running(&self) -> bool {
        !self.thread.is_finished()
    }

    /// Stop the supervisor, which stops and joins every device thread.
    pub(super) fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        super::signal_pipe(&self.wake_tx);
        if let Err(e) = self.thread.join() {
            error!("hook supervisor panicked on shutdown: {e:?}");
        }
    }
}

/// Hook every mouse present now, then supervise.
///
/// Returns [`HookError::NoDeviceFound`] when no hookable mouse is present, as
/// before supervision existed: on Linux that is also how a missing `input`
/// group shows up, and callers rely on it to fall back. The agent starts the
/// hook again when an inventory snapshot later reports a device.
pub(super) fn start(cb: Callback) -> Result<Running, HookError> {
    // Watch before the first scan, so a node created in between still wakes
    // the supervisor.
    let watch = InputDirWatch::open();
    let devices = super::find_mouse_devices();
    if devices.is_empty() {
        return Err(HookError::NoDeviceFound);
    }
    let (wake_rx, wake_tx) = super::create_pipe().map_err(HookError::Linux)?;
    let stop = Arc::new(AtomicBool::new(false));
    let mut supervisor = Supervisor {
        cb,
        stop: Arc::clone(&stop),
        wake_rx,
        wake_tx: Arc::new(wake_tx),
        watch,
        workers: Vec::with_capacity(devices.len()),
    };
    for (path, device) in devices {
        // On error, dropping `supervisor` stops the workers already started.
        supervisor.spawn(path, device).map_err(HookError::Linux)?;
    }
    let wake_tx = Arc::clone(&supervisor.wake_tx);
    let thread = thread::Builder::new()
        .name("openlogi-hook:supervisor".to_owned())
        .spawn(move || supervisor.run())
        .map_err(HookError::Linux)?;
    Ok(Running {
        stop,
        wake_tx,
        thread,
    })
}

/// Whether a device thread is still reading its node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerState {
    Running,
    Exited,
}

/// Whether a worker's claim on its path is over, so the path may be hooked
/// again.
///
/// A running worker keeps its claim even when its node was re-created: it
/// exits on the hang-up and wakes the supervisor. An exited worker whose node
/// is unchanged keeps its claim too. That device failed its grab or broke its
/// virtual device, and retrying it on every rescan would only repeat the
/// failure.
fn claim_is_stale(claimed_node: u64, state: WorkerState, current_node: Option<u64>) -> bool {
    state == WorkerState::Exited && current_node != Some(claimed_node)
}

/// The inode of the node at `path`, or `None` when the node is gone. devtmpfs
/// gives a re-created node a new inode even when it reuses the path.
fn node_inode(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|metadata| metadata.ino())
}

/// The inode of the node `device` was opened from.
///
/// Read from the open descriptor, not the path: the node at the path can
/// already be a re-created one, and recording its inode would make the claim
/// outlive the node it was taken for.
fn opened_inode(device: &Device) -> io::Result<u64> {
    let fd = device.as_fd().try_clone_to_owned()?;
    Ok(fs::File::from(fd).metadata()?.ino())
}

/// One device thread and the event node it was started for.
struct Worker {
    path: PathBuf,
    /// The inode of the node the worker's device was opened from.
    node: u64,
    /// Set by the worker's [`ExitNotice`] before it wakes the supervisor.
    exited: Arc<AtomicBool>,
    stop_tx: OwnedFd,
    thread: thread::JoinHandle<()>,
}

impl Worker {
    /// `exited` is the signal: the wake that follows it can arrive before
    /// the thread finishes, so `is_finished` alone could read as still
    /// running for that one rescan. `is_finished` stays as the fallback.
    fn state(&self) -> WorkerState {
        if self.exited.load(Ordering::Acquire) || self.thread.is_finished() {
            WorkerState::Exited
        } else {
            WorkerState::Running
        }
    }
}

/// Marks a device thread exited and wakes the supervisor when the thread's
/// work ends, also when it unwinds from a panic.
struct ExitNotice {
    exited: Arc<AtomicBool>,
    wake: Arc<OwnedFd>,
}

impl Drop for ExitNotice {
    fn drop(&mut self) {
        self.exited.store(true, Ordering::Release);
        // A node re-created at this path waits behind this worker's claim, so
        // let the supervisor reap it now rather than at the next heartbeat.
        super::signal_pipe(&self.wake);
    }
}

fn join(worker: Worker) {
    if let Err(e) = worker.thread.join() {
        error!("hook thread for {} panicked: {e:?}", worker.path.display());
    }
}

/// Why [`Supervisor::wait`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wake {
    Rescan,
    Stop,
}

struct Supervisor {
    cb: Callback,
    stop: Arc<AtomicBool>,
    wake_rx: OwnedFd,
    /// Shared with [`Running`] and every device thread, which writes to it
    /// when it exits.
    wake_tx: Arc<OwnedFd>,
    watch: Option<InputDirWatch>,
    workers: Vec<Worker>,
}

impl Supervisor {
    fn run(mut self) {
        while self.wait() == Wake::Rescan && !self.stop.load(Ordering::Relaxed) {
            self.reconcile();
        }
        // Dropping `self` stops the device threads.
    }

    /// Block until something may have changed, and drain what fired.
    fn wait(&mut self) -> Wake {
        const ERR_FLAGS: libc::c_short = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
        let watch_fd = self.watch.as_ref().map_or(-1, |watch| watch.0.as_raw_fd());
        let mut fds = [
            libc::pollfd {
                fd: self.wake_rx.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: watch_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let timeout = i32::try_from(RESCAN_HEARTBEAT.as_millis()).unwrap_or(i32::MAX);
        loop {
            // SAFETY: `fds` is a valid two-element pollfd array; poll ignores
            // the entry whose fd is negative.
            let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
            if ret >= 0 {
                break;
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                error!("hook supervisor poll() failed: {err}");
                // Bound the retry rate; the caller still checks the stop flag.
                thread::sleep(Duration::from_secs(1));
                return Wake::Rescan;
            }
        }
        if fds[0].revents & ERR_FLAGS != 0 {
            // Every write end is gone, so nothing can stop us later either.
            return Wake::Stop;
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 64];
            // SAFETY: `buf` is valid for `buf.len()` bytes of writes, and poll
            // reported the pipe readable, so the read does not block.
            unsafe { libc::read(self.wake_rx.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        }
        if fds[1].revents & ERR_FLAGS != 0 {
            warn!("the /dev/input watch failed — rescanning only on the heartbeat");
            self.watch = None;
        } else if fds[1].revents & libc::POLLIN != 0
            && let Some(watch) = &self.watch
        {
            watch.drain();
        }
        Wake::Rescan
    }

    /// Join the workers whose claim is over, then hook every hookable mouse
    /// that has no worker.
    fn reconcile(&mut self) {
        let stale: Vec<Worker> = self
            .workers
            .extract_if(.., |worker| {
                claim_is_stale(worker.node, worker.state(), node_inode(&worker.path))
            })
            .collect();
        for worker in stale {
            join(worker);
        }
        for (path, device) in super::find_mouse_devices() {
            if self.workers.iter().any(|worker| worker.path == path) {
                continue;
            }
            let shown = format!(
                "{} ({})",
                path.display(),
                device.name().unwrap_or("unnamed")
            );
            info!("hooking {shown}, which appeared after the hook started");
            if let Err(e) = self.spawn(path, device) {
                warn!("failed to hook {shown}: {e} — retrying at the next rescan");
            }
        }
    }

    /// Start a device thread for `device` and record its claim on `path`.
    fn spawn(&mut self, path: PathBuf, device: Device) -> io::Result<()> {
        let node = opened_inode(&device)?;
        let virtual_device = super::build_virtual_device(&device)?;
        let (stop_rx, stop_tx) = super::create_pipe()?;
        let cb = Arc::clone(&self.cb);
        let stop = Arc::clone(&self.stop);
        let exited = Arc::new(AtomicBool::new(false));
        let notice = ExitNotice {
            exited: Arc::clone(&exited),
            wake: Arc::clone(&self.wake_tx),
        };
        let thread_path = path.clone();
        let thread = thread::Builder::new()
            .name(format!("openlogi-hook:{}", path.display()))
            .spawn(move || {
                // Dropped after `device_thread` returns, so after the device
                // and its grab are released.
                let _notice = notice;
                super::device_thread(thread_path, device, virtual_device, cb, stop, stop_rx);
            })?;
        self.workers.push(Worker {
            path,
            node,
            exited,
            stop_tx,
            thread,
        });
        Ok(())
    }
}

impl Drop for Supervisor {
    /// Stop and join every device thread. Each one releases its grab when it
    /// drops its `Device`.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for worker in &self.workers {
            super::signal_pipe(&worker.stop_tx);
        }
        for worker in self.workers.drain(..) {
            join(worker);
        }
    }
}

/// An inotify watch on `/dev/input` for new event nodes and for permission
/// changes on them.
struct InputDirWatch(OwnedFd);

impl InputDirWatch {
    /// Watch `/dev/input`, or return `None` (logged) when inotify is
    /// unavailable. The heartbeat still rescans without it.
    fn open() -> Option<Self> {
        // SAFETY: inotify_init1 takes only flags and returns a new descriptor
        // or -1.
        let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if raw < 0 {
            warn!(
                "inotify unavailable ({}) — a reconnected mouse is hooked again only on the {}s rescan",
                io::Error::last_os_error(),
                RESCAN_HEARTBEAT.as_secs()
            );
            return None;
        }
        // SAFETY: inotify_init1 succeeded, so `raw` is a valid descriptor that
        // nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // IN_ATTRIB matters as much as IN_CREATE: udev grants access to a new
        // node after devtmpfs creates it, and until then the scan cannot open
        // it.
        let mask = libc::IN_CREATE | libc::IN_ATTRIB | libc::IN_ONLYDIR;
        // SAFETY: `fd` is a valid inotify descriptor and the path is a
        // NUL-terminated literal.
        if unsafe { libc::inotify_add_watch(fd.as_raw_fd(), c"/dev/input".as_ptr(), mask) } < 0 {
            warn!(
                "cannot watch /dev/input ({}) — a reconnected mouse is hooked again only on the {}s rescan",
                io::Error::last_os_error(),
                RESCAN_HEARTBEAT.as_secs()
            );
            return None;
        }
        Some(Self(fd))
    }

    /// Discard the queued events: every wake rescans all of `/dev/input`.
    fn drain(&self) {
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: `buf` is valid for `buf.len()` bytes of writes, and the
            // descriptor is non-blocking.
            let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 || (n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted) {
                continue;
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    const NODE: u64 = 41;
    const RECREATED: u64 = 42;

    /// The notice marks the worker exited before it wakes the supervisor, on
    /// a normal return and on a panic, so the rescan the wake triggers never
    /// sees the worker as still running.
    #[test]
    fn exit_notice_marks_exited_before_waking() {
        for panics in [false, true] {
            let (wake_rx, wake_tx) = super::super::create_pipe().expect("wake pipe");
            let exited = Arc::new(AtomicBool::new(false));
            let notice = ExitNotice {
                exited: Arc::clone(&exited),
                wake: Arc::new(wake_tx),
            };
            let result = std::panic::catch_unwind(move || {
                let _notice = notice;
                assert!(!panics, "device thread panicked");
            });
            assert_eq!(result.is_err(), panics);
            assert!(exited.load(Ordering::Acquire), "panics={panics}");
            let mut buf = [0u8; 8];
            let woken = fs::File::from(wake_rx)
                .read(&mut buf)
                .expect("read wake pipe");
            assert!(woken > 0, "no wake with panics={panics}");
        }
    }

    /// Every combination of worker state and what is at the path now.
    #[test]
    fn claim_is_stale_truth_table() {
        let cases = [
            // A running worker keeps its node, whatever is at the path.
            (WorkerState::Running, Some(NODE), false),
            (WorkerState::Running, Some(RECREATED), false),
            (WorkerState::Running, None, false),
            // An exited worker on an unchanged node: a failed grab, not
            // retried on every rescan.
            (WorkerState::Exited, Some(NODE), false),
            // The node was re-created at the same path: hook it again.
            (WorkerState::Exited, Some(RECREATED), true),
            // The node is gone: drop the claim.
            (WorkerState::Exited, None, true),
        ];
        for (state, current, stale) in cases {
            assert_eq!(
                claim_is_stale(NODE, state, current),
                stale,
                "{state:?} with {current:?} at the path"
            );
        }
    }
}
