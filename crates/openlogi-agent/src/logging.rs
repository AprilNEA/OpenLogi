//! Agent tracing setup: stderr when the process has one, a rotating file under
//! the XDG state directory (launchd discards the agent's stderr, and a Windows
//! release build has none), and a panic hook so crashes land in the same file.

use std::io::{self, Write as _};
use std::panic::PanicHookInfo;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use openlogi_core::logging::MAX_LOG_FILES;
use tracing::warn;
use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::MakeWriter as _;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime as _, SystemTime};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Keep the file worker alive for the process lifetime. Exec uses acknowledged
/// writes rather than relying on the guard's bounded shutdown wait.
struct WriterState {
    guard: WorkerGuard,
    file: LogFile,
    shared: SharedWriter,
}
static FILE_WRITER: Mutex<Option<WriterState>> = Mutex::new(None);

/// Keep the queue and its completion ledger together. Only this writer enqueues
/// lines; the sink acknowledges each accepted message after its disk write.
#[derive(Clone)]
struct SharedWriter {
    writer: Arc<Mutex<NonBlocking>>,
    progress: Arc<Progress>,
    switching_drops: Arc<AtomicUsize>,
}

#[derive(Default)]
struct Progress {
    state: Mutex<Pending>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct Pending {
    lines: usize,
    failed: bool,
}

struct Acknowledged<W> {
    inner: W,
    progress: Arc<Progress>,
}

impl<W: io::Write> io::Write for Acknowledged<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        // tracing-appender delivers each queued message with write_all.
        let result = self.inner.write_all(bytes);
        let mut pending = self
            .progress
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pending.lines -= 1;
        pending.failed |= result.is_err();
        self.progress.changed.notify_all();
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        if result.is_err() {
            let mut pending = self
                .progress
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            pending.failed = true;
            self.progress.changed.notify_all();
        }
        result
    }
}

impl SharedWriter {
    fn new<W: io::Write + Send + 'static>(
        file: W,
        builder: NonBlockingBuilder,
    ) -> (Self, WorkerGuard) {
        let progress = Arc::new(Progress::default());
        let (writer, guard) =
            builder
                .lossy(true)
                .thread_name("openlogi-log")
                .finish(Acknowledged {
                    inner: file,
                    progress: Arc::clone(&progress),
                });
        (
            Self {
                writer: Arc::new(Mutex::new(writer)),
                progress,
                switching_drops: Arc::new(AtomicUsize::new(0)),
            },
            guard,
        )
    }

    #[cfg(any(test, all(unix, not(target_os = "macos"))))]
    fn prepare_exec(
        &self,
        file: &mut impl io::Write,
        timeout: std::time::Duration,
        replace: impl FnOnce() -> io::Error,
    ) -> io::Error {
        // Hold producers out through flush and exec. Logging callers use try_lock
        // and drop instead of waiting for this potentially slow operation.
        let producer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let pending = self
            .progress
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (pending, _) = self
            .progress
            .changed
            .wait_timeout_while(pending, timeout, |pending| {
                pending.lines != 0 && !pending.failed
            })
            .unwrap_or_else(PoisonError::into_inner);
        if pending.failed {
            return io::Error::other("log worker failed to write or flush an accepted line");
        }
        if pending.lines != 0 {
            return io::Error::new(
                io::ErrorKind::TimedOut,
                "log queue did not drain; keeping the current agent",
            );
        }
        drop(pending);
        let dropped =
            producer.error_counter().dropped_lines() + self.switching_drops.load(Ordering::Relaxed);
        if dropped > 0
            && let Err(error) = writeln!(
                file,
                "WARN dropped {dropped} log messages in total because the queue was full or busy"
            )
        {
            return error;
        }
        if let Err(error) = file.flush() {
            return error;
        }
        // Failed exec leaves this same worker alive and the producer unlocked.
        replace()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Ok(mut writer) = self.writer.try_lock() {
            let mut pending = self
                .progress
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let counter = writer.error_counter();
            let dropped = counter.dropped_lines();
            pending.lines += 1;
            let result = writer.write(bytes);
            if result.is_err() || counter.dropped_lines() != dropped {
                pending.lines -= 1;
            }
            result
        } else {
            self.switching_drops.fetch_add(1, Ordering::Relaxed);
            Ok(bytes.len())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Install the agent's tracing subscriber and panic hook.
///
/// Logs to stderr when there is one (a foreground run), and to a
/// daily-rotated `agent.<date>.log` under [`openlogi_core::paths::state_dir`]
/// — launchd gives the agent's stderr no destination and a Windows release
/// build has no console, so without the file such an agent cannot be diagnosed
/// at all (#336). File lines are written by a thread of their own, so the
/// threads that log never wait on the disk; the queue drops lines when full, and
/// [`flush`] requests a drain for a process about to end. If the file cannot be
/// opened the agent falls back to stderr only and says so.
pub(crate) fn init() {
    let (filter, rejected) = openlogi_core::logging::env_filter(openlogi_core::env::LOG_DEFAULT);
    let file = file_appender().map(|appender| LogFile(Arc::new(appender)));
    let file_layer = file.as_ref().ok().map(|file| {
        let (shared, guard) = SharedWriter::new(file.clone(), NonBlockingBuilder::default());
        *FILE_WRITER.lock().unwrap_or_else(PoisonError::into_inner) = Some(WriterState {
            guard,
            file: file.clone(),
            shared: shared.clone(),
        });
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(move || shared.clone())
    });
    let stderr_layer = openlogi_core::logging::stderr_is_attached()
        .then(|| tracing_subscriber::fmt::layer().with_writer(io::stderr));
    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
    let file = file
        .inspect_err(|e| warn!(error = %e, "agent log file unavailable — logging to stderr only"))
        .ok();
    if let Some(reason) = rejected {
        openlogi_core::logging::warn_rejected_filter(&reason);
    }
    route_panics_to_file(file);
}

/// Request a bounded drain and stop the file writer for process exit, which
/// runs no destructors. Exec instead requires acknowledged completion before
/// replacing the image. Lines logged afterwards reach stderr only.
pub(crate) fn flush() {
    let writer = FILE_WRITER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(WriterState {
        guard,
        mut file,
        shared,
    }) = writer
    {
        let writer = shared.writer.lock().unwrap_or_else(PoisonError::into_inner);
        drop(guard);
        let dropped =
            writer.error_counter().dropped_lines() + shared.switching_drops.load(Ordering::Relaxed);
        if dropped > 0 {
            let _ = writeln!(
                file,
                "WARN dropped {dropped} log lines because the queue was full or busy"
            );
        }
    }
}

/// Replace only after accepted messages are written and the file is flushed.
/// A stalled or failed disk keeps this process and its worker alive for retry.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
pub(crate) fn prepare_exec(replace: impl FnOnce() -> io::Error) -> io::Error {
    let mut state = FILE_WRITER.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(state) = state.as_mut() else {
        return replace();
    };
    state
        .shared
        .prepare_exec(&mut state.file, std::time::Duration::from_secs(5), replace)
}

/// A daily-rotated appender in the app state directory, capped at
/// [`MAX_LOG_FILES`] files.
fn file_appender() -> io::Result<RollingFileAppender> {
    let dir = openlogi_core::paths::state_dir().map_err(io::Error::other)?;
    std::fs::create_dir_all(&dir)?;
    build_appender(&dir)
}

fn build_appender(dir: &Path) -> io::Result<RollingFileAppender> {
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("agent")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(dir)
        .map_err(io::Error::other)
}

/// The agent's log file, shared by the writer thread draining the queue and
/// by the panic hook, which writes past it.
#[derive(Clone)]
struct LogFile(Arc<RollingFileAppender>);

impl LogFile {
    /// Append `info` in the shape of a tracing line.
    fn record_panic(&self, info: &PanicHookInfo<'_>) {
        let mut timestamp = String::new();
        let _ = SystemTime.format_time(&mut Writer::new(&mut timestamp));
        let _ = writeln!(
            self.0.make_writer(),
            "{timestamp} ERROR agent panicked: {info}"
        );
    }
}

impl io::Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.make_writer().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.make_writer().flush()
    }
}

/// Panics print to stderr, which launchd discards and a Windows release build
/// lacks; record them in the log file too, so it says why the agent died.
fn route_panics_to_file(file: Option<LogFile>) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Straight to the file, not through the queue: a panic inside the
        // input-hook callback aborts at its FFI boundary before the writer
        // thread would reach a queued line.
        if let Some(file) = &file {
            file.record_panic(info);
        }
        // Under `panic = "abort"` every panic ends the process; write out what
        // led up to it while there is time. An unwinding panic may yet be
        // caught — the macOS activation observer catches its own on the main
        // thread — and flushing stops the writer for good, so `main` flushes
        // for one that unwinds out of it instead.
        if cfg!(panic = "abort") {
            flush();
        }
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_drains_and_keeps_writer_usable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = LogFile(Arc::new(build_appender(dir.path()).expect("appender")));
        let (mut sender, guard) = SharedWriter::new(file.clone(), NonBlockingBuilder::default());
        *FILE_WRITER.lock().expect("writer state") = Some(WriterState {
            guard,
            file,
            shared: sender.clone(),
        });
        sender.write_all(b"before\n").expect("queue before");
        let mut during_exec = sender.clone();
        let error = prepare_exec(|| {
            during_exec
                .write_all(b"busy\n")
                .expect("logging during exec must not block");
            io::Error::new(io::ErrorKind::NotFound, "simulated failed exec")
        });
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(sender.switching_drops.load(Ordering::Relaxed), 1);
        let content = || {
            std::fs::read_dir(dir.path())
                .expect("list")
                .filter_map(Result::ok)
                .map(|e| std::fs::read_to_string(e.path()).expect("read"))
                .collect::<String>()
        };
        assert_eq!(content(), "before\n");
        sender.write_all(b"after\n").expect("queue after");
        prepare_exec(|| io::Error::other("second failed exec"));
        assert_eq!(
            content(),
            "before\nafter\nWARN dropped 1 log messages in total because the queue was full or busy\n"
        );
        flush();
    }

    struct PausedSink {
        started: Option<std::sync::mpsc::Sender<()>>,
        resume: std::sync::mpsc::Receiver<()>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl io::Write for PausedSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(started) = self.started.take() {
                started.send(()).expect("announce blocked write");
                self.resume
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("release blocked write");
            }
            self.written
                .lock()
                .expect("written")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stalled_full_queue_prevents_exec_and_recovers_without_replacing_worker() {
        let (started, blocked) = std::sync::mpsc::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let (mut sender, guard) = SharedWriter::new(
            PausedSink {
                started: Some(started),
                resume: paused,
                written: Arc::clone(&written),
            },
            NonBlockingBuilder::default().buffered_lines_limit(1),
        );
        sender.write_all(b"first\n").expect("first");
        blocked
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker blocked");
        sender.write_all(b"second\n").expect("second");
        sender
            .write_all(b"dropped\n")
            .expect("lossy queue never blocks");
        assert_eq!(
            sender
                .writer
                .lock()
                .expect("producer")
                .error_counter()
                .dropped_lines(),
            1
        );
        let called = std::cell::Cell::new(false);
        let error = sender.prepare_exec(
            &mut io::sink(),
            std::time::Duration::from_millis(10),
            || {
                called.set(true);
                io::Error::other("must not replace while disk is stalled")
            },
        );
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(!called.get());
        resume.send(()).expect("release worker");
        let error = sender.prepare_exec(&mut io::sink(), std::time::Duration::from_secs(5), || {
            called.set(true);
            io::Error::new(io::ErrorKind::NotFound, "failed exec")
        });
        assert!(called.get());
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(*written.lock().expect("written"), b"first\nsecond\n");
        sender
            .write_all(b"after\n")
            .expect("same worker still usable");
        sender.prepare_exec(&mut io::sink(), std::time::Duration::from_secs(5), || {
            io::Error::other("failed exec")
        });
        assert_eq!(*written.lock().expect("written"), b"first\nsecond\nafter\n");
        drop(guard);
    }

    #[test]
    fn failed_disk_write_prevents_exec() {
        struct FailedSink;
        impl io::Write for FailedSink {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk failed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (mut sender, guard) = SharedWriter::new(FailedSink, NonBlockingBuilder::default());
        sender.write_all(b"line\n").expect("enqueue");
        let error = sender.prepare_exec(&mut io::sink(), std::time::Duration::from_secs(5), || {
            panic!("exec must not run after a failed write")
        });
        assert_eq!(error.kind(), io::ErrorKind::Other);
        drop(guard);
    }

    #[test]
    fn appender_writes_into_the_given_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut appender = build_appender(dir.path()).expect("build appender");
        appender.write_all(b"probe line\n").expect("write");
        appender.flush().expect("flush");
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            files
                .iter()
                .any(|name| name.starts_with("agent") && name.ends_with("log")),
            "no agent log file created: {files:?}"
        );
    }

    #[test]
    fn queued_lines_reach_the_file_through_the_shared_appender() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = LogFile(Arc::new(
            build_appender(dir.path()).expect("build appender"),
        ));
        let (mut writer, guard) = NonBlockingBuilder::default()
            .lossy(false)
            .finish(file.clone());
        writer.write_all(b"queued line\n").expect("queue");
        drop(guard);
        let mut direct = file;
        direct.write_all(b"direct line\n").expect("write");
        let written: String = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|entry| std::fs::read_to_string(entry.path()).expect("read"))
            .collect();
        assert_eq!(written, "queued line\ndirect line\n");
    }
}
