//! The `tracing` setup every OpenLogi process shares.
//!
//! Which variable holds the filter and what a process logs without it are one
//! decision ([`crate::env::LOG`], [`crate::env::LOG_DEFAULT`]); so is turning
//! the two into a filter. A process that spells that step itself can fall back
//! to something else, or to nothing, and nobody would notice until a bug report
//! arrives without logs. Where the GUI and the overlay keep theirs is the same
//! kind of decision ([`init_with_log_file`]). Behind the `logging` feature so
//! the portable core stays free of `tracing-subscriber`.

use std::io;

use tracing_subscriber::EnvFilter;

#[cfg(feature = "fs")]
pub use self::file::init_with_log_file;

/// Log files a process keeps, one per day it logged anything, before it
/// deletes the oldest.
pub const MAX_LOG_FILES: usize = 7;

/// The filter a process installs: [`crate::env::LOG`] when it is set and
/// parses, otherwise `default`.
///
/// A variable that is set but does not parse also comes back as the reason,
/// for [`warn_rejected_filter`] once a subscriber is installed — whoever set
/// it is looking for exactly those lines, and before then nothing would hear
/// the warning.
#[must_use]
pub fn env_filter(default: &str) -> (EnvFilter, Option<String>) {
    let value = match std::env::var(crate::env::LOG) {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return (EnvFilter::new(default), None),
        Err(error) => return (EnvFilter::new(default), Some(error.to_string())),
    };
    match EnvFilter::builder().parse(&value) {
        Ok(filter) => (filter, None),
        Err(error) => (EnvFilter::new(default), Some(format!("{value:?}: {error}"))),
    }
}

/// Report a filter [`env_filter`] rejected, through the subscriber just
/// installed.
pub fn warn_rejected_filter(reason: &str) {
    tracing::warn!(
        variable = crate::env::LOG,
        reason,
        "log filter does not parse — using the default"
    );
}

/// Whether the process has a stderr worth formatting lines for. A Windows
/// release build runs in the GUI subsystem, which starts without standard
/// handles unless its parent redirected them, and every line formatted for
/// the missing stream would be thrown away.
#[cfg(windows)]
#[must_use]
pub fn stderr_is_attached() -> bool {
    use std::os::windows::io::AsRawHandle as _;
    // std reports a missing standard handle as null.
    !io::stderr().as_raw_handle().is_null()
}

/// Whether the process has a stderr worth formatting lines for: off Windows,
/// always.
#[cfg(not(windows))]
#[must_use]
pub fn stderr_is_attached() -> bool {
    true
}

/// Log to stderr through [`env_filter`] — the whole setup for a process run
/// from a terminal (the CLI, the mock agent), which keeps no log file.
///
/// # Panics
///
/// If a global `tracing` subscriber is already installed; call it once, first
/// thing in `main`.
pub fn init_stderr() {
    let (filter, rejected) = env_filter(crate::env::LOG_DEFAULT);
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(filter)
        .init();
    if let Some(reason) = rejected {
        warn_rejected_filter(&reason);
    }
}

#[cfg(feature = "fs")]
mod file {
    use std::ffi::OsStr;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::SystemTime;

    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    use super::{MAX_LOG_FILES, env_filter, stderr_is_attached, warn_rejected_filter};

    /// Log to stderr, when the process has one, and to a daily
    /// `<prefix>.<YYYY-MM-DD>.log` in [`crate::paths::state_dir`] beside the
    /// agent's, keeping [`MAX_LOG_FILES`]; panics are recorded there too.
    ///
    /// For the GUI and the overlay: a Windows release build of either has no
    /// console, so without the file whatever they log goes nowhere. They log
    /// [`crate::env::LOG_DEFAULT_QUIET`] unless [`crate::env::LOG`] says
    /// otherwise. If the file cannot be opened the process logs to stderr only
    /// and says so.
    ///
    /// # Panics
    ///
    /// If a global `tracing` subscriber is already installed; call it once,
    /// first thing in `main`.
    pub fn init_with_log_file(prefix: &'static str) {
        let (filter, rejected) = env_filter(crate::env::LOG_DEFAULT_QUIET);
        let file = crate::paths::state_dir()
            .map_err(io::Error::other)
            .and_then(|dir| DailyFile::open(dir, prefix));
        let (file_layer, file_error) = match file {
            Ok(file) => (
                Some(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(Arc::new(file)),
                ),
                None,
            ),
            Err(error) => (None, Some(error)),
        };
        let stderr_layer =
            stderr_is_attached().then(|| tracing_subscriber::fmt::layer().with_writer(io::stderr));
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .with(file_layer)
            .init();
        if let Some(error) = file_error {
            tracing::warn!(%error, "log file unavailable — logging to stderr only");
        }
        if let Some(reason) = rejected {
            warn_rejected_filter(&reason);
        }
        record_panics(prefix);
    }

    /// Panics print to stderr, which a Windows release build lacks; mirror
    /// them into tracing so the log file records why the process died.
    fn record_panics(process: &'static str) {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            tracing::error!("{process} panicked: {info}");
            default_hook(info);
        }));
    }

    /// A log file per UTC day, `<prefix>.<YYYY-MM-DD>.log`, of which the newest
    /// [`MAX_LOG_FILES`] are kept — the names and retention `tracing-appender`
    /// gives the agent's files, so the state directory lists every process's
    /// logs alike. Writes are synchronous: the GUI and the overlay log too
    /// rarely for a writer thread to pay for itself.
    struct DailyFile {
        dir: PathBuf,
        prefix: &'static str,
        /// The day the open file is for, in days since the Unix epoch, and
        /// the file.
        current: Mutex<(u64, File)>,
    }

    impl DailyFile {
        /// Open today's file in `dir`, creating both as needed.
        fn open(dir: PathBuf, prefix: &'static str) -> io::Result<Self> {
            std::fs::create_dir_all(&dir)?;
            let today = days_since_epoch(SystemTime::now());
            let file = open_day(&dir, prefix, today)?;
            Ok(Self {
                dir,
                prefix,
                current: Mutex::new((today, file)),
            })
        }
    }

    impl io::Write for &DailyFile {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
            let today = days_since_epoch(SystemTime::now());
            if current.0 != today {
                // A new day's file that cannot be opened leaves the old one in
                // use until tomorrow, rather than losing every line meanwhile.
                match open_day(&self.dir, self.prefix, today) {
                    Ok(file) => *current = (today, file),
                    Err(_) => current.0 = today,
                }
            }
            current.1.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.current
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .1
                .flush()
        }
    }

    /// Open (appending) `prefix`'s file for `day`, then delete the ones past
    /// [`MAX_LOG_FILES`].
    fn open_day(dir: &Path, prefix: &str, day: u64) -> io::Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(file_name(prefix, day)))?;
        prune(dir, prefix);
        Ok(file)
    }

    /// Whole days since 1970-01-01 UTC; a clock set before then counts as day 0.
    fn days_since_epoch(now: SystemTime) -> u64 {
        now.duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs() / 86_400)
    }

    /// `<prefix>.<YYYY-MM-DD>.log` for `day` days after 1970-01-01, the date by
    /// Howard Hinnant's `civil_from_days`.
    fn file_name(prefix: &str, day: u64) -> String {
        let z = day + 719_468;
        let era = z / 146_097;
        let doe = z % 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day_of_month = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = era * 400 + yoe + u64::from(month <= 2);
        format!("{prefix}.{year:04}-{month:02}-{day_of_month:02}.log")
    }

    /// Delete all but the newest [`MAX_LOG_FILES`] of `prefix`'s files in
    /// `dir`. Best effort: a file that cannot be listed or removed is kept.
    fn prune(dir: &Path, prefix: &str) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| is_day_file(name, prefix))
            })
            .collect();
        // ISO dates sort as text, so the oldest come first.
        files.sort();
        for stale in files.iter().take(files.len().saturating_sub(MAX_LOG_FILES)) {
            let _ = std::fs::remove_file(stale);
        }
    }

    /// Whether `name` is one of [`file_name`]'s for `prefix`.
    fn is_day_file(name: &str, prefix: &str) -> bool {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('.'))
            .and_then(|rest| rest.strip_suffix(".log"))
            .is_some_and(|date| date.len() == "YYYY-MM-DD".len())
    }

    #[cfg(test)]
    mod tests {
        use std::io::Write as _;

        use super::*;

        #[test]
        fn file_names_carry_the_utc_date() {
            assert_eq!(file_name("gui", 0), "gui.1970-01-01.log");
            assert_eq!(file_name("gui", 11_016), "gui.2000-02-29.log");
            assert_eq!(file_name("overlay", 19_723), "overlay.2024-01-01.log");
        }

        #[test]
        fn writes_land_in_todays_file() {
            let dir = tempfile::tempdir().expect("tempdir");
            let log = DailyFile::open(dir.path().to_path_buf(), "gui").expect("open");
            (&log).write_all(b"probe line\n").expect("write");
            let today = file_name("gui", days_since_epoch(SystemTime::now()));
            let written = std::fs::read_to_string(dir.path().join(today)).expect("read");
            assert_eq!(written, "probe line\n");
        }

        #[test]
        fn pruning_keeps_the_newest_files_of_its_own_prefix() {
            let dir = tempfile::tempdir().expect("tempdir");
            for day in 0..10 {
                File::create(dir.path().join(file_name("gui", day))).expect("create");
            }
            let agent = dir.path().join(file_name("agent", 0));
            File::create(&agent).expect("create");
            prune(dir.path(), "gui");
            let mut left: Vec<_> = std::fs::read_dir(dir.path())
                .expect("read dir")
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("gui."))
                .collect();
            left.sort();
            let newest: Vec<_> = (3..10).map(|day| file_name("gui", day)).collect();
            assert_eq!(left, newest);
            assert!(agent.exists(), "another process's log was pruned");
        }
    }
}
