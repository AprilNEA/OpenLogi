//! Agent tracing setup: stderr for foreground runs, a rotating file under the
//! XDG state directory for launchd runs (which discard stderr), and a panic
//! hook so crashes land in the same file.

use std::io;
use std::path::Path;

use tracing::warn;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Rotated log files kept before the oldest is deleted.
const MAX_LOG_FILES: usize = 7;

/// Install the agent's tracing subscriber and panic hook.
///
/// Always logs to stderr (visible in a foreground run), and additionally to a
/// daily-rotated `agent.<date>.log` under [`openlogi_core::paths::state_dir`]
/// — launchd gives the agent's stderr no destination, so without the file a
/// launchd-run agent cannot be diagnosed at all (#336). If the file cannot be
/// opened the agent falls back to stderr only and says so.
pub(crate) fn init() {
    let filter = EnvFilter::try_from_env("OPENLOGI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let (file_layer, file_error) = match file_appender() {
        Ok(appender) => (
            Some(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(appender),
            ),
            None,
        ),
        Err(e) => (None, Some(e)),
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(file_layer)
        .init();
    if let Some(e) = file_error {
        warn!(error = %e, "agent log file unavailable — logging to stderr only");
    }
    route_panics_to_tracing();
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

/// Panics print to stderr, which launchd discards; mirror them into tracing
/// so the log file records why the agent died.
fn route_panics_to_tracing() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("agent panicked: {info}");
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

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
}
