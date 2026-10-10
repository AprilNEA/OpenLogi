//! The `tracing` setup every OpenLogi process shares.
//!
//! Which variable holds the filter and what a process logs without it are one
//! decision ([`crate::env::LOG`], [`crate::env::LOG_DEFAULT`]); so is turning
//! the two into a filter. A process that spells that step itself can fall back
//! to something else, or to nothing, and nobody would notice until a bug report
//! arrives without logs. Behind the `logging` feature so the portable core
//! stays free of `tracing-subscriber`.

use tracing_subscriber::EnvFilter;

/// The filter every process installs: [`crate::env::LOG`] when it is set and
/// parses, otherwise [`crate::env::LOG_DEFAULT`].
#[must_use]
pub fn env_filter() -> EnvFilter {
    EnvFilter::try_from_env(crate::env::LOG)
        .unwrap_or_else(|_| EnvFilter::new(crate::env::LOG_DEFAULT))
}

/// Log to stderr through [`env_filter`] — the whole setup for a process that,
/// unlike the agent, keeps no log file of its own.
///
/// # Panics
///
/// If a global `tracing` subscriber is already installed; call it once, first
/// thing in `main`.
pub fn init_stderr() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(env_filter())
        .init();
}
