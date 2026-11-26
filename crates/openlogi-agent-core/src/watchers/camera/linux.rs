//! Linux aggregate camera-use probe.
//!
//! Linux keeps no system-wide record of camera use the way CoreMediaIO or the
//! Windows consent store do. What every capture path shares is its end: a
//! native V4L2 client, a browser, and PipeWire serving a sandboxed app each
//! hold a `/dev/videoN` node open for as long as they capture, and the kernel
//! publishes every process's open files under `/proc/<pid>/fd`. The probe reads
//! those links. It never opens a camera itself, which would take the device
//! from its client and trip this probe's own watches.
//!
//! Only `video*` nodes count. WirePlumber's libcamera monitor holds the
//! `/dev/media*` controller nodes for its whole lifetime, camera or no camera,
//! whereas a capture node stays open only while something streams from it —
//! or for the instant a client enumerates it, which the scan that follows
//! that open finds already closed.
//!
//! Scope: `/proc/<pid>/fd` is readable only for processes this agent could
//! ptrace — its own user's, which is where a desktop session's camera clients
//! and its PipeWire daemon run. A process of another account, or one that made
//! itself non-dumpable, is outside the scan. That is a fixed property of the
//! host rather than a transient failure, so such a process is skipped instead
//! of reported: reporting it would hold the watcher in its error path on every
//! ordinary system, where root-owned processes always exist.
//!
//! Cost: walking every process's fd table is tens of thousands of `readlink`s
//! on a desktop with a browser open — too much to repeat every second. An
//! inotify watch on each node reports every open and close of it, by any
//! process, so the probe rescans only when one arrives, and otherwise every
//! [`RECONCILE_EVERY`] samples: a queue can overflow, and an event is a cue to
//! look again, not the answer.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::inotify::{self, CreateFlags, WatchFlags};
use rustix::io::Errno;
use tracing::warn;

use super::status_of;

/// Where the kernel publishes each process's open files.
const PROC: &str = "/proc";
/// Where udev creates the V4L2 nodes.
const DEV: &str = "/dev";
/// The name every V4L2 video node carries, followed by its minor index.
const NODE_PREFIX: &str = "video";
/// Samples answered from the last scan before the probe rescans regardless of
/// what the watches report. At the watcher's one-second period, a missed event
/// leaves a light wrong for at most half a minute.
const RECONCILE_EVERY: u32 = 30;

/// The Linux provider: the last scan's answer, and the watches that say when
/// it stopped being current.
pub(super) struct Probe {
    /// `None` when inotify could not be set up, which leaves every sample to
    /// scan.
    wake: Option<Wake>,
    schedule: Schedule,
}

impl Probe {
    /// Set up the node watches. A host where they cannot be set up still gets
    /// a working probe, one that scans on every sample.
    pub(super) fn new() -> Self {
        let wake = Wake::new(PathBuf::from(DEV))
            .inspect_err(|error| {
                warn!(error = %error, "camera node watches unavailable — scanning every sample");
            })
            .ok();
        Self {
            wake,
            schedule: Schedule::default(),
        }
    }

    /// Report whether any process this agent can inspect holds a V4L2 node.
    /// The error is the `errno` that kept the scan from an answer.
    pub(super) fn sample(&mut self) -> Result<bool, i32> {
        // Drain before scanning: an open that lands after the drain is still
        // queued for the next sample, so a scan can never consume a change
        // whose cue it then discards.
        let cue = self.cue();
        if let Some(held) = self.schedule.cached(cue) {
            return Ok(held);
        }
        let scan = scan(Path::new(PROC)).map_err(|error| status_of(&error));
        self.schedule.record(scan)
    }

    fn cue(&mut self) -> Cue {
        let Some(wake) = &mut self.wake else {
            return Cue::Unwatched;
        };
        wake.drain().unwrap_or_else(|error| {
            warn!(error = %error, "camera node watches failed — scanning every sample");
            self.wake = None;
            Cue::Unwatched
        })
    }
}

/// What the watches reported since the previous sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cue {
    /// Every node is watched, and none was opened, closed, added or removed.
    Quiet,
    /// A node was opened, closed, added or removed, or the queue overflowed.
    Changed,
    /// Some node carries no watch, so silence proves nothing.
    Unwatched,
}

/// When the last scan's answer still stands.
#[derive(Default)]
struct Schedule {
    /// The last scan's answer: `None` before the first scan and after a failed
    /// one, so a failure is retried on the next sample rather than repeated.
    last: Option<bool>,
    /// Samples answered from `last` since it was scanned.
    reused: u32,
}

impl Schedule {
    /// The last scan's answer when this sample may reuse it, or `None` when
    /// the sample must scan.
    fn cached(&mut self, cue: Cue) -> Option<bool> {
        let last = self.last?;
        if cue != Cue::Quiet || self.reused + 1 >= RECONCILE_EVERY {
            return None;
        }
        self.reused += 1;
        Some(last)
    }

    /// Adopt a scan's result as the answer later samples reuse.
    fn record(&mut self, scan: Result<bool, i32>) -> Result<bool, i32> {
        self.last = scan.ok();
        self.reused = 0;
        scan
    }
}

/// inotify watches on the device directory, for nodes that appear, and on
/// every V4L2 node in it, for opens and closes.
struct Wake {
    fd: OwnedFd,
    dev: PathBuf,
    /// Whether the last refresh watched every node. Watching a node needs read
    /// access to it, which the agent may lack — another seat's camera — and
    /// then the probe scans every sample rather than trust that node's silence.
    complete: bool,
}

impl Wake {
    fn new(dev: PathBuf) -> io::Result<Self> {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        inotify::add_watch(
            &fd,
            &dev,
            WatchFlags::CREATE | WatchFlags::MOVED_TO | WatchFlags::ONLYDIR,
        )?;
        let mut wake = Self {
            fd,
            dev,
            complete: false,
        };
        wake.watch_nodes()?;
        Ok(wake)
    }

    /// Watch every V4L2 node now in the device directory. Watching a node
    /// again is idempotent — inotify hands back its existing watch — so this
    /// runs whenever a node appears, and on every drain while a node is
    /// unwatched.
    fn watch_nodes(&mut self) -> io::Result<()> {
        let mut complete = true;
        for entry in fs::read_dir(&self.dev)? {
            let entry = entry?;
            if !is_v4l2_node(&entry.file_name()) {
                continue;
            }
            match inotify::add_watch(&self.fd, entry.path(), WatchFlags::OPEN | WatchFlags::CLOSE) {
                // A node removed since the listing needs no watch.
                Ok(_) | Err(Errno::NOENT) => {}
                Err(_) => complete = false,
            }
        }
        self.complete = complete;
        Ok(())
    }

    /// Consume every queued event and report what they amount to.
    fn drain(&mut self) -> io::Result<Cue> {
        let mut changed = false;
        let mut appeared = false;
        {
            let mut buffer = [MaybeUninit::<u8>::uninit(); 4096];
            let mut events = inotify::Reader::new(&self.fd, &mut buffer);
            loop {
                match events.next() {
                    Ok(event) => match event.file_name() {
                        // The directory's watch, which names the entry: a
                        // device node appeared. Only a V4L2 one matters.
                        Some(name) => {
                            if is_v4l2_node(OsStr::from_bytes(name.to_bytes())) {
                                appeared = true;
                                changed = true;
                            }
                        }
                        // A node's own watch — an open, a close, the node's
                        // removal — or the queue overflowing, which names
                        // nothing and leaves every node's state unknown.
                        None => changed = true,
                    },
                    Err(Errno::AGAIN) => break,
                    Err(Errno::INTR) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        if appeared || !self.complete {
            self.watch_nodes()?;
        }
        Ok(if !self.complete {
            Cue::Unwatched
        } else if changed {
            Cue::Changed
        } else {
            Cue::Quiet
        })
    }
}

/// Whether any process under `proc_root` holds a V4L2 node.
///
/// A held node anywhere answers the scan, so a process that could not be read
/// never outweighs another's evidence. Without one, the first such failure
/// answers instead of "none": a process whose files could not be read is not
/// one known to hold nothing.
fn scan(proc_root: &Path) -> io::Result<bool> {
    let mut failure = None;
    for entry in fs::read_dir(proc_root)? {
        let entry = entry?;
        if !is_decimal(&entry.file_name()) {
            continue;
        }
        match holds_v4l2_node(&entry.path().join("fd")) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    failure.map_or(Ok(false), Err)
}

/// Whether the process whose descriptor table is `fd_dir` holds a V4L2 node. A
/// process outside the scan's view holds nothing the scan can see.
fn holds_v4l2_node(fd_dir: &Path) -> io::Result<bool> {
    let fds = match fs::read_dir(fd_dir) {
        Ok(fds) => fds,
        Err(error) if out_of_view(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    for fd in fds {
        let fd = match fd {
            Ok(fd) => fd,
            Err(error) if out_of_view(&error) => return Ok(false),
            Err(error) => return Err(error),
        };
        match fs::read_link(fd.path()) {
            Ok(target) if is_v4l2_path(&target) => return Ok(true),
            // The descriptor closed between the listing and the read.
            Ok(_) => {}
            Err(error) if out_of_view(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// Whether an error puts a process or descriptor outside the scan's view
/// rather than making it unreadable: it went away (`ENOENT`, or `ESRCH` while
/// the process is torn down), or it is not this agent's to inspect (`EACCES`,
/// `EPERM`).
fn out_of_view(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    ) || error.raw_os_error() == Some(Errno::SRCH.raw_os_error())
}

/// Whether a descriptor's link target is a V4L2 video node. The kernel appends
/// ` (deleted)` to a node removed while held — an unplugged camera, which
/// nothing is capturing from any more — so that target does not count.
fn is_v4l2_path(target: &Path) -> bool {
    target.parent() == Some(Path::new(DEV)) && target.file_name().is_some_and(is_v4l2_node)
}

/// Whether a device-directory entry names a V4L2 video node: `video` and its
/// minor index, never `v4l-subdev*` or the `media*` controllers.
fn is_v4l2_node(name: &OsStr) -> bool {
    name.as_bytes()
        .strip_prefix(NODE_PREFIX.as_bytes())
        .is_some_and(|index| is_decimal(OsStr::from_bytes(index)))
}

/// Whether a name is a non-empty run of ASCII digits — a `/proc` process
/// directory, or a node's minor index.
fn is_decimal(name: &OsStr) -> bool {
    !name.is_empty() && name.as_bytes().iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use super::{Cue, RECONCILE_EVERY, Schedule, Wake, is_v4l2_path, scan};

    #[test]
    fn only_video_nodes_count_as_cameras() {
        for held in ["/dev/video0", "/dev/video12"] {
            assert!(is_v4l2_path(Path::new(held)), "{held} is a capture node");
        }
        for other in [
            // WirePlumber holds these for its whole lifetime.
            "/dev/media0",
            "/dev/v4l-subdev0",
            "/dev/video",
            "/dev/videox",
            // A camera unplugged while held.
            "/dev/video0 (deleted)",
            "/dev/v4l/by-id/usb-046d_Logitech_BRIO-video-index0",
            "/tmp/video0",
            "/dev/null",
        ] {
            assert!(
                !is_v4l2_path(Path::new(other)),
                "{other} is not a capture node"
            );
        }
    }

    #[test]
    fn the_first_sample_scans_and_quiet_samples_reuse_it() {
        let mut schedule = Schedule::default();
        assert_eq!(schedule.cached(Cue::Quiet), None);
        assert_eq!(schedule.record(Ok(true)), Ok(true));
        assert_eq!(schedule.cached(Cue::Quiet), Some(true));
    }

    #[test]
    fn a_cue_or_an_unwatched_node_forces_a_scan() {
        let mut schedule = Schedule::default();
        schedule.record(Ok(false)).unwrap();
        assert_eq!(schedule.cached(Cue::Changed), None);
        schedule.record(Ok(false)).unwrap();
        assert_eq!(schedule.cached(Cue::Unwatched), None);
    }

    #[test]
    fn a_failed_scan_is_retried_rather_than_repeated() {
        let mut schedule = Schedule::default();
        schedule.record(Ok(true)).unwrap();
        assert_eq!(schedule.record(Err(5)), Err(5));
        // Even with quiet watches: a failure has no answer to reuse.
        assert_eq!(schedule.cached(Cue::Quiet), None);
    }

    #[test]
    fn quiet_watches_still_reconcile_on_a_bounded_period() {
        let mut schedule = Schedule::default();
        schedule.record(Ok(false)).unwrap();
        let reused = (0..RECONCILE_EVERY)
            .take_while(|_| schedule.cached(Cue::Quiet).is_some())
            .count();
        // One scan plus the reused samples make up one period.
        assert_eq!(reused + 1, usize::try_from(RECONCILE_EVERY).unwrap());
        assert_eq!(schedule.cached(Cue::Quiet), None);
        schedule.record(Ok(false)).unwrap();
        assert_eq!(schedule.cached(Cue::Quiet), Some(false));
    }

    /// A stand-in for `/proc`: one directory per pid, each holding an `fd`
    /// directory of links named the way the kernel names them.
    fn process(root: &Path, pid: &str, targets: &[&str]) {
        let fd_dir = root.join(pid).join("fd");
        fs::create_dir_all(&fd_dir).unwrap();
        for (fd, target) in targets.iter().enumerate() {
            symlink(target, fd_dir.join(fd.to_string())).unwrap();
        }
    }

    #[test]
    fn a_process_holding_a_video_node_is_found() {
        let proc_root = tempfile::tempdir().unwrap();
        process(proc_root.path(), "100", &["/dev/null", "/dev/media0"]);
        assert!(!scan(proc_root.path()).unwrap());
        process(proc_root.path(), "200", &["socket:[42]", "/dev/video2"]);
        assert!(scan(proc_root.path()).unwrap());
    }

    #[test]
    fn entries_that_are_not_processes_are_ignored() {
        let proc_root = tempfile::tempdir().unwrap();
        // `/proc/sys` and friends have no descriptor table; an `fd` directory
        // under one is not a process's.
        process(proc_root.path(), "sys", &["/dev/video0"]);
        assert!(!scan(proc_root.path()).unwrap());
    }

    #[test]
    fn a_process_that_exited_holds_nothing() {
        let proc_root = tempfile::tempdir().unwrap();
        // The pid directory without its descriptor table: the process is gone
        // between the listing and the read.
        fs::create_dir(proc_root.path().join("300")).unwrap();
        assert!(!scan(proc_root.path()).unwrap());
    }

    #[test]
    fn an_unreadable_process_does_not_read_as_an_idle_one() {
        let proc_root = tempfile::tempdir().unwrap();
        process(proc_root.path(), "100", &["/dev/null"]);
        // A descriptor table that cannot be listed for any reason other than
        // the process being gone or not ours.
        fs::create_dir(proc_root.path().join("400")).unwrap();
        File::create(proc_root.path().join("400").join("fd")).unwrap();
        scan(proc_root.path()).expect_err("an unreadable process leaves the answer unknown");
        // Unless another process establishes use, which nothing contradicts.
        process(proc_root.path(), "500", &["/dev/video0"]);
        assert!(scan(proc_root.path()).unwrap());
    }

    #[test]
    fn a_missing_proc_is_a_failure_not_silence() {
        let proc_root = tempfile::tempdir().unwrap();
        scan(&proc_root.path().join("absent")).expect_err("no process table is no answer");
    }

    #[test]
    fn watches_report_opens_closes_and_new_nodes() {
        let dev = tempfile::tempdir().unwrap();
        let node = dev.path().join("video0");
        File::create(&node).unwrap();
        let mut wake = Wake::new(dev.path().to_path_buf()).unwrap();
        assert_eq!(wake.drain().unwrap(), Cue::Quiet);

        drop(File::open(&node).unwrap());
        assert_eq!(wake.drain().unwrap(), Cue::Changed);
        assert_eq!(wake.drain().unwrap(), Cue::Quiet);

        // Another device node appearing is not a camera.
        File::create(dev.path().join("hidraw3")).unwrap();
        assert_eq!(wake.drain().unwrap(), Cue::Quiet);

        // A camera plugged in after startup is watched from then on.
        let plugged = dev.path().join("video4");
        File::create(&plugged).unwrap();
        assert_eq!(wake.drain().unwrap(), Cue::Changed);
        drop(File::open(&plugged).unwrap());
        assert_eq!(wake.drain().unwrap(), Cue::Changed);
        assert_eq!(wake.drain().unwrap(), Cue::Quiet);
    }
}
