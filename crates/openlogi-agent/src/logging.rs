//! Agent tracing setup: stderr when the process has one, a rotating file under
//! the XDG state directory (launchd discards the agent's stderr, and a Windows
//! release build has none), and a panic hook so crashes land in the same file.

use std::io::{self, Write as _};
use std::panic::PanicHookInfo;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use openlogi_core::logging::MAX_LOG_FILES;
use tracing::warn;
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::MakeWriter as _;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime as _, SystemTime};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// The file writer thread's guard, held for the life of the process: dropping
/// it is what writes out the lines still queued — see [`flush`].
static FILE_WRITER: Mutex<Option<WorkerGuard>> = Mutex::new(None);

/// Install the agent's tracing subscriber and panic hook.
///
/// Logs to stderr when there is one (a foreground run), and to a
/// daily-rotated `agent.<date>.log` under [`openlogi_core::paths::state_dir`]
/// — launchd gives the agent's stderr no destination and a Windows release
/// build has no console, so without the file such an agent cannot be diagnosed
/// at all (#336). File lines are written by a thread of their own, so the
/// threads that log never wait on the disk; the queue is lossless, and
/// [`flush`] writes it out for a process about to end. If the file cannot be
/// opened the agent falls back to stderr only and says so.
pub(crate) fn init() {
    let (filter, rejected) = openlogi_core::logging::env_filter(openlogi_core::env::LOG_DEFAULT);
    let file = file_appender().map(|appender| LogFile(Arc::new(appender)));
    let file_layer = file.as_ref().ok().map(|file| {
        let (writer, guard) = NonBlockingBuilder::default()
            .lossy(false)
            .thread_name("openlogi-log")
            .finish(file.clone());
        *FILE_WRITER.lock().unwrap_or_else(PoisonError::into_inner) = Some(guard);
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
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

/// Write out the queued log lines and stop the file writer thread, for a
/// process about to end: `process::exit` runs no destructors, so the queue
/// would die with it. Lines logged afterwards reach stderr only.
pub(crate) fn flush() {
    let writer = FILE_WRITER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    drop(writer);
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
